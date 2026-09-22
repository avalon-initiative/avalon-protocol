//! Generates wire-shape request/response types for a hand-picked slice of
//! the account/auth API surface from `docs/generated/openapi.json` (#723),
//! via `typify` (JSON Schema -> Rust types). Output lands in `OUT_DIR` and
//! is pulled in by `src/generated.rs`'s `include!`.
//!
//! `SCHEMA_NAMES` below is a deliberate allowlist, not "generate
//! everything" — issue #724's first slice only. Four WebAuthn-ceremony
//! schemas (`RegisterStartResponse`, `RegisterFinishRequest`,
//! `SessionStartResponse`, `SessionFinishRequest`) are excluded on purpose:
//! their `challenge`/`credential`/`webauthn_credential` fields are opaque
//! `"type": "object"` blobs in the schema (since `webauthn-rs`'s own types
//! have no `ToSchema` impl), and typify would flatten them to
//! `serde_json::Value`, discarding the real `passkey_types::webauthn::*`
//! typing `crates/sdk/src/account/mod.rs` relies on to drive the ceremony —
//! those stay hand-written. `UpdateProfileRequest`'s three-state
//! (omitted/null/value) `PATCH /me` semantics are also deferred pending
//! verification that typify's default output preserves that behavior.
//!
//! `sdk` reads `openapi.json` as a plain checked-in data file, not a Cargo
//! build-graph output — `crates/server` already depends on `avalon-sdk`,
//! so `sdk` depending on `avalon-server` (or invoking its `dump-openapi`
//! binary) would create a workspace cycle.

use std::env;
use std::fs;
use std::path::Path;

use schemars::schema::Schema;
use typify::{TypeSpace, TypeSpaceSettings};

const SCHEMA_NAMES: &[&str] = &[
    "ProfileResponse",
    "DeviceResponse",
    "RegisterStartRequest",
    "RegisterFinishResponse",
    "SessionStartRequest",
    "SessionFinishResponse",
    // Pulled in transitively by ProfileResponse.favorite_genres — replaced
    // below with the existing `avalon_protocol::identity::Genre` rather
    // than generated fresh, since that's already the real type
    // `avalon_protocol::identity::Profile` (and its callers) uses.
    "Genre",
];

/// Typify hardcodes `"format": "date-time"` to `chrono::DateTime<Utc>`,
/// with no setting to redirect it to the `time` crate this workspace
/// actually uses everywhere else (`Cargo.toml` has no `chrono` dependency
/// at all — adding one just for this would duplicate date/time handling
/// crate-wide for three fields). Instead, strip the `date-time` format
/// hint before handing schemas to typify, so those fields come through as
/// plain `String`, and parse them with `time`'s own RFC3339 support at the
/// handful of call sites that read them (see `account/mod.rs`).
fn strip_date_time_format(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if map.get("format").and_then(|f| f.as_str()) == Some("date-time") {
                map.remove("format");
            }
            for v in map.values_mut() {
                strip_date_time_format(v);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                strip_date_time_format(v);
            }
        }
        _ => {}
    }
}

fn main() {
    let openapi_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/generated/openapi.json");
    println!("cargo:rerun-if-changed={}", openapi_path.display());

    let raw = fs::read_to_string(&openapi_path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", openapi_path.display()));
    let doc: serde_json::Value =
        serde_json::from_str(&raw).expect("docs/generated/openapi.json is valid JSON");
    let schemas = doc["components"]["schemas"]
        .as_object()
        .expect("docs/generated/openapi.json has components.schemas");

    let defs: Vec<(String, Schema)> = SCHEMA_NAMES
        .iter()
        .map(|name| {
            let mut value = schemas
                .get(*name)
                .unwrap_or_else(|| {
                    panic!(
                        "docs/generated/openapi.json is missing components.schemas.{name} \
                         — update crates/sdk/build.rs's SCHEMA_NAMES or check for a schema rename"
                    )
                })
                .clone();
            strip_date_time_format(&mut value);
            let schema: Schema = serde_json::from_value(value).unwrap_or_else(|e| {
                panic!("components.schemas.{name} did not parse as a JSON Schema: {e}")
            });
            (name.to_string(), schema)
        })
        .collect();

    let mut settings = TypeSpaceSettings::default();
    settings.with_replacement(
        "Genre",
        "avalon_protocol::identity::Genre",
        std::iter::empty(),
    );
    let mut type_space = TypeSpace::new(&settings);
    type_space
        .add_ref_types(defs)
        .expect("typify failed to convert the account/auth schema allowlist");

    let out_path = Path::new(&env::var("OUT_DIR").unwrap()).join("generated.rs");
    fs::write(&out_path, type_space.to_stream().to_string())
        .unwrap_or_else(|e| panic!("writing {}: {e}", out_path.display()));
}
