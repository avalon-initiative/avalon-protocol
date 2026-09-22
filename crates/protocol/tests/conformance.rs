//! Server side of the cross-SDK conformance suite (issue #727, extended by
//! #774) — the same shared vectors under `conformance/vectors/` that each
//! SDK's own runner consumes, asserted here against this crate's
//! implementations, which are what `avalon-server` actually verifies
//! incoming signatures with.
//!
//! This half exists because #774 made the Rust SDK an independent
//! reimplementation like the C# and TypeScript ones: before it, the SDK
//! called these functions directly and the compiler guaranteed the two
//! agreed. Now nothing does — except these vectors, checked from both
//! sides. A change to a signing-byte format here that isn't mirrored in
//! every SDK fails this file; a change in an SDK that isn't mirrored here
//! fails that SDK's runner.
//!
//! Offline and dependency-free — no server, no database.

use avalon_protocol::achievements::{
    attestation_signing_bytes, bulk_attestation_signing_bytes, revocation_signing_bytes,
};
use avalon_protocol::continuation::signing_bytes as continuation_signing_bytes;
use avalon_protocol::cross_node_login::signing_bytes as cross_node_login_signing_bytes;
use avalon_protocol::ids::{AttestationId, IdentityId};
use avalon_protocol::interest_claim::{
    signing_bytes as interest_claim_signing_bytes, ClaimedScope,
};
use avalon_protocol::sth::signing_message as sth_signing_message;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::Value;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;
use uuid::Uuid;

fn vectors_dir() -> PathBuf {
    // crates/protocol -> repo root -> conformance/vectors
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/protocol sits two levels under the repo root")
        .join("conformance/vectors")
}

fn load(name: &str) -> Value {
    let path = vectors_dir().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read conformance vector {}: {e}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("invalid JSON in {}: {e}", path.display()))
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

fn assert_signature_matches(name: &str, key: &SigningKey, bytes: &[u8], expected_sig_hex: &str) {
    assert_eq!(
        hex::encode(key.sign(bytes).to_bytes()),
        expected_sig_hex,
        "[{name}] Ed25519 signature diverged from the shared vector — a signature an SDK \
         produces for this input would no longer be the one this crate verifies"
    );
}

#[test]
fn attestation_signing_matches_shared_vectors() {
    let doc = load("attestation-signing.json");
    let signing_key = signing_key_from_seed_hex(doc["signingKeySeedHex"].as_str().unwrap());
    assert_eq!(
        hex::encode(signing_key.verifying_key().as_bytes()),
        doc["signingPublicKeyHex"].as_str().unwrap(),
        "the shared test keypair's derived public key must match the vector file"
    );

    for vector in doc["vectors"].as_array().unwrap() {
        let name = vector["name"].as_str().unwrap_or("<unnamed>");
        let input = &vector["input"];
        let claim_kind = input["claimKind"].as_str().unwrap();
        let issuer_ref = input["issuerRef"].as_str().unwrap();

        let bytes = match input["operation"].as_str().unwrap() {
            "issue" => attestation_signing_bytes(
                claim_kind,
                issuer_ref,
                IdentityId(parse_uuid(input, "subject")),
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
                    IdentityId(parse_uuid(input, "subject")),
                    &achievements,
                )
            }
            "revoke" => revocation_signing_bytes(
                claim_kind,
                issuer_ref,
                AttestationId(parse_uuid(input, "attestationId")),
                input["reasonCode"].as_str().unwrap(),
            ),
            other => panic!("[{name}] unknown operation {other}"),
        };

        assert_eq!(
            hex::encode(&bytes),
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
fn signed_tree_head_signing_matches_shared_vectors() {
    let doc = load("signed-tree-head.json");
    let signing_key = signing_key_from_seed_hex(doc["signingKeySeedHex"].as_str().unwrap());

    for vector in doc["vectors"].as_array().unwrap() {
        let name = vector["name"].as_str().unwrap_or("<unnamed>");
        let input = &vector["input"];
        let bytes = sth_signing_message(
            input["treeSize"].as_i64().unwrap(),
            input["rootHashHex"].as_str().unwrap(),
            input["networkId"].as_str().unwrap(),
            parse_offset(input, "createdAtUnixSeconds"),
        );
        assert_eq!(
            hex::encode(&bytes),
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
fn cross_node_login_grant_signing_matches_shared_vectors() {
    let doc = load("cross-node-login.json");
    let signing_key = signing_key_from_seed_hex(doc["signingKeySeedHex"].as_str().unwrap());

    for vector in doc["vectors"].as_array().unwrap() {
        let name = vector["name"].as_str().unwrap_or("<unnamed>");
        let input = &vector["input"];
        let bytes = cross_node_login_signing_bytes(
            parse_uuid(input, "identityId"),
            parse_uuid(input, "signingKeyId"),
            input["destinationBaseUrl"].as_str().unwrap(),
            input["requestingContext"].as_str().unwrap(),
            parse_uuid(input, "nonce"),
            parse_offset(input, "issuedAtUnixSeconds"),
            parse_offset(input, "expiresAtUnixSeconds"),
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

#[test]
fn session_continuation_signing_matches_shared_vectors() {
    let doc = load("session-continuation.json");
    let vector = &doc["vectors"][0];
    let input = &vector["input"];
    let bytes = continuation_signing_bytes(
        parse_uuid(input, "identityId"),
        parse_uuid(input, "signingKeyId"),
        parse_uuid(input, "nonce"),
        parse_offset(input, "issuedAtUnixSeconds"),
        parse_offset(input, "expiresAtUnixSeconds"),
    );
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        vector["expected"]["signingBytesUtf8"].as_str().unwrap(),
        "avalon_protocol::continuation::signing_bytes diverged from the shared vector"
    );
}

#[test]
fn websocket_interest_claim_signing_matches_shared_vectors() {
    let doc = load("websocket-interest-claim.json");
    let vector = &doc["vectors"][0];
    let input = &vector["input"];
    let bytes = interest_claim_signing_bytes(
        parse_uuid(input, "identityId"),
        parse_uuid(input, "signingKeyId"),
        ClaimedScope::Channel {
            channel_id: parse_uuid(&input["scope"], "channelId"),
        },
        input["baseUrl"].as_str().unwrap(),
        parse_uuid(input, "nonce"),
        parse_offset(input, "issuedAtUnixSeconds"),
        parse_offset(input, "expiresAtUnixSeconds"),
    );
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        vector["expected"]["signingBytesUtf8"].as_str().unwrap(),
        "avalon_protocol::interest_claim::signing_bytes diverged from the shared vector"
    );
}
