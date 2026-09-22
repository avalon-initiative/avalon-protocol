//! Cross-SDK conformance suite (issue #727, epic #722) — loads the shared
//! test vectors under `conformance/vectors/` (repo root) and asserts this
//! crate's (and, where the behavior actually lives one layer down, the
//! `avalon-protocol` crate this SDK depends on and re-exports through)
//! real implementation produces byte-for-byte identical output. Pure,
//! offline, no server/database needed — runs in `cargo test -p avalon-sdk`
//! same as every other non-`--ignored` test here.
//!
//! Client-side wire-shape codegen (#723-#726) already covers plain
//! request/response shapes; this suite exists for the "smart client"
//! behavior that codegen can't produce — a real signing algorithm, a real
//! derivation, a real multi-step handshake — see #714's own decision and
//! `conformance/vectors/SCHEMA.md` for the full rationale.
//!
//! When a vector's `supportedIn` doesn't list `"rust"`, this file asserts
//! nothing false: it prints an explicit, named skip rather than faking a
//! pass. See each vector file's own `notSupported.rust` entry for why.

use avalon_protocol::continuation::signing_bytes as continuation_signing_bytes;
use avalon_protocol::cross_node_login::signing_bytes as cross_node_login_signing_bytes;
use avalon_protocol::interest_claim::{
    signing_bytes as interest_claim_signing_bytes, ClaimedScope,
};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use serde_json::Value;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;
use uuid::Uuid;

fn vectors_dir() -> PathBuf {
    // crates/sdk/tests -> repo root -> conformance/vectors
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/sdk sits two levels under the repo root")
        .join("conformance/vectors")
}

fn load(name: &str) -> Value {
    let path = vectors_dir().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read conformance vector {}: {e}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("invalid JSON in {}: {e}", path.display()))
}

fn supported_in(doc: &Value, lang: &str) -> bool {
    doc["supportedIn"]
        .as_array()
        .expect("supportedIn must be an array")
        .iter()
        .any(|v| v.as_str() == Some(lang))
}

fn signing_key_from_seed_hex(hex_seed: &str) -> SigningKey {
    let bytes = hex::decode(hex_seed).expect("valid hex seed");
    let seed: [u8; 32] = bytes.try_into().expect("32-byte seed");
    SigningKey::from_bytes(&seed)
}

fn parse_uuid(v: &Value, field: &str) -> Uuid {
    Uuid::parse_str(
        v[field]
            .as_str()
            .unwrap_or_else(|| panic!("missing {field}")),
    )
    .unwrap_or_else(|e| panic!("invalid uuid in {field}: {e}"))
}

fn parse_offset(v: &Value, field: &str) -> OffsetDateTime {
    let secs = v[field]
        .as_i64()
        .unwrap_or_else(|| panic!("missing {field}"));
    OffsetDateTime::from_unix_timestamp(secs)
        .unwrap_or_else(|e| panic!("invalid timestamp {field}: {e}"))
}

fn to_hex(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

#[test]
fn cross_node_login_grant_signing_matches_shared_vectors() {
    let doc = load("cross-node-login.json");
    assert!(
        supported_in(&doc, "rust"),
        "cross-node-login.json must list rust in supportedIn — crates/sdk/src/cross_node_login.rs \
         wraps avalon_protocol::cross_node_login::signing_bytes directly"
    );

    let signing_key = signing_key_from_seed_hex(doc["signingKeySeedHex"].as_str().unwrap());
    let expected_pub = doc["signingPublicKeyHex"].as_str().unwrap();
    assert_eq!(
        to_hex(signing_key.verifying_key().as_bytes()),
        expected_pub,
        "the shared test keypair's derived public key must match the vector file"
    );

    for vector in doc["vectors"].as_array().unwrap() {
        let name = vector["name"].as_str().unwrap_or("<unnamed>");
        let input = &vector["input"];
        let identity_id = parse_uuid(input, "identityId");
        let signing_key_id = parse_uuid(input, "signingKeyId");
        let destination_base_url = input["destinationBaseUrl"].as_str().unwrap();
        let requesting_context = input["requestingContext"].as_str().unwrap();
        let nonce = parse_uuid(input, "nonce");
        let issued_at =
            OffsetDateTime::from_unix_timestamp(input["issuedAtUnixSeconds"].as_i64().unwrap())
                .unwrap();
        let expires_at =
            OffsetDateTime::from_unix_timestamp(input["expiresAtUnixSeconds"].as_i64().unwrap())
                .unwrap();

        let bytes = cross_node_login_signing_bytes(
            identity_id,
            signing_key_id,
            destination_base_url,
            requesting_context,
            nonce,
            issued_at,
            expires_at,
        );

        let expected_bytes_utf8 = vector["expected"]["signingBytesUtf8"].as_str().unwrap();
        assert_eq!(
            String::from_utf8(bytes.clone()).unwrap(),
            expected_bytes_utf8,
            "[{name}] signing bytes diverged from the shared vector"
        );

        let signature = signing_key.sign(&bytes);
        let expected_sig_hex = vector["expected"]["signatureHex"].as_str().unwrap();
        assert_eq!(
            to_hex(&signature.to_bytes()),
            expected_sig_hex,
            "[{name}] Ed25519 signature diverged from the shared vector — a grant minted by \
             the Rust SDK would not be interchangeable with one from another SDK for the same input"
        );

        // Round-trip: the vector's own recorded signature must still verify
        // against the shared public key.
        let verifying_key =
            VerifyingKey::from_bytes(signing_key.verifying_key().as_bytes()).unwrap();
        let recorded_sig_bytes: [u8; 64] =
            hex::decode(expected_sig_hex).unwrap().try_into().unwrap();
        verifying_key
            .verify_strict(
                &bytes,
                &ed25519_dalek::Signature::from_bytes(&recorded_sig_bytes),
            )
            .unwrap_or_else(|e| panic!("[{name}] recorded vector signature does not verify: {e}"));
    }
}

#[test]
fn session_continuation_token_signing_is_a_known_rust_sdk_gap() {
    let doc = load("session-continuation.json");
    if supported_in(&doc, "rust") {
        panic!(
            "session-continuation.json now lists rust in supportedIn, but this test only \
             documents the gap — implement real assertions here (mirroring \
             cross_node_login_grant_signing_matches_shared_vectors above) before flipping supportedIn"
        );
    }
    let gap = doc["notSupported"]["rust"]
        .as_str()
        .expect("notSupported.rust must explain why rust is missing");
    println!(
        "SKIP conformance/vectors/session-continuation.json for rust: {gap}\n\
         (avalon_protocol::continuation::signing_bytes exists and is verified below against \
         the shared vector's signing-bytes format, since crates/protocol IS shared workspace \
         code — but nothing in crates/sdk exposes client-side minting yet)"
    );

    // Even though crates/sdk has no minting API, avalon_protocol (which
    // crates/sdk depends on) defines the exact signing-bytes contract a
    // future crates/sdk implementation would have to match — verify that
    // contract against the vector now, so a change to the shared protocol
    // format itself is still caught here.
    let vector = &doc["vectors"][0];
    let input = &vector["input"];
    let identity_id = parse_uuid(input, "identityId");
    let signing_key_id = parse_uuid(input, "signingKeyId");
    let nonce = parse_uuid(input, "nonce");
    let issued_at = parse_offset(input, "issuedAtUnixSeconds");
    let expires_at = parse_offset(input, "expiresAtUnixSeconds");
    let bytes =
        continuation_signing_bytes(identity_id, signing_key_id, nonce, issued_at, expires_at);
    let expected = vector["expected"]["signingBytesUtf8"].as_str().unwrap();
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        expected,
        "avalon_protocol::continuation::signing_bytes diverged from the shared vector"
    );
}

#[test]
fn websocket_interest_claim_signing_is_a_known_rust_sdk_gap() {
    let doc = load("websocket-interest-claim.json");
    if supported_in(&doc, "rust") {
        panic!(
            "websocket-interest-claim.json now lists rust in supportedIn, but this test only \
             documents the gap — implement real assertions here before flipping supportedIn"
        );
    }
    let gap = doc["notSupported"]["rust"]
        .as_str()
        .expect("notSupported.rust must explain why rust is missing");
    println!(
        "SKIP conformance/vectors/websocket-interest-claim.json for rust: {gap}\n\
         (avalon_protocol::interest_claim::signing_bytes is verified below against the shared \
         vector's signing-bytes format — crates/sdk itself has no interest-claim handshake yet)"
    );

    let vector = &doc["vectors"][0];
    let input = &vector["input"];
    let identity_id = parse_uuid(input, "identityId");
    let signing_key_id = parse_uuid(input, "signingKeyId");
    let channel_id = parse_uuid(&input["scope"], "channelId");
    let base_url = input["baseUrl"].as_str().unwrap();
    let nonce = parse_uuid(input, "nonce");
    let issued_at = parse_offset(input, "issuedAtUnixSeconds");
    let expires_at = parse_offset(input, "expiresAtUnixSeconds");
    let bytes = interest_claim_signing_bytes(
        identity_id,
        signing_key_id,
        ClaimedScope::Channel { channel_id },
        base_url,
        nonce,
        issued_at,
        expires_at,
    );
    let expected = vector["expected"]["signingBytesUtf8"].as_str().unwrap();
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        expected,
        "avalon_protocol::interest_claim::signing_bytes diverged from the shared vector"
    );
}

#[test]
fn bip39_mnemonic_derivation_is_a_known_rust_sdk_gap() {
    let doc = load("bip39-mnemonic.json");
    assert!(
        !supported_in(&doc, "rust"),
        "bip39-mnemonic.json now lists rust in supportedIn, but crates/sdk has no BIP39 \
         dependency or derivation code — add a real implementation and real assertions here \
         before flipping supportedIn, don't just relabel the vector file"
    );
    let gap = doc["notSupported"]["rust"]
        .as_str()
        .expect("notSupported.rust must explain why rust is missing");
    println!("SKIP conformance/vectors/bip39-mnemonic.json for rust: {gap}");
}
