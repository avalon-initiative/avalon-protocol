//! Real protobuf parsing/validation for Integrator Space schemas and
//! instance data — replacing the original
//! "store `proto_source` opaquely, never parse it" stance. See
//! `docs/projects/backend-server/architecture/integrator-space.md`'s "Today in the repo" for the
//! parser toolchain, the single-root-message convention, and why parsing
//! happens in a scoped `TempDir` with no panics on integrator input.

use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex};

use protobuf::reflect::{FileDescriptor, MessageDescriptor};

use crate::error::AppError;

/// The file name every parsed schema is given inside its scratch temp
/// directory — there is only ever one file (no imports are supported; a
/// schema is meant to be one self-contained `.proto` document), so the
/// name itself is never observed by a caller.
const VIRTUAL_FILE_NAME: &str = "schema.proto";

/// In-process cache of already-parsed root messages, keyed by schema id
/// (`game:<slug>:schema:<version>`). Schema versions are immutable once
/// published (`integrator_schemas`'s own invariant), so a cache entry never
/// goes stale — there is no invalidation to get wrong, only population.
/// At integrator scale, re-running the pure-Rust `.proto` parser (a real,
/// non-trivial CPU cost: a temp file write + full lex/parse/typecheck)
/// once per *instance-data write* rather than once per *schema
/// publication* would multiply that cost across every write from every
/// integrator — this cache is what keeps parsing to "once per publish,
/// once per process cache-miss thereafter" instead. A plain in-process
/// `HashMap` is enough for this pass, same reasoning `authz.rs`'s own doc
/// comment gives for not building a distributed cache before there's
/// real call volume to profile against.
static ROOT_MESSAGE_CACHE: LazyLock<Mutex<HashMap<String, MessageDescriptor>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Recovers from a poisoned lock (a prior panic while holding it)
/// rather than propagating that poisoning into an `unwrap` panic here —
/// this cache is pure optimization, never a correctness dependency, so
/// the right behavior on poisoning is "keep going, possibly re-parsing
/// more than strictly necessary," never "bring this request down too."
fn lock_cache() -> std::sync::MutexGuard<'static, HashMap<String, MessageDescriptor>> {
    ROOT_MESSAGE_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// [`parse_root_message`], but checks/populates [`ROOT_MESSAGE_CACHE`]
/// first — the entry point [`crate::integrator_data::publish_instance`] uses
/// (validating an instance re-parses nothing once the schema's first
/// instance write has warmed the cache). `integrator_schemas::publish_schema_version`
/// calls [`cache_root_message`] directly at publish time instead, so the
/// very first instance write against a brand-new schema is already a
/// cache hit too.
pub fn parse_root_message_cached(
    schema_id: &str,
    proto_source: &str,
) -> Result<MessageDescriptor, AppError> {
    if let Some(cached) = lock_cache().get(schema_id) {
        return Ok(cached.clone());
    }
    let root = parse_root_message(proto_source)?;
    lock_cache().insert(schema_id.to_string(), root.clone());
    Ok(root)
}

/// Populates the cache for `schema_id` with an already-parsed root
/// message — called right after a successful publish
/// (`integrator_schemas::publish_schema_version`), so parsing happens exactly
/// once per publication even under concurrent instance-data writes
/// racing to be the first cache populator.
pub fn cache_root_message(schema_id: &str, root: MessageDescriptor) {
    lock_cache().insert(schema_id.to_string(), root);
}

/// Parses `proto_source` and resolves its single root message descriptor
/// (see module doc comment for the "exactly one top-level message"
/// convention). This is the one function both call sites above go
/// through — never call `protobuf_parse`/`protobuf::reflect` directly
/// from elsewhere in this crate.
pub fn parse_root_message(proto_source: &str) -> Result<MessageDescriptor, AppError> {
    let dir = tempfile::tempdir().map_err(|e| AppError::InvalidProtoSchema {
        detail: format!("could not allocate scratch space to parse schema: {e}"),
    })?;
    let file_path = dir.path().join(VIRTUAL_FILE_NAME);
    std::fs::write(&file_path, proto_source).map_err(|e| AppError::InvalidProtoSchema {
        detail: format!("could not stage schema source for parsing: {e}"),
    })?;

    let file_descriptor_set = protobuf_parse::Parser::new()
        .pure()
        .include(dir.path())
        .input(&file_path)
        .file_descriptor_set()
        .map_err(|e| AppError::InvalidProtoSchema {
            detail: e.to_string(),
        })?;
    // `dir` (and the temp file inside it) is deleted here, once parsing —
    // the only reason it existed — is done, success or failure alike.
    drop(dir);

    let proto = file_descriptor_set.file.into_iter().next().ok_or_else(|| {
        AppError::InvalidProtoSchema {
            detail: "no file descriptor produced by the parser".to_string(),
        }
    })?;

    let file_descriptor =
        FileDescriptor::new_dynamic(proto, &[]).map_err(|e| AppError::InvalidProtoSchema {
            detail: e.to_string(),
        })?;

    let mut top_level_messages: Vec<MessageDescriptor> = file_descriptor.messages().collect();
    match top_level_messages.len() {
        0 => Err(AppError::InvalidProtoSchema {
            detail: "schema must declare exactly one top-level message; found none".to_string(),
        }),
        1 => Ok(top_level_messages.remove(0)),
        n => Err(AppError::InvalidProtoSchema {
            detail: format!(
                "schema must declare exactly one top-level message; found {n} ({})",
                top_level_messages
                    .iter()
                    .map(|m| m.name().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }),
    }
}

/// Validates that every key in `field_visibility` names a real field of
/// `root` — issue #384's amendment: this is now actually checkable, so a
/// typo'd/nonexistent field name is rejected at publish time rather than
/// silently accepted into a manifest nothing will ever match.
pub fn validate_field_visibility_keys(
    root: &MessageDescriptor,
    field_visibility: &BTreeMap<String, String>,
) -> Result<(), AppError> {
    for field_name in field_visibility.keys() {
        if root.field_by_name(field_name).is_none() {
            return Err(AppError::InvalidProtoSchema {
                detail: format!(
                    "field_visibility names field `{field_name}`, which is not a field of the schema's root message `{}`",
                    root.name()
                ),
            });
        }
    }
    Ok(())
}

/// Parses+validates `instance_json` (a JSON object, as submitted) against
/// `root` via `protobuf-json-mapping`'s dynamic-message parser. Rejects
/// unknown fields (the parser's default — matching this repo's existing
/// "reject, don't silently drop" posture, e.g. `handlers::validate_favorite_genres`),
/// wrong types, and — for a proto2-style schema — missing `required`
/// fields (`is_initialized_dyn`, checked explicitly after a successful
/// parse since a syntactically-valid-but-incomplete message parses fine
/// on its own). The *submitted* JSON is returned unchanged on success —
/// per #384's amendment, the validated instance is still stored/served as
/// plain JSON, never re-encoded through the dynamic message.
pub fn validate_instance_json(
    root: &MessageDescriptor,
    instance_json: &serde_json::Value,
) -> Result<(), AppError> {
    if !instance_json.is_object() {
        return Err(AppError::InstanceSchemaMismatch {
            detail: "instance must be a JSON object".to_string(),
        });
    }
    let json_text = instance_json.to_string();
    let message = protobuf_json_mapping::parse_dyn_from_str(root, &json_text).map_err(|e| {
        AppError::InstanceSchemaMismatch {
            detail: e.to_string(),
        }
    })?;
    if !message.is_initialized_dyn() {
        return Err(AppError::InstanceSchemaMismatch {
            detail: "instance is missing one or more required fields".to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! No live Postgres needed — these exercise the parser/reflection
    //! machinery directly, pure logic (aside from the parser's own,
    //! entirely in-memory work).

    use super::*;

    const CHARACTER_PROTO: &str =
        "syntax = \"proto3\"; message Character { uint32 level = 1; string name = 2; }";

    #[test]
    fn parses_a_well_formed_single_message_schema() {
        let root = parse_root_message(CHARACTER_PROTO).unwrap();
        assert_eq!(root.name(), "Character");
        assert!(root.field_by_name("level").is_some());
        assert!(root.field_by_name("name").is_some());
    }

    #[test]
    fn rejects_malformed_proto_source() {
        let err = parse_root_message("this is not valid proto syntax {{{").unwrap_err();
        assert!(matches!(err, AppError::InvalidProtoSchema { .. }));
    }

    #[test]
    fn rejects_zero_top_level_messages() {
        let err = parse_root_message("syntax = \"proto3\";").unwrap_err();
        assert!(matches!(err, AppError::InvalidProtoSchema { .. }));
    }

    #[test]
    fn rejects_multiple_top_level_messages() {
        let source = "syntax = \"proto3\"; message A { uint32 x = 1; } message B { uint32 y = 1; }";
        let err = parse_root_message(source).unwrap_err();
        assert!(matches!(err, AppError::InvalidProtoSchema { .. }));
    }

    #[test]
    fn field_visibility_rejects_unknown_field_name() {
        let root = parse_root_message(CHARACTER_PROTO).unwrap();
        let mut field_visibility = BTreeMap::new();
        field_visibility.insert("does_not_exist".to_string(), "private".to_string());
        let err = validate_field_visibility_keys(&root, &field_visibility).unwrap_err();
        assert!(matches!(err, AppError::InvalidProtoSchema { .. }));
    }

    #[test]
    fn field_visibility_accepts_real_field_names() {
        let root = parse_root_message(CHARACTER_PROTO).unwrap();
        let mut field_visibility = BTreeMap::new();
        field_visibility.insert("level".to_string(), "private".to_string());
        assert!(validate_field_visibility_keys(&root, &field_visibility).is_ok());
    }

    #[test]
    fn validates_a_conforming_instance() {
        let root = parse_root_message(CHARACTER_PROTO).unwrap();
        let instance = serde_json::json!({ "level": 5, "name": "Aria" });
        assert!(validate_instance_json(&root, &instance).is_ok());
    }

    #[test]
    fn rejects_an_instance_with_an_unknown_field() {
        let root = parse_root_message(CHARACTER_PROTO).unwrap();
        let instance = serde_json::json!({ "level": 5, "unknown_field": "x" });
        let err = validate_instance_json(&root, &instance).unwrap_err();
        assert!(matches!(err, AppError::InstanceSchemaMismatch { .. }));
    }

    #[test]
    fn rejects_an_instance_with_a_wrong_type() {
        let root = parse_root_message(CHARACTER_PROTO).unwrap();
        let instance = serde_json::json!({ "level": "not a number" });
        let err = validate_instance_json(&root, &instance).unwrap_err();
        assert!(matches!(err, AppError::InstanceSchemaMismatch { .. }));
    }

    #[test]
    fn rejects_a_non_object_instance() {
        let root = parse_root_message(CHARACTER_PROTO).unwrap();
        let instance = serde_json::json!([1, 2, 3]);
        let err = validate_instance_json(&root, &instance).unwrap_err();
        assert!(matches!(err, AppError::InstanceSchemaMismatch { .. }));
    }

    #[test]
    fn rejects_missing_required_field_in_proto2_schema() {
        let source = "syntax = \"proto2\"; message Character { required uint32 level = 1; optional string name = 2; }";
        let root = parse_root_message(source).unwrap();
        let instance = serde_json::json!({ "name": "Aria" });
        let err = validate_instance_json(&root, &instance).unwrap_err();
        assert!(matches!(err, AppError::InstanceSchemaMismatch { .. }));
    }
}
