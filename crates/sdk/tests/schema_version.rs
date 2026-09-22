//! Issue #735: `OPENAPI_SCHEMA_VERSION` is generated straight from
//! `docs/generated/openapi.json`'s own `info.version` by `build.rs`, so it
//! can't drift by construction — this exists to catch a future change to
//! that generation step breaking the link, not routine drift.

#[test]
fn schema_version_matches_openapi_json() {
    let openapi_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/generated/openapi.json");
    let raw = std::fs::read_to_string(&openapi_path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", openapi_path.display()));
    let doc: serde_json::Value =
        serde_json::from_str(&raw).expect("docs/generated/openapi.json is valid JSON");
    let expected = doc["info"]["version"]
        .as_str()
        .expect("docs/generated/openapi.json has info.version");
    assert_eq!(avalon_sdk::OPENAPI_SCHEMA_VERSION, expected);
}
