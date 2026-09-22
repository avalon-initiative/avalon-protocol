//! Cross-SDK conformance suite (issue #727, epic #722) — loads the shared
//! test vectors under `conformance/vectors/` (repo root) and asserts this
//! crate's real implementation produces byte-for-byte identical output.
//! Pure, offline, no server/database needed — runs in
//! `cargo test -p avalon-sdk` same as every other non-`--ignored` test here.
//!
//! Client-side wire-shape codegen (#723-#726) already covers plain
//! request/response shapes; this suite exists for the "smart client"
//! behavior that codegen can't produce — a real signing algorithm, a real
//! derivation, a real multi-step handshake — see #714's own decision and
//! `conformance/vectors/SCHEMA.md` for the full rationale.
//!
//! Since issue #774 this crate no longer shares Rust source with
//! `avalon-protocol`, so the signing-byte constructions below are this
//! SDK's own independent implementations, exactly like the C# and
//! TypeScript SDKs'. These vectors are what proves they haven't drifted
//! from the server's; `crates/protocol/tests/conformance.rs` asserts the
//! *server* side of each of the same vector files, so a divergence fails a
//! test on whichever side moved.
//!
//! When a vector's `supportedIn` doesn't list `"rust"`, this file asserts
//! nothing false: it prints an explicit, named skip rather than faking a
//! pass. See each vector file's own `notSupported.rust` entry for why.

use avalon_sdk::achievements::{
    attestation_signing_bytes, bulk_attestation_signing_bytes, revocation_signing_bytes,
};
use avalon_sdk::cross_node_login::signing_bytes as cross_node_login_signing_bytes;
use avalon_sdk::sth::signing_message as sth_signing_message;
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

fn to_hex(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

/// Asserts a vector's recorded signature both matches what this SDK
/// produces for `bytes` and still verifies against the file's shared
/// public key — the second half catches a vector file edited by hand to
/// match a broken implementation.
fn assert_signature_matches(
    name: &str,
    signing_key: &SigningKey,
    bytes: &[u8],
    expected_sig_hex: &str,
) {
    let signature = signing_key.sign(bytes);
    assert_eq!(
        to_hex(&signature.to_bytes()),
        expected_sig_hex,
        "[{name}] Ed25519 signature diverged from the shared vector — a payload signed by \
         the Rust SDK would not be interchangeable with one from another SDK for the same input"
    );

    let verifying_key = VerifyingKey::from_bytes(signing_key.verifying_key().as_bytes()).unwrap();
    let recorded_sig_bytes: [u8; 64] = hex::decode(expected_sig_hex)
        .expect("signatureHex must be valid hex")
        .try_into()
        .expect("signatureHex must be 64 bytes");
    verifying_key
        .verify_strict(
            bytes,
            &ed25519_dalek::Signature::from_bytes(&recorded_sig_bytes),
        )
        .unwrap_or_else(|e| panic!("[{name}] recorded vector signature does not verify: {e}"));
}

#[test]
fn cross_node_login_grant_signing_matches_shared_vectors() {
    let doc = load("cross-node-login.json");
    assert!(
        supported_in(&doc, "rust"),
        "cross-node-login.json must list rust in supportedIn — \
         avalon_sdk::cross_node_login::signing_bytes implements it"
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
        let issued_at =
            OffsetDateTime::from_unix_timestamp(input["issuedAtUnixSeconds"].as_i64().unwrap())
                .unwrap();
        let expires_at =
            OffsetDateTime::from_unix_timestamp(input["expiresAtUnixSeconds"].as_i64().unwrap())
                .unwrap();

        let bytes = cross_node_login_signing_bytes(
            parse_uuid(input, "identityId"),
            parse_uuid(input, "signingKeyId"),
            input["destinationBaseUrl"].as_str().unwrap(),
            input["requestingContext"].as_str().unwrap(),
            parse_uuid(input, "nonce"),
            issued_at,
            expires_at,
        );

        assert_eq!(
            String::from_utf8(bytes.clone()).unwrap(),
            vector["expected"]["signingBytesUtf8"].as_str().unwrap(),
            "[{name}] signing bytes diverged from the shared vector"
        );
        assert_signature_matches(
            name,
            &signing_key,
            &bytes,
            vector["expected"]["signatureHex"].as_str().unwrap(),
        );
    }
}

/// Issue #774's own safety net: this crate builds attestation issuance,
/// bulk issuance, and revocation signing bytes itself rather than calling
/// the server's construction, so this is the test that would catch the two
/// drifting apart.
#[test]
fn attestation_signing_matches_shared_vectors() {
    let doc = load("attestation-signing.json");
    assert!(
        supported_in(&doc, "rust"),
        "attestation-signing.json must list rust in supportedIn — \
         avalon_sdk::achievements implements all three constructions"
    );

    let signing_key = signing_key_from_seed_hex(doc["signingKeySeedHex"].as_str().unwrap());
    assert_eq!(
        to_hex(signing_key.verifying_key().as_bytes()),
        doc["signingPublicKeyHex"].as_str().unwrap(),
        "the shared test keypair's derived public key must match the vector file"
    );

    for vector in doc["vectors"].as_array().unwrap() {
        let name = vector["name"].as_str().unwrap_or("<unnamed>");
        let input = &vector["input"];
        let operation = input["operation"].as_str().unwrap();
        let claim_kind = input["claimKind"].as_str().unwrap();
        let issuer_ref = input["issuerRef"].as_str().unwrap();

        let bytes = match operation {
            "issue" => attestation_signing_bytes(
                claim_kind,
                issuer_ref,
                parse_uuid(input, "subject"),
                input["achievement"].as_str().unwrap(),
            ),
            "bulk_issue" => {
                let achievements: Vec<String> = input["achievements"]
                    .as_array()
                    .expect("bulk_issue vectors carry an achievements array")
                    .iter()
                    .map(|v| v.as_str().unwrap().to_string())
                    .collect();
                bulk_attestation_signing_bytes(
                    claim_kind,
                    issuer_ref,
                    parse_uuid(input, "subject"),
                    &achievements,
                )
            }
            "revoke" => revocation_signing_bytes(
                claim_kind,
                issuer_ref,
                parse_uuid(input, "attestationId"),
                input["reasonCode"].as_str().unwrap(),
            ),
            other => panic!("[{name}] unknown operation {other}"),
        };

        assert_eq!(
            to_hex(&bytes),
            vector["expected"]["signingBytesHex"].as_str().unwrap(),
            "[{name}] signing bytes diverged from the shared vector"
        );
        if let Some(utf8) = vector["expected"]["signingBytesUtf8"].as_str() {
            assert_eq!(
                String::from_utf8(bytes.clone()).unwrap(),
                utf8,
                "[{name}] signing bytes diverged from the shared vector"
            );
        }
        assert_signature_matches(
            name,
            &signing_key,
            &bytes,
            vector["expected"]["signatureHex"].as_str().unwrap(),
        );
    }
}

/// `crate::sth` is the other construction #774 made independent — a node's
/// Signed Tree Head is what `avalon_sdk::network` verifies a network's
/// identity against, so a drift here would make every pinned trust anchor
/// reject a legitimate node.
#[test]
fn signed_tree_head_signing_matches_shared_vectors() {
    let doc = load("signed-tree-head.json");
    assert!(
        supported_in(&doc, "rust"),
        "signed-tree-head.json must list rust in supportedIn — \
         avalon_sdk::sth implements it"
    );

    let signing_key = signing_key_from_seed_hex(doc["signingKeySeedHex"].as_str().unwrap());
    assert_eq!(
        to_hex(signing_key.verifying_key().as_bytes()),
        doc["signingPublicKeyHex"].as_str().unwrap(),
        "the shared test keypair's derived public key must match the vector file"
    );

    for vector in doc["vectors"].as_array().unwrap() {
        let name = vector["name"].as_str().unwrap_or("<unnamed>");
        let input = &vector["input"];
        let created_at =
            OffsetDateTime::from_unix_timestamp(input["createdAtUnixSeconds"].as_i64().unwrap())
                .unwrap();
        let bytes = sth_signing_message(
            input["treeSize"].as_i64().unwrap(),
            input["rootHashHex"].as_str().unwrap(),
            input["networkId"].as_str().unwrap(),
            created_at,
        );

        assert_eq!(
            to_hex(&bytes),
            vector["expected"]["signingBytesHex"].as_str().unwrap(),
            "[{name}] signing bytes diverged from the shared vector"
        );
        assert_signature_matches(
            name,
            &signing_key,
            &bytes,
            vector["expected"]["signatureHex"].as_str().unwrap(),
        );
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
         (the server-side construction is still checked against this same vector by \
         crates/protocol/tests/conformance.rs — nothing in crates/sdk exposes client-side \
         minting yet)"
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
         (the server-side construction is still checked against this same vector by \
         crates/protocol/tests/conformance.rs — crates/sdk itself has no interest-claim \
         handshake yet)"
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
