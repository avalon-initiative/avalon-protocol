//! The headline security property `crates/server/src/cross_shard_fetch.rs`'s
//! own module doc comment claims: a remote node handing back a genuine,
//! signature-verified STH and a genuine, structurally-valid inclusion
//! proof for a *real* entry, but a *different* (forged) payload than the
//! one that entry's hash actually covers, must still be rejected.
//! `crates/server/tests/cross_shard_fetch.rs`'s live tests exercise the
//! happy path and the two trust-anchor fail-closed cases against a real,
//! honest server — this file is the one adversarial case a real server
//! would never actually produce on its own, so it's exercised here against
//! a mocked remote node (`wiremock`, already used the same way by
//! `crates/sdk`) instead.
//!
//! Self-contained: no real Postgres, no `make start`, not gated `--ignored`
//! — `resolve_shard_verify_keys_from_db` returns immediately without a
//! query for a `"core"`-shaped `shard_id` (no `:`, so it never resolves
//! any DB-backed key), so a lazily-connecting pool that's never actually
//! asked to run a query is enough.

use std::collections::HashMap;

use ed25519_dalek::SigningKey;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use time::OffsetDateTime;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use avalon_chain::{hash_entry, merkle, EntryContent};
use avalon_protocol::sth::sign_tree_head;
use avalon_server::cross_shard_fetch::{fetch_verified_entries, CrossShardFetchError};

const NETWORK_ID: &str = "avalon-tamper-test";

fn lazy_pool() -> sqlx::PgPool {
    // Never actually connects — `"core"` never triggers a real query (see
    // this file's own module doc comment) — so an address that resolves
    // but refuses connections is fine; nothing here ever dials it.
    PgPoolOptions::new()
        .connect_lazy("postgres://user:pass@127.0.0.1:1/nonexistent")
        .expect("connect_lazy should never fail just building the pool")
}

/// A real, self-consistent `identity.signing_key_added` entry's content —
/// what an honest node would actually have committed.
struct RealEntry {
    prev_hash: String,
    event_id: Uuid,
    kind: String,
    issuer: String,
    subject: String,
    payload: serde_json::Value,
    timestamp: OffsetDateTime,
    version: i32,
    entry_hash: String,
}

fn real_entry() -> RealEntry {
    let identity_id = Uuid::new_v4();
    let subject = format!("identity:{identity_id}:self:signing_key_added");
    let payload = json!({
        "identity_id": identity_id,
        "signing_key_id": Uuid::new_v4(),
        "public_key": "AAAA",
    });
    let timestamp = OffsetDateTime::from_unix_timestamp(1_758_000_000).unwrap();
    let prev_hash = "0".repeat(64);
    let event_id = Uuid::new_v4();
    let kind = "identity.signing_key_added".to_string();
    let issuer = subject.clone();
    let version = 1;

    let entry_hash = hash_entry(
        NETWORK_ID,
        &prev_hash,
        &EntryContent {
            event_id,
            kind: &kind,
            issuer: &issuer,
            subject: &subject,
            payload: &payload,
            timestamp,
            version,
        },
    );

    RealEntry {
        prev_hash,
        event_id,
        kind,
        issuer,
        subject,
        payload,
        timestamp,
        version,
        entry_hash,
    }
}

/// Wires up a mock remote node serving a genuinely signed STH plus a
/// genuinely valid single-leaf inclusion proof for `real.entry_hash` — but
/// `served_payload` (the `GET /ledger/entries` response body) in place of
/// `real.payload`, so a caller comparing (real entry_hash) against
/// (recomputed hash of `served_payload`) should never see them match
/// unless `served_payload == real.payload`.
async fn mock_remote_node(
    real: &RealEntry,
    served_payload: &serde_json::Value,
) -> (MockServer, SigningKey) {
    let signing_key = SigningKey::generate(&mut rand::rng());
    let root = merkle::mth_of_hex_hashes(std::slice::from_ref(&real.entry_hash))
        .expect("single valid hex hash should always produce a root");
    let root_hash = hex::encode(root);
    let proof = merkle::inclusion_proof_of_hex_hashes(0, std::slice::from_ref(&real.entry_hash))
        .expect("single-leaf inclusion proof should always succeed");
    assert!(
        proof.is_empty(),
        "a single-leaf tree's own inclusion proof has no audit-path nodes"
    );

    let sth = sign_tree_head(
        &signing_key,
        "test-key",
        1,
        &root_hash,
        NETWORK_ID,
        OffsetDateTime::now_utc(),
    );

    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/ledger/sth/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tree_size": sth.tree_size,
            "root_hash": sth.root_hash,
            "network_id": sth.network_id,
            "signing_key_id": sth.signing_key_id,
            "signature": sth.signature,
            "created_at": sth.created_at.format(&time::format_description::well_known::Rfc3339).unwrap(),
            "protocol_version": "test",
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/ledger/entries"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "seq": 1,
            "event_id": real.event_id,
            "kind": real.kind,
            "issuer": real.issuer,
            "subject": real.subject,
            "payload": served_payload,
            "payload_pruned": false,
            "version": real.version,
            "event_timestamp": real.timestamp.format(&time::format_description::well_known::Rfc3339).unwrap(),
            "prev_hash": real.prev_hash,
            "entry_hash": real.entry_hash,
            "batch_id": Uuid::new_v4(),
        }])))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/ledger/proof/inclusion"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "seq": 1,
            "tree_size": 1,
            "leaf_index": 0,
            "leaf_hash": real.entry_hash,
            "root_hash": root_hash,
            "proof": Vec::<String>::new(),
        })))
        .mount(&server)
        .await;

    (server, signing_key)
}

/// The real baseline: an honestly-served payload (matching what the entry
/// hash actually covers) verifies successfully — proves the mock harness
/// itself is correct before the adversarial case below relies on it.
#[tokio::test]
async fn an_honestly_served_payload_verifies() {
    let real = real_entry();
    let (server, signing_key) = mock_remote_node(&real, &real.payload).await;
    let verify_keys = HashMap::from([("core".to_string(), signing_key.verifying_key())]);

    let entries = fetch_verified_entries(
        &lazy_pool(),
        NETWORK_ID,
        "core",
        &server.uri(),
        &real.subject,
        &verify_keys,
        &[],
        &[],
    )
    .await
    .expect("an honestly-served payload should verify");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].payload, real.payload);
}

/// The headline case: a genuinely signed STH and a genuinely valid
/// inclusion proof for the *real* entry, served alongside a *forged*
/// payload — must be rejected, not silently accepted just because steps
/// 1-3 of the trust chain (signature, proof structure, root match) all
/// pass on their own.
#[tokio::test]
async fn a_forged_payload_alongside_a_valid_unrelated_proof_is_rejected() {
    let real = real_entry();
    let forged_payload = json!({
        "identity_id": real.payload["identity_id"],
        // A different signing_key_id than what entry_hash was actually
        // computed over — exactly the kind of substitution that would let
        // an attacker redirect trust to a key they control, if this went
        // undetected.
        "signing_key_id": Uuid::new_v4(),
        "public_key": "AAAA",
    });
    assert_ne!(forged_payload, real.payload);

    let (server, signing_key) = mock_remote_node(&real, &forged_payload).await;
    let verify_keys = HashMap::from([("core".to_string(), signing_key.verifying_key())]);

    let result = fetch_verified_entries(
        &lazy_pool(),
        NETWORK_ID,
        "core",
        &server.uri(),
        &real.subject,
        &verify_keys,
        &[],
        &[],
    )
    .await;

    assert!(matches!(
        result,
        Err(CrossShardFetchError::EntryHashMismatch)
    ));
}
