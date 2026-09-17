//! Exercises `PostgresSettlementProvider::prepare`/`finalize` (issue #531
//! — managed settlement hosting's two-phase remote-signing flow) against a
//! real Postgres instance. Gated `--ignored` since it needs live infra —
//! see `make test-live` / `make start`; same pattern
//! `crates/chain/tests/settlement.rs` already established for exercising
//! `avalon-chain` directly rather than through `avalon-server`'s HTTP
//! surface.
//!
//! `prepare` returns an unsigned preview; the "integrator" role in these
//! tests signs it with a plain `ed25519_dalek::SigningKey` generated in
//! the test itself (standing in for a real integrator's own settlement
//! key, which a managed host never holds) and hands the signature to
//! `finalize`.

use avalon_chain::{PostgresSettlementProvider, SettlementProvider};
use avalon_protocol::events::{EventBatch, ProtocolEvent};
use avalon_protocol::ids::GlobalId;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use std::sync::OnceLock;
use time::OffsetDateTime;
use tokio::sync::Mutex;
use uuid::Uuid;

async fn test_pool() -> PgPool {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

/// Same rationale as `crates/chain/tests/settlement.rs`'s own lock: these
/// tests (via `prepare`'s full-table read) share the real `ledger_entries`
/// table with every other live test in this crate.
fn ledger_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn sample_batch(kind: &str) -> EventBatch {
    let actor = Uuid::new_v4();
    EventBatch {
        id: Uuid::new_v4(),
        events: vec![ProtocolEvent {
            id: Uuid::new_v4(),
            kind: kind.to_string(),
            issuer: GlobalId::new("game", "managed-hosting-test", "self", "test_event"),
            subject: GlobalId::new("identity", &actor.to_string(), "self", "test_event"),
            payload: json!({ "note": format!("managed hosting test — {kind}") }),
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }],
        created_at: OffsetDateTime::now_utc(),
    }
}

#[tokio::test]
#[ignore]
async fn prepare_then_finalize_with_a_valid_signature_commits_the_batch() {
    let pool = test_pool().await;
    let _guard = ledger_test_lock().lock().await;
    let chain = PostgresSettlementProvider::new(pool.clone(), "avalon-test");

    let integrator_key = SigningKey::generate(&mut rand::rng());
    let batch = sample_batch("test.managed_hosting_ok");

    let preview = chain.prepare(&batch).await.expect("prepare failed");
    assert_eq!(preview.batch_id, batch.id);
    assert_eq!(preview.network_id, "avalon-test");

    // The integrator signs the preview locally — this signing key never
    // touches the chain crate/managed host at any point.
    let message = avalon_chain::sth::signing_message(
        preview.tree_size,
        &preview.root_hash,
        &preview.network_id,
        preview.created_at,
    );
    let signature = integrator_key.sign(&message);

    let commitment = chain
        .finalize(
            &batch,
            preview.created_at,
            "integrator-key-1",
            &hex::encode(signature.to_bytes()),
            &integrator_key.verifying_key(),
        )
        .await
        .expect("finalize should succeed with a valid signature");
    assert_eq!(commitment.batch_id, batch.id);

    let row = sqlx::query("SELECT event_id FROM ledger_entries WHERE event_id = $1")
        .bind(batch.events[0].id)
        .fetch_optional(&pool)
        .await
        .expect("query failed");
    assert!(
        row.is_some(),
        "finalize should have actually inserted the ledger entry"
    );

    let sth_row = sqlx::query(
        "SELECT signing_key_id FROM signed_tree_heads WHERE tree_size = $1 AND root_hash = $2",
    )
    .bind(preview.tree_size)
    .bind(&preview.root_hash)
    .fetch_one(&pool)
    .await
    .expect("STH row should exist");
    let signing_key_id: String = sth_row.try_get("signing_key_id").unwrap();
    assert_eq!(
        signing_key_id, "integrator-key-1",
        "the STH must be stamped with the caller's own signing_key_id, not a local one"
    );
}

#[tokio::test]
#[ignore]
async fn finalize_with_an_invalid_signature_is_rejected_and_inserts_nothing() {
    let pool = test_pool().await;
    let _guard = ledger_test_lock().lock().await;
    let chain = PostgresSettlementProvider::new(pool.clone(), "avalon-test");

    let integrator_key = SigningKey::generate(&mut rand::rng());
    let wrong_key = SigningKey::generate(&mut rand::rng());
    let batch = sample_batch("test.managed_hosting_bad_sig");

    let preview = chain.prepare(&batch).await.expect("prepare failed");

    // Signed with the WRONG key — `finalize` verifies against
    // `integrator_key.verifying_key()`, so this must fail.
    let message = avalon_chain::sth::signing_message(
        preview.tree_size,
        &preview.root_hash,
        &preview.network_id,
        preview.created_at,
    );
    let bad_signature = wrong_key.sign(&message);

    let result = chain
        .finalize(
            &batch,
            preview.created_at,
            "integrator-key-1",
            &hex::encode(bad_signature.to_bytes()),
            &integrator_key.verifying_key(),
        )
        .await;
    assert!(result.is_err(), "finalize must reject an invalid signature");

    let row = sqlx::query("SELECT event_id FROM ledger_entries WHERE event_id = $1")
        .bind(batch.events[0].id)
        .fetch_optional(&pool)
        .await
        .expect("query failed");
    assert!(
        row.is_none(),
        "a rejected finalize must roll back — the entry must never land"
    );
}

/// The whole point of "finalize recomputes fresh rather than trusting
/// `prepare`'s preview back": if the ledger's tip moves between `prepare`
/// and `finalize` (another batch commits in between), a signature over
/// the now-stale preview must fail against the freshly-recomputed tree
/// head, not be silently accepted against outdated values.
#[tokio::test]
#[ignore]
async fn finalize_after_the_tip_moved_since_prepare_is_rejected() {
    let pool = test_pool().await;
    let _guard = ledger_test_lock().lock().await;
    let chain = PostgresSettlementProvider::new(pool.clone(), "avalon-test");

    let integrator_key = SigningKey::generate(&mut rand::rng());
    let batch = sample_batch("test.managed_hosting_stale");

    let preview = chain.prepare(&batch).await.expect("prepare failed");
    let message = avalon_chain::sth::signing_message(
        preview.tree_size,
        &preview.root_hash,
        &preview.network_id,
        preview.created_at,
    );
    let signature = integrator_key.sign(&message);

    // Something else commits to the ledger in between — moving the tip
    // out from under this prepared preview.
    let other_batch = sample_batch("test.managed_hosting_stale_intervening");
    chain
        .commit(&other_batch)
        .await
        .expect("intervening commit failed");

    let result = chain
        .finalize(
            &batch,
            preview.created_at,
            "integrator-key-1",
            &hex::encode(signature.to_bytes()),
            &integrator_key.verifying_key(),
        )
        .await;
    assert!(
        result.is_err(),
        "finalize against a stale (tip-moved) preview must be rejected, not silently accepted \
         against outdated tree_size/root_hash"
    );
}

/// Issue #564: a replayed `finalize` for the same `batch_id` (a client
/// retrying after a timeout without knowing whether its first attempt
/// landed) must return the *existing* commitment, not error or insert a
/// second copy of the entry.
#[tokio::test]
#[ignore]
async fn finalize_is_idempotent_on_a_replayed_batch_id() {
    let pool = test_pool().await;
    let _guard = ledger_test_lock().lock().await;
    let chain = PostgresSettlementProvider::new(pool.clone(), "avalon-test");

    let integrator_key = SigningKey::generate(&mut rand::rng());
    let batch = sample_batch("test.managed_hosting_idempotent");

    let preview = chain.prepare(&batch).await.expect("prepare failed");
    let message = avalon_chain::sth::signing_message(
        preview.tree_size,
        &preview.root_hash,
        &preview.network_id,
        preview.created_at,
    );
    let signature = integrator_key.sign(&message);

    let first = chain
        .finalize(
            &batch,
            preview.created_at,
            "integrator-key-1",
            &hex::encode(signature.to_bytes()),
            &integrator_key.verifying_key(),
        )
        .await
        .expect("first finalize should succeed");

    // Replay the exact same finalize call — same batch, same signature —
    // simulating a client that never saw the first response.
    let second = chain
        .finalize(
            &batch,
            preview.created_at,
            "integrator-key-1",
            &hex::encode(signature.to_bytes()),
            &integrator_key.verifying_key(),
        )
        .await
        .expect("replayed finalize must return the existing commitment, not error");

    assert_eq!(
        first.batch_id, second.batch_id,
        "must be the same commitment"
    );
    assert_eq!(first.proof, second.proof);
    assert_eq!(
        first.committed_at, second.committed_at,
        "a replay must echo the same caller-signed created_at back, not a DB-sourced timestamp"
    );

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ledger_entries WHERE event_id = $1")
        .bind(batch.events[0].id)
        .fetch_one(&pool)
        .await
        .expect("query failed");
    assert_eq!(count, 1, "the replay must not have inserted a second entry");
}
