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

/// Serializes every test below that shares the real `ledger_entries` table
/// (as opposed to `isolated_genesis_pool`'s own per-test schema) — several
/// assertions read the whole table (a full-ledger Merkle root, `list_entries`,
/// prune counts), so two of these running concurrently under cargo's default
/// multi-threaded test runner corrupt each other's expectations.
fn ledger_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
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
    let _guard = ledger_test_lock().lock().await;
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
    let _guard = ledger_test_lock().lock().await;
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
    let _guard = ledger_test_lock().lock().await;
    let chain = PostgresSettlementProvider::new(pool, "avalon-test");

    let result = chain.get_commitment(Uuid::new_v4()).await;
    assert!(matches!(result, Err(SettlementError::BatchNotFound)));
}

#[tokio::test]
#[ignore]
async fn verify_accepts_an_untampered_batch() {
    let pool = test_pool().await;
    let _guard = ledger_test_lock().lock().await;
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
    let _guard = ledger_test_lock().lock().await;
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
    let _guard = ledger_test_lock().lock().await;
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
    let _guard = ledger_test_lock().lock().await;
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
    let _guard = ledger_test_lock().lock().await;
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
    // batch's tree_size — not just "some value that happens to differ".
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

    // A Signed Tree Head must exist for this batch's tree_size — a leaf
    // *count* (`hashes.len()`), never `ledger_batches.last_seq`: `seq` can
    // have gaps (see postgres.rs's module doc), so the two diverge once any
    // commit anywhere has ever rolled back.
    let tree_size = hashes.len() as i64;
    let sth_root: String =
        sqlx::query("SELECT root_hash FROM signed_tree_heads WHERE tree_size = $1")
            .bind(tree_size)
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
    let _guard = ledger_test_lock().lock().await;
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

// --- Node-tiered retention / payload pruning (issue #208) ---

/// Backdates `batch`'s entries' `committed_at` directly via SQL, the same
/// tamper/seed style the tests above use for state the provider itself has
/// no write path for — `commit` always stamps `now()`, so a pruning test
/// needs a way to simulate "this batch is old" without waiting.
async fn backdate_batch(pool: &PgPool, batch_id: Uuid, committed_at: OffsetDateTime) {
    sqlx::query("UPDATE ledger_entries SET committed_at = $1 WHERE batch_id = $2")
        .bind(committed_at)
        .bind(batch_id)
        .execute(pool)
        .await
        .expect("failed to backdate ledger_entries for pruning test");
}

#[tokio::test]
#[ignore]
async fn prune_payloads_older_than_only_nulls_payload_of_entries_before_cutoff() {
    let pool = test_pool().await;
    let _guard = ledger_test_lock().lock().await;
    let chain = PostgresSettlementProvider::new(pool.clone(), "avalon-test");

    let old_batch = sample_batch(2);
    chain
        .commit(&old_batch)
        .await
        .expect("commit should succeed");
    backdate_batch(
        &pool,
        old_batch.id,
        OffsetDateTime::now_utc() - time::Duration::days(400),
    )
    .await;

    let new_batch = sample_batch(2);
    chain
        .commit(&new_batch)
        .await
        .expect("commit should succeed");

    let cutoff = OffsetDateTime::now_utc() - time::Duration::days(30);
    let report = chain
        .prune_payloads_older_than(cutoff)
        .await
        .expect("pruning should not error");
    assert_eq!(
        report.pruned_count, 2,
        "only the old batch's 2 entries should be pruned"
    );

    let entries = chain.list_entries().await.expect("list_entries failed");
    let old_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.batch_id == old_batch.id)
        .collect();
    let new_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.batch_id == new_batch.id)
        .collect();

    assert!(
        old_entries
            .iter()
            .all(|e| e.payload.is_none() && e.payload_pruned),
        "old batch's entries should have had their payload pruned"
    );
    assert!(
        new_entries
            .iter()
            .all(|e| e.payload.is_some() && !e.payload_pruned),
        "new batch's entries must be untouched by pruning"
    );

    // The hash-chain link and Merkle-relevant columns must survive pruning
    // intact — a pruned entry with a still-intact link reports
    // `chain_intact: true` (content just isn't independently re-checkable
    // any more), never a false "broken chain".
    assert!(
        old_entries.iter().all(|e| e.chain_intact),
        "pruning must never make an entry's link/structure report as broken"
    );
}

#[tokio::test]
#[ignore]
async fn prune_payloads_older_than_is_idempotent() {
    let pool = test_pool().await;
    let _guard = ledger_test_lock().lock().await;
    let chain = PostgresSettlementProvider::new(pool.clone(), "avalon-test");

    let batch = sample_batch(3);
    chain.commit(&batch).await.expect("commit should succeed");
    backdate_batch(
        &pool,
        batch.id,
        OffsetDateTime::now_utc() - time::Duration::days(400),
    )
    .await;

    let cutoff = OffsetDateTime::now_utc() - time::Duration::days(30);
    let first = chain
        .prune_payloads_older_than(cutoff)
        .await
        .expect("first prune should not error");
    assert_eq!(first.pruned_count, 3);

    let second = chain
        .prune_payloads_older_than(cutoff)
        .await
        .expect("second prune should not error");
    assert_eq!(
        second.pruned_count, 0,
        "already-pruned rows must not be re-counted on a second pass"
    );
}

#[tokio::test]
#[ignore]
async fn verify_still_succeeds_via_the_merkle_check_after_a_batchs_payloads_are_pruned() {
    let pool = test_pool().await;
    let _guard = ledger_test_lock().lock().await;
    let chain = PostgresSettlementProvider::new(pool.clone(), "avalon-test");

    let batch = sample_batch(3);
    let commitment = chain.commit(&batch).await.expect("commit should succeed");
    backdate_batch(
        &pool,
        batch.id,
        OffsetDateTime::now_utc() - time::Duration::days(400),
    )
    .await;

    let cutoff = OffsetDateTime::now_utc() - time::Duration::days(30);
    chain
        .prune_payloads_older_than(cutoff)
        .await
        .expect("pruning should not error");

    // This is the ticket's central invariant: pruning payload never makes
    // a settled fact unverifiable. The hash-chain replay check inside
    // `verify` can no longer run (it needs the now-pruned payload), but
    // the independent Merkle check — built entirely from `entry_hash`,
    // never `payload` — still can, and `verify` must still report `true`.
    let verified = chain
        .verify(&commitment)
        .await
        .expect("verify should not error even once payloads are pruned");
    assert!(
        verified,
        "a batch must remain verifiable via the Merkle check after its payloads are pruned"
    );
}

/// Regression test for the exact bug this fix closes: pruning one entry in
/// a batch must never disable tamper detection for the batch's *other*
/// entries. Directly nulls one entry's payload (simulating pruning without
/// going through the age-based `prune_payloads_older_than` cutoff, so the
/// scenario is deterministic) and separately mutates a *different* entry's
/// `kind` column without touching its `entry_hash` — a classic tamper that
/// leaves the stored hash stale relative to the content. Before this fix,
/// the presence of the first entry's missing payload caused the whole
/// batch's hash-chain replay to be skipped, so the second entry's tamper
/// went undetected and `verify` incorrectly returned `true`.
#[tokio::test]
#[ignore]
async fn verify_still_detects_tampering_in_a_batch_with_a_separately_pruned_entry() {
    let pool = test_pool().await;
    let _guard = ledger_test_lock().lock().await;
    let chain = PostgresSettlementProvider::new(pool.clone(), "avalon-test");

    let batch = sample_batch(2);
    let commitment = chain.commit(&batch).await.expect("commit should succeed");

    let seqs: Vec<i64> =
        sqlx::query("SELECT seq FROM ledger_entries WHERE batch_id = $1 ORDER BY seq ASC")
            .bind(batch.id)
            .fetch_all(&pool)
            .await
            .expect("failed to read seqs")
            .iter()
            .map(|row| row.try_get::<i64, _>("seq").unwrap())
            .collect();
    assert_eq!(seqs.len(), 2, "expected exactly two entries in this batch");
    let (pruned_seq, tampered_seq) = (seqs[0], seqs[1]);

    // Simulate pruning of the first entry only — same end state
    // `prune_payloads_older_than` would leave it in.
    sqlx::query(
        "UPDATE ledger_entries SET payload = NULL, payload_pruned_at = now() WHERE seq = $1",
    )
    .bind(pruned_seq)
    .execute(&pool)
    .await
    .expect("failed to simulate pruning the first entry");

    // Tamper with the second entry's content without touching its stored
    // entry_hash — the classic case `verify` exists to catch.
    sqlx::query("UPDATE ledger_entries SET kind = 'tampered.kind' WHERE seq = $1")
        .bind(tampered_seq)
        .execute(&pool)
        .await
        .expect("failed to simulate tampering the second entry");

    let verified = chain
        .verify(&commitment)
        .await
        .expect("verify should not error");
    assert!(
        !verified,
        "a genuinely tampered entry must still be caught even when a *different* \
         entry in the same batch has had its payload pruned"
    );
}

#[tokio::test]
#[ignore]
async fn prunable_entry_count_matches_what_pruning_actually_prunes() {
    let pool = test_pool().await;
    let _guard = ledger_test_lock().lock().await;
    let chain = PostgresSettlementProvider::new(pool.clone(), "avalon-test");

    let batch = sample_batch(4);
    chain.commit(&batch).await.expect("commit should succeed");
    backdate_batch(
        &pool,
        batch.id,
        OffsetDateTime::now_utc() - time::Duration::days(400),
    )
    .await;

    let cutoff = OffsetDateTime::now_utc() - time::Duration::days(30);
    let dry_run_count = chain
        .prunable_entry_count(cutoff)
        .await
        .expect("dry-run count should not error");
    assert_eq!(dry_run_count, 4);

    let report = chain
        .prune_payloads_older_than(cutoff)
        .await
        .expect("pruning should not error");
    assert_eq!(report.pruned_count, dry_run_count);
}
