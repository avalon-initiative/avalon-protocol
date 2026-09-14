//! `#[derive(AvalonSchema)]` (issue #386) — generates the `.proto` message
//! text, and the `default_visibility`/`field_visibility` maps, an
//! Integrator Space schema publication
//! (`crates/server/src/integrator_schemas.rs`, #255/#381/#384) needs from
//! an ordinary Rust struct, so an integrator using the Rust SDK never
//! hand-writes protobuf syntax just to publish a schema.
//!
//! Not used directly — re-exported through `avalon_sdk::schema`, which
//! also defines the [`AvalonSchema`](trait@avalon_sdk::schema::AvalonSchema)
//! trait this macro implements and the `Session` methods
//! (`publish_schema_version`/`publish_instance`) that actually call the
//! server with the generated output.
//!
//! **Supported field types**: `String`, `bool`, `i32`/`i64`/`u32`/`u64`,
//! `f32`/`f64`, and `Option<T>`/`Vec<T>` of any of those. Nested messages,
//! maps, bytes, and enums are not supported in this first pass — a field
//! of any other type is a compile error naming the field, not a silently
//! wrong generated schema. See `crates/schema-derive/src/codegen.rs` for
//! the actual mapping logic (kept separate from this file specifically so
//! it's unit-testable via plain `cargo test`, without needing a real
//! proc-macro invocation).
//!
//! **Attributes**:
//! - `#[avalon(default_visibility = "private")]` on the struct — #381's
//!   schema-level opt-out. Defaults to `"public"` when omitted, matching
//!   the server's own default.
//! - `#[avalon(visibility = "private")]` on a field — overrides
//!   `default_visibility` for that field specifically, in either
//!   direction. Only fields carrying this attribute appear in the
//!   generated `field_visibility` map; a field without it simply falls
//!   back to `default_visibility` server-side, so there's nothing to list
//!   for it and nothing that could drift out of sync with the struct's
//!   actual fields.
//!
//! **Field numbering**: sequential, in declaration order, starting at 1.
//! Deliberate, not incidental — see `codegen.rs::proto_message_text`'s own
//! doc comment for why this is fine given #255's "always publish a new
//! version, never edit one in place" model.

mod codegen;

use proc_macro::TokenStream;
use syn::{parse_macro_input, DeriveInput};

#[proc_macro_derive(AvalonSchema, attributes(avalon))]
pub fn derive_avalon_schema(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match codegen::derive_avalon_schema(input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}
