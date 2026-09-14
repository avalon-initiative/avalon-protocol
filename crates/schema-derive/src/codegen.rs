//! The actual codegen logic, kept separate from `lib.rs`'s thin
//! `#[proc_macro_derive]` entry point specifically so it's unit-testable
//! with plain `cargo test` — a proc-macro crate's own `#[proc_macro_*]`
//! functions can only be exercised by actually compiling a second crate
//! that uses them (`trybuild` or similar), which is real infrastructure
//! this ticket doesn't need: everything interesting here (Rust type ->
//! proto type mapping, field numbering, attribute parsing) is a pure
//! function over `syn` types, so it's tested directly instead.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, Type};

/// One field's resolved shape: its wire name (the struct field's own
/// identifier — see [`derive_avalon_schema`]'s doc comment on why this,
/// not a renamed one), its proto type, and whether it's `repeated`
/// (`Vec<T>`) or `optional` (`Option<T>`, proto3's explicit-presence
/// modifier). A field is never both.
struct FieldShape {
    name: String,
    proto_type: &'static str,
    repeated: bool,
    optional: bool,
    /// `Some(visibility)` only when the field carries an explicit
    /// `#[avalon(visibility = "...")]` attribute — see this module's own
    /// "only emit overrides" reasoning in [`build_field_visibility`].
    visibility_override: Option<String>,
}

/// Maps a Rust scalar type to its proto3 equivalent. Deliberately narrow:
/// nested messages, maps, bytes, and enums aren't supported in this first
/// pass (see this crate's own README/doc comment for the reasoning) — an
/// unsupported type is a compile error here, not a silently-wrong
/// generated schema.
fn proto_scalar_type(ty: &Type) -> Option<&'static str> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    match segment.ident.to_string().as_str() {
        "String" => Some("string"),
        "bool" => Some("bool"),
        "i32" => Some("int32"),
        "i64" => Some("int64"),
        "u32" => Some("uint32"),
        "u64" => Some("uint64"),
        "f32" => Some("float"),
        "f64" => Some("double"),
        _ => None,
    }
}

/// Unwraps `Option<T>`/`Vec<T>` one level, returning the inner type and
/// which wrapper (if either) was present. Only ever unwraps once — a
/// `Vec<Option<T>>`/`Option<Vec<T>>` is intentionally unsupported (proto3
/// has no direct equivalent for "an optional repeated field" or "a
/// repeated field of optionals" that this simple mapping could produce
/// without guessing at intent).
fn unwrap_container(ty: &Type) -> (&Type, bool, bool) {
    let Type::Path(type_path) = ty else {
        return (ty, false, false);
    };
    let Some(segment) = type_path.path.segments.last() else {
        return (ty, false, false);
    };
    let ident = segment.ident.to_string();
    if ident != "Option" && ident != "Vec" {
        return (ty, false, false);
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return (ty, false, false);
    };
    let Some(syn::GenericArgument::Type(inner)) = args.args.first() else {
        return (ty, false, false);
    };
    match ident.as_str() {
        "Option" => (inner, false, true),
        "Vec" => (inner, true, false),
        _ => unreachable!(),
    }
}

/// The fixed vocabulary `#[avalon(visibility = "...")]`/
/// `#[avalon(default_visibility = "...")]` accept — matches
/// `crates/server/src/integrator_schemas.rs::is_valid_visibility` exactly;
/// a mismatch here would only be caught at publish time, over the network,
/// instead of at compile time, which defeats a real chunk of the point of
/// generating this at all.
fn is_valid_visibility(value: &str) -> bool {
    matches!(value, "public" | "private")
}

/// Reads a single `#[avalon(key = "value")]` attribute's `value` for the
/// given `key`, if present. Returns `Ok(None)` when the attribute simply
/// isn't there (not an error — every `#[avalon(...)]` attribute is
/// optional), `Err` for a malformed one (wrong key, non-string value,
/// anything `syn` itself can't parse).
fn read_avalon_attr(attrs: &[syn::Attribute], key: &str) -> syn::Result<Option<String>> {
    for attr in attrs {
        if !attr.path().is_ident("avalon") {
            continue;
        }
        let mut found = None;
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident(key) {
                let value: syn::LitStr = meta.value()?.parse()?;
                found = Some(value.value());
                Ok(())
            } else {
                // A key this macro doesn't recognize at all (not just
                // "not the one we're looking for right now" — this
                // function is called once per key, so an attribute with
                // an entirely unknown key would otherwise be silently
                // accepted the first time and never flagged). Left for a
                // future pass if `#[avalon(...)]` grows more keys; today
                // there are only two (`visibility`, `default_visibility`),
                // each read independently, so this is deliberately
                // permissive rather than cross-checking against a fixed set.
                Ok(())
            }
        })?;
        if found.is_some() {
            return Ok(found);
        }
    }
    Ok(None)
}

fn field_shape(field: &syn::Field) -> syn::Result<FieldShape> {
    let name = field
        .ident
        .as_ref()
        .ok_or_else(|| syn::Error::new_spanned(field, "AvalonSchema requires named fields"))?
        .to_string();

    let (inner_ty, repeated, optional) = unwrap_container(&field.ty);
    let proto_type = proto_scalar_type(inner_ty).ok_or_else(|| {
        syn::Error::new_spanned(
            &field.ty,
            "AvalonSchema only supports String, bool, i32/i64/u32/u64, f32/f64, \
             and Option<T>/Vec<T> of one of those — nested messages, maps, and enums \
             aren't supported yet",
        )
    })?;

    let visibility_override = read_avalon_attr(&field.attrs, "visibility")?;
    if let Some(v) = &visibility_override {
        if !is_valid_visibility(v) {
            return Err(syn::Error::new_spanned(
                field,
                format!(
                    r#"#[avalon(visibility = "...")] must be "public" or "private", got "{v}""#
                ),
            ));
        }
    }

    Ok(FieldShape {
        name,
        proto_type,
        repeated,
        optional,
        visibility_override,
    })
}

/// The generated `.proto` message text — always exactly one top-level
/// `message` (matching `crates/server/src/proto_schema.rs`'s own "exactly
/// one top-level message" requirement), named after the struct itself,
/// fields numbered sequentially in declaration order starting at 1. Field
/// order determining field numbers is a real, deliberate property, not an
/// oversight: `integrator_schemas::publish_schema_version` never updates a
/// published version in place, only ever publishes a new one (#255's
/// immutability invariant) — there is no "evolve this schema's field
/// numbers over time" case to design around, because that's not how a
/// schema version changes here at all.
fn proto_message_text(name: &str, fields: &[FieldShape]) -> String {
    let mut lines = vec![
        "syntax = \"proto3\";".to_string(),
        String::new(),
        format!("message {name} {{"),
    ];
    for (index, field) in fields.iter().enumerate() {
        let number = index + 1;
        let modifier = if field.repeated {
            "repeated "
        } else if field.optional {
            "optional "
        } else {
            ""
        };
        lines.push(format!(
            "  {modifier}{} {} = {number};",
            field.proto_type, field.name
        ));
    }
    lines.push("}".to_string());
    lines.push(String::new());
    lines.join("\n")
}

/// Only fields carrying an explicit `#[avalon(visibility = "...")]`
/// attribute end up in the generated `field_visibility` map — a field
/// with no attribute falls back to `default_visibility` on the server
/// side already (`integrator_schemas.rs`'s own documented semantics), so
/// listing it here too would be redundant, and worse, a second place that
/// same fact could drift out of sync with the field's actual attribute if
/// one were ever added or removed later.
fn build_field_visibility(fields: &[FieldShape]) -> Vec<(String, String)> {
    fields
        .iter()
        .filter_map(|f| f.visibility_override.clone().map(|v| (f.name.clone(), v)))
        .collect()
}

/// The real entry point [`lib.rs`](super)'s `#[proc_macro_derive]`
/// function calls — takes/returns `proc_macro2::TokenStream` rather than
/// `proc_macro::TokenStream` so it's callable from plain `cargo test`
/// too (the latter type only exists inside an active proc-macro
/// invocation).
pub fn derive_avalon_schema(input: DeriveInput) -> syn::Result<TokenStream> {
    let struct_name = input.ident.to_string();
    let ident = &input.ident;

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input,
            "AvalonSchema can only be derived for structs",
        ));
    };
    let Fields::Named(named_fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &input,
            "AvalonSchema requires a struct with named fields",
        ));
    };

    let fields: Vec<FieldShape> = named_fields
        .named
        .iter()
        .map(field_shape)
        .collect::<syn::Result<_>>()?;

    let default_visibility = read_avalon_attr(&input.attrs, "default_visibility")?
        .unwrap_or_else(|| "public".to_string());
    if !is_valid_visibility(&default_visibility) {
        return Err(syn::Error::new_spanned(
            &input,
            format!(
                r#"#[avalon(default_visibility = "...")] must be "public" or "private", got "{default_visibility}""#
            ),
        ));
    }

    let proto_source = proto_message_text(&struct_name, &fields);
    let field_visibility_entries = build_field_visibility(&fields);
    let field_visibility_inserts = field_visibility_entries.iter().map(|(name, vis)| {
        quote! { map.insert(#name.to_string(), #vis.to_string()); }
    });

    Ok(quote! {
        impl ::avalon_sdk::schema::AvalonSchema for #ident {
            fn proto_source() -> ::std::string::String {
                #proto_source.to_string()
            }

            fn default_visibility() -> &'static str {
                #default_visibility
            }

            fn field_visibility() -> ::std::collections::BTreeMap<::std::string::String, ::std::string::String> {
                let mut map = ::std::collections::BTreeMap::new();
                #(#field_visibility_inserts)*
                map
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expand(source: &str) -> syn::Result<TokenStream> {
        let input: DeriveInput = syn::parse_str(source).unwrap();
        derive_avalon_schema(input)
    }

    #[test]
    fn generates_a_message_with_scalar_fields_numbered_in_order() {
        let output = expand(
            r#"
            struct CharacterSummary {
                name: String,
                level: u32,
                score: f64,
            }
            "#,
        )
        .unwrap();
        let rendered = output.to_string();
        assert!(rendered.contains("message CharacterSummary"));
        assert!(rendered.contains("string name = 1"));
        assert!(rendered.contains("uint32 level = 2"));
        assert!(rendered.contains("double score = 3"));
    }

    #[test]
    fn option_becomes_optional_and_vec_becomes_repeated() {
        let output = expand(
            r#"
            struct Loadout {
                guild: Option<String>,
                tags: Vec<String>,
            }
            "#,
        )
        .unwrap();
        let rendered = output.to_string();
        assert!(rendered.contains("optional string guild = 1"));
        assert!(rendered.contains("repeated string tags = 2"));
    }

    #[test]
    fn unsupported_field_type_is_a_clear_compile_error() {
        let err = expand(
            r#"
            struct Bad {
                nested: SomeOtherStruct,
            }
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("only supports"));
    }

    #[test]
    fn tuple_struct_is_rejected_with_a_clear_error() {
        let err = expand("struct Bad(String, u32);").unwrap_err();
        assert!(err.to_string().contains("named fields"));
    }

    #[test]
    fn field_visibility_override_is_captured() {
        let output = expand(
            r#"
            struct Profile {
                #[avalon(visibility = "private")]
                real_name: String,
                nickname: String,
            }
            "#,
        )
        .unwrap();
        let rendered = output.to_string();
        assert!(rendered.contains("real_name"));
        assert!(rendered.contains("private"));
        // Only the overridden field appears in the generated inserts —
        // `nickname` (no attribute) must not show up as a literal insert
        // call at all.
        assert!(!rendered.contains(r#"map . insert ("nickname""#));
    }

    #[test]
    fn invalid_visibility_value_is_a_clear_compile_error() {
        let err = expand(
            r#"
            struct Profile {
                #[avalon(visibility = "hidden")]
                real_name: String,
            }
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains(r#""public" or "private""#));
    }

    #[test]
    fn struct_level_default_visibility_is_captured() {
        let output = expand(
            r#"
            #[avalon(default_visibility = "private")]
            struct Profile {
                name: String,
            }
            "#,
        )
        .unwrap();
        let rendered = output.to_string();
        assert!(rendered.contains(r#"fn default_visibility () -> & 'static str { "private" }"#));
    }

    #[test]
    fn default_visibility_defaults_to_public_when_omitted() {
        let output = expand(
            r#"
            struct Profile {
                name: String,
            }
            "#,
        )
        .unwrap();
        let rendered = output.to_string();
        assert!(rendered.contains(r#""public""#));
    }
}
