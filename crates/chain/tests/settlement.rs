//! Exercises `PostgresSettlementProvider::commit`/`verify`/`get_commitment`
//! against a real Postgres instance (issue #38 — event batching; issue #210
//! — the real Merkle root and Signed Tree Head `commit` now also produces).
//! Gated `--ignored` since it needs live infra — see `make test-live` /
//! `make start`; mirrors the pattern `crates/server/tests/*.rs` already
//! uses for its own live-only tests (see e.g. `crates/server/tests/history.rs`).
//!
//! Since issue #210, `commit` also needs `AVALON_SETTLEMENT_SIGNING_KEY` set
//! (see `.env.example`) — every test here goes through `test_pool()`, which
//! loads `.env` via `dotenvy` the same way `make test-live` expects.
//!
//! These tests exercise `avalon-chain` directly rather than through
//! `avalon-server`'s outbox/HTTP surface: `SettlementProvider` is `chain`'s
//! own public boundary, and batching is a property of `commit` itself, not
//! of any particular caller.

use avalon_chain::{PostgresSettlementProvider, SettlementError, SettlementProvider};
use avalon_protocol::events::{EventBatch, ProtocolEvent};
use avalon_protocol::ids::GlobalId;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

async fn test_pool() -> PgPool {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

fn sample_event(kind: &str) -> ProtocolEvent {
    let actor = Uuid::new_v4();
    ProtocolEvent {
        id: Uuid::new_v4(),
        kind: kind.to_string(),
        issuer: GlobalId::new("identity", &actor.to_string(), "self", "test_event"),
        subject: GlobalId::new("identity", &actor.to_string(), "self", "test_event"),
        payload: json!({ "note": format!("settlement test — {kind}") }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    }
}

fn sample_batch(event_count: usize) -> EventBatch {
    EventBatch {
        id: Uuid::new_v4(),
        events: (0..event_count)
            .map(|i| sample_event(&format!("test.event_{i}")))
            .collect(),
        created_at: OffsetDateTime::now_utc(),
    }
}

#[tokio::test]
#[ignore]
async fn commit_groups_events_under_one_batch() {
    let pool = test_pool().await;
    let chain = PostgresSettlementProvider::new(pool.clone(), "avalon-test");

    let batch = sample_batch(3);
    let commitment = chain.commit(&batch).await.expect("commit should succeed");
    assert_eq!(commitment.batch_id, batch.id);

    let rows = sqlx::query("SELECT DISTINCT batch_id FROM ledger_entries WHERE batch_id = $1")
        .bind(batch.id)
        .fetch_all(&pool)
        .await
        .expect("failed to query ledger_entries");
    assert_eq!(
        rows.len(),
        1,
        "all entries should share exactly one batch_id"
    );

    let entry_count: i64 =
        sqlx::query("SELECT count(*) AS c FROM ledger_entries WHERE batch_id = $1")
            .bind(batch.id)
            .fetch_one(&pool)
            .await
            .expect("failed to count ledger_entries")
            .try_get("c")
            .unwrap();
    assert_eq!(entry_count, 3);

    let batch_row_count: i64 =
        sqlx::query("SELECT count(*) AS c FROM ledger_batches WHERE batch_id = $1")
            .bind(batch.id)
            .fetch_one(&pool)
            .await
            .expect("failed to count ledger_batches")
            .try_get("c")
            .unwrap();
    assert_eq!(
        batch_row_count, 1,
        "exactly one ledger_batches row should exist"
    );
}

#[tokio::test]
#[ignore]
async fn get_commitment_returns_committed_batch() {
    let pool = test_pool().await;
    let chain = PostgresSettlementProvider::new(pool, "avalon-test");

    let batch = sample_batch(2);
    let commitment = chain.commit(&batch).await.expect("commit should succeed");

    let looked_up = chain
        .get_commitment(batch.id)
        .await
        .expect("get_commitment should find the committed batch, not BatchNotFound");
    assert_eq!(looked_up.batch_id, batch.id);
    assert_eq!(looked_up.proof, commitment.proof);
}

#[tokio::test]
#[ignore]
async fn get_commitment_of_unknown_batch_is_not_found() {
    let pool = test_pool().await;
    let chain = PostgresSettlementProvider::new(pool, "avalon-test");

    let result = chain.get_commitment(Uuid::new_v4()).await;
    assert!(matches!(result, Err(SettlementError::BatchNotFound)));
}

#[tokio::test]
#[ignore]
async fn verify_accepts_an_untampered_batch() {
    let pool = test_pool().await;
    let chain = PostgresSettlementProvider::new(pool, "avalon-test");

    let batch = sample_batch(3);
    let commitment = chain.commit(&batch).await.expect("commit should succeed");

    let verified = chain
        .verify(&commitment)
        .await
        .expect("verify should not error");
    assert!(verified, "an untampered batch should verify");
}

#[tokio::test]
#[ignore]
async fn verify_detects_tampered_batch_root() {
    let pool = test_pool().await;
    let chain = PostgresSettlementProvider::new(pool.clone(), "avalon-test");

    let batch = sample_batch(3);
    let commitment = chain.commit(&batch).await.expect("commit should succeed");

    // Mutate one entry's payload in place, directly via SQL — same style
    // `crates/server/tests/*.rs` uses to seed/mutate rows the provider
    // itself doesn't expose a write path for tampering with. This leaves
    // `entry_hash` stale relative to the (now tampered) content, exactly
    // what a real tamper attempt would look like.
    sqlx::query(
        "UPDATE ledger_entries SET payload = $1 WHERE batch_id = $2 AND seq = (SELECT min(seq) FROM ledger_entries WHERE batch_id = $2)",
    )
    .bind(json!({ "note": "tampered" }))
    .bind(batch.id)
    .execute(&pool)
    .await
    .expect("failed to tamper with ledger_entries");

    let verified = chain
        .verify(&commitment)
        .await
        .expect("verify should not error");
    assert!(!verified, "a tampered batch should fail verification");
}

#[tokio::test]
#[ignore]
async fn list_entries_reports_chain_intact_across_batch_boundaries() {
    let pool = test_pool().await;
    let chain = PostgresSettlementProvider::new(pool, "avalon-test");

    let first_batch = sample_batch(2);
    chain
        .commit(&first_batch)
        .await
        .expect("first commit should succeed");
    let second_batch = sample_batch(2);
    chain
        .commit(&second_batch)
        .await
        .expect("second commit should succeed");

    let entries = chain.list_entries().await.expect("failed to list entries");
    let relevant: Vec<_> = entries
        .iter()
        .filter(|e| e.batch_id == first_batch.id || e.batch_id == second_batch.id)
        .collect();
    assert_eq!(relevant.len(), 4);
    assert!(
        relevant.iter().all(|e| e.chain_intact),
        "the hash chain must stay intact across the boundary between two batches"
    );
}

#[tokio::test]
#[ignore]
async fn commit_of_an_empty_batch_is_rejected() {
    let pool = test_pool().await;
    let chain = PostgresSettlementProvider::new(pool, "avalon-test");

    let empty = EventBatch {
        id: Uuid::new_v4(),
        events: vec![],
        created_at: OffsetDateTime::now_utc(),
    };
    let result = chain.commit(&empty).await;
    assert!(result.is_err(), "an empty batch should never be committed");
}

// --- Real Merkle root + Signed Tree Head (issue #210) ---

#[tokio::test]
#[ignore]
async fn commit_produces_real_merkle_root_not_placeholder() {
    let pool = test_pool().await;
    let chain = PostgresSettlementProvider::new(pool.clone(), "avalon-test");

    let batch = sample_batch(3);
    let commitment = chain.commit(&batch).await.expect("commit should succeed");
    let claimed_root = String::from_utf8_lossy(&commitment.proof).to_string();

    let last_entry_hash: String = sqlx::query(
        "SELECT entry_hash FROM ledger_entries WHERE batch_id = $1 ORDER BY seq DESC LIMIT 1",
    )
    .bind(batch.id)
    .fetch_one(&pool)
    .await
    .expect("failed to read last entry")
    .try_get("entry_hash")
    .unwrap();

    assert_ne!(
        claimed_root, last_entry_hash,
        "batch_root must be a real Merkle root, not the old placeholder chain tip"
    );

    // It must actually equal the RFC 6962 MTH of the whole ledger up to this
    // batch's last_seq — not just "some value that happens to differ".
    let rows = sqlx::query("SELECT entry_hash FROM ledger_entries ORDER BY seq ASC")
        .fetch_all(&pool)
        .await
        .expect("failed to read ledger_entries");
    let hashes: Vec<String> = rows
        .iter()
        .map(|r| r.try_get::<String, _>("entry_hash").unwrap())
        .collect();
    let expected_root = hex::encode(
        avalon_chain::merkle::mth_of_hex_hashes(&hashes)
            .expect("stored entry_hash values should always be valid hex"),
    );
    assert_eq!(claimed_root, expected_root);

    // A Signed Tree Head must exist for this batch's tree_size too.
    let last_seq: i64 = sqlx::query("SELECT last_seq FROM ledger_batches WHERE batch_id = $1")
        .bind(batch.id)
        .fetch_one(&pool)
        .await
        .expect("failed to read ledger_batches")
        .try_get("last_seq")
        .unwrap();
    let sth_root: String =
        sqlx::query("SELECT root_hash FROM signed_tree_heads WHERE tree_size = $1")
            .bind(last_seq)
            .fetch_one(&pool)
            .await
            .expect("a signed_tree_heads row should exist for this batch's tree_size")
            .try_get("root_hash")
            .unwrap();
    assert_eq!(sth_root, claimed_root);
}

#[tokio::test]
#[ignore]
async fn verify_detects_entry_tampering_via_merkle_recomputation() {
    let pool = test_pool().await;
    let chain = PostgresSettlementProvider::new(pool.clone(), "avalon-test");

    let first_batch = sample_batch(2);
    chain
        .commit(&first_batch)
        .await
        .expect("first commit should succeed");
    let second_batch = sample_batch(2);
    let commitment = chain
        .commit(&second_batch)
        .await
        .expect("second commit should succeed");

    assert!(
        chain
            .verify(&commitment)
            .await
            .expect("verify should not error"),
        "an untampered second batch should verify"
    );

    // Tamper with an entry_hash from the FIRST batch — the second batch's
    // own entries and hash-chain replay are completely untouched, so a
    // purely batch-local check would miss this. The ledger-wide Merkle
    // recompute at the second batch's tree_size must still catch it, since
    // that first-batch entry is one of its leaves.
    sqlx::query(
        "UPDATE ledger_entries SET entry_hash = $1 WHERE batch_id = $2 AND seq = (SELECT min(seq) FROM ledger_entries WHERE batch_id = $2)",
    )
    .bind("0".repeat(64))
    .bind(first_batch.id)
    .execute(&pool)
    .await
    .expect("failed to tamper with entry_hash");

    let verified = chain
        .verify(&commitment)
        .await
        .expect("verify should not error");
    assert!(
        !verified,
        "tampering with any entry_hash in the ledger's tree, even outside this batch, must fail verification"
    );
}

// --- Genesis / network identity (issue #173) ---
//
// `chain_genesis` is a real singleton — at most one row per database — so
// these tests can't share the same `public` schema as the tests above (and
// each other) without racing on that row. Each test gets its own throwaway
// schema with just the one table `PostgresSettlementProvider::connect`
// actually touches, mirroring `0016_chain_genesis/up.sql`, then drops it —
// isolation without needing a second database or a full migration run.

async fn isolated_genesis_pool(schema: &str) -> PgPool {
    let pool = test_pool().await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP SCHEMA IF EXISTS {schema} CASCADE"
    )))
    .execute(&pool)
    .await
    .expect("failed to drop any stale test schema");
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&pool)
        .await
        .expect("failed to create test schema");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE TABLE {schema}.chain_genesis (
            id BOOLEAN PRIMARY KEY DEFAULT true CHECK (id),
            network_id TEXT NOT NULL,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now()
        )"
    )))
    .execute(&pool)
    .await
    .expect("failed to create test chain_genesis table");

    // `PostgresSettlementProvider` addresses `chain_genesis` unqualified, so
    // every connection this pool hands out needs this schema ahead of
    // `public` on its search_path.
    let owned_schema = schema.to_string();
    let search_path_pool = PgPoolOptions::new()
        .after_connect(move |conn, _meta| {
            let schema = owned_schema.clone();
            Box::pin(async move {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "SET search_path = {schema}, public"
                )))
                .execute(conn)
                .await?;
                Ok(())
            })
        })
        .connect_lazy_with((*pool.connect_options()).clone());
    search_path_pool
}

#[tokio::test]
#[ignore]
async fn connect_creates_genesis_on_an_empty_table() {
    let pool = isolated_genesis_pool("test_genesis_create").await;

    let chain = PostgresSettlementProvider::connect(pool.clone(), "avalon-dev-alpha")
        .await
        .expect("first connect should create genesis");
    assert_eq!(chain.network_id(), "avalon-dev-alpha");

    let stored = PostgresSettlementProvider::read_genesis_network_id(&pool)
        .await
        .expect("read should succeed")
        .expect("genesis row should now exist");
    assert_eq!(stored, "avalon-dev-alpha");
}

#[tokio::test]
#[ignore]
async fn connect_succeeds_when_network_id_matches_existing_genesis() {
    let pool = isolated_genesis_pool("test_genesis_match").await;

    PostgresSettlementProvider::connect(pool.clone(), "avalon-dev-beta")
        .await
        .expect("first connect should create genesis");

    let second = PostgresSettlementProvider::connect(pool, "avalon-dev-beta")
        .await
        .expect("reconnecting with the same network_id should succeed");
    assert_eq!(second.network_id(), "avalon-dev-beta");
}

#[tokio::test]
#[ignore]
async fn connect_fails_fast_on_network_id_mismatch() {
    let pool = isolated_genesis_pool("test_genesis_mismatch").await;

    PostgresSettlementProvider::connect(pool.clone(), "avalon-dev-gamma")
        .await
        .expect("first connect should create genesis");

    let result = PostgresSettlementProvider::connect(pool, "avalon-mainnet-1").await;
    assert!(
        result.is_err(),
        "a mismatched network_id must never produce a usable provider"
    );
}
