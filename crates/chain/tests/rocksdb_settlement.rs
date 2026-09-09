#![cfg(feature = "rocksdb-backend")]
//! Exercises `RocksDbSettlementProvider` (issue #178) — mirrors
//! `crates/chain/tests/settlement.rs`'s Postgres coverage. Unlike that
//! file, none of this needs `--ignored`/live infra: a temp directory is all
//! this backend needs, so these run for real in this sandbox.
//!
//! The whole file compiles to nothing without `--features rocksdb-backend`
//! (see the `#![cfg(...)]` above) — `cargo test -p avalon-chain` (default)
//! never touches RocksDB at all.

use avalon_chain::{
    PostgresSettlementProvider, RocksDbSettlementProvider, SettlementError, SettlementProvider,
};
use avalon_protocol::events::{EventBatch, ProtocolEvent};
use avalon_protocol::ids::GlobalId;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use time::OffsetDateTime;
use uuid::Uuid;

fn sample_event(kind: &str) -> ProtocolEvent {
    let actor = Uuid::new_v4();
    ProtocolEvent {
        id: Uuid::new_v4(),
        kind: kind.to_string(),
        issuer: GlobalId::new("identity", &actor.to_string(), "self", "test_event"),
        subject: GlobalId::new("identity", &actor.to_string(), "self", "test_event"),
        payload: json!({ "note": format!("rocksdb settlement test — {kind}") }),
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
async fn commit_groups_events_under_one_batch() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let chain = RocksDbSettlementProvider::connect(dir.path(), "avalon-test")
        .expect("connect should create genesis on an empty database");

    let batch = sample_batch(3);
    let commitment = chain.commit(&batch).await.expect("commit should succeed");
    assert_eq!(commitment.batch_id, batch.id);

    let entries = chain.list_entries().expect("failed to list entries");
    let relevant: Vec<_> = entries.iter().filter(|e| e.batch_id == batch.id).collect();
    assert_eq!(
        relevant.len(),
        3,
        "all entries should share exactly one batch_id"
    );

    let batches = chain.list_batches().expect("failed to list batches");
    assert_eq!(
        batches.iter().filter(|b| b.batch_id == batch.id).count(),
        1,
        "exactly one batch record should exist"
    );
}

#[tokio::test]
async fn get_commitment_returns_committed_batch() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let chain = RocksDbSettlementProvider::connect(dir.path(), "avalon-test").unwrap();

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
async fn get_commitment_of_unknown_batch_is_not_found() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let chain = RocksDbSettlementProvider::connect(dir.path(), "avalon-test").unwrap();

    let result = chain.get_commitment(Uuid::new_v4()).await;
    assert!(matches!(result, Err(SettlementError::BatchNotFound)));
}

#[tokio::test]
async fn verify_accepts_an_untampered_batch() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let chain = RocksDbSettlementProvider::connect(dir.path(), "avalon-test").unwrap();

    let batch = sample_batch(3);
    let commitment = chain.commit(&batch).await.expect("commit should succeed");

    let verified = chain
        .verify(&commitment)
        .await
        .expect("verify should not error");
    assert!(verified, "an untampered batch should verify");
}

#[tokio::test]
async fn verify_detects_tampered_batch_root() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let path = dir.path().to_path_buf();

    let commitment = {
        let chain = RocksDbSettlementProvider::connect(&path, "avalon-test").unwrap();
        let batch = sample_batch(3);
        chain.commit(&batch).await.expect("commit should succeed")
        // `chain` drops here, releasing RocksDB's exclusive lock on `path`
        // before the raw reopen below.
    };

    // Tamper with the *first* entry's payload directly at the storage
    // layer — same idea as `settlement.rs`'s raw SQL `UPDATE` against
    // Postgres. `StoredEntry`'s JSON shape is walked generically here
    // rather than importing the (private) struct, which keeps this test
    // honest about only depending on the wire format, not internals.
    {
        let opts = rocksdb::Options::default();
        let db = rocksdb::DB::open_cf(&opts, &path, ["meta", "entries", "batches"])
            .expect("failed to reopen rocksdb for tampering");
        let cf = db.cf_handle("entries").expect("entries CF should exist");
        let (key, value) = db
            .iterator_cf(cf, rocksdb::IteratorMode::Start)
            .next()
            .expect("at least one entry should exist")
            .expect("iterator should not error");
        let mut decoded: serde_json::Value =
            serde_json::from_slice(&value).expect("stored entry should be valid JSON");
        decoded["event"]["payload"] = json!({ "note": "tampered" });
        db.put_cf(
            cf,
            &key,
            serde_json::to_vec(&decoded).expect("re-encoding should succeed"),
        )
        .expect("tampering write should succeed");
    }

    let chain = RocksDbSettlementProvider::connect(&path, "avalon-test").unwrap();
    let verified = chain
        .verify(&commitment)
        .await
        .expect("verify should not error");
    assert!(!verified, "a tampered batch should fail verification");
}

#[tokio::test]
async fn list_entries_reports_chain_intact_across_batch_boundaries() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let chain = RocksDbSettlementProvider::connect(dir.path(), "avalon-test").unwrap();

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

    let entries = chain.list_entries().expect("failed to list entries");
    assert_eq!(entries.len(), 4);
    assert!(
        entries.iter().all(|e| e.chain_intact),
        "the hash chain must stay intact across the boundary between two batches"
    );
}

#[tokio::test]
async fn commit_of_an_empty_batch_is_rejected() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let chain = RocksDbSettlementProvider::connect(dir.path(), "avalon-test").unwrap();

    let empty = EventBatch {
        id: Uuid::new_v4(),
        events: vec![],
        created_at: OffsetDateTime::now_utc(),
    };
    let result = chain.commit(&empty).await;
    assert!(result.is_err(), "an empty batch should never be committed");
}

// --- Genesis (issue #178, mirrors #173's Postgres genesis behavior) ---

#[test]
fn connect_creates_genesis_on_an_empty_database() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let chain = RocksDbSettlementProvider::connect(dir.path(), "avalon-dev-alpha")
        .expect("first connect should create genesis");
    assert_eq!(chain.network_id(), "avalon-dev-alpha");
}

#[test]
fn connect_succeeds_when_network_id_matches_existing_genesis() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    {
        RocksDbSettlementProvider::connect(dir.path(), "avalon-dev-beta")
            .expect("first connect should create genesis");
    }
    let reopened = RocksDbSettlementProvider::connect(dir.path(), "avalon-dev-beta")
        .expect("reconnecting with the same network_id should succeed");
    assert_eq!(reopened.network_id(), "avalon-dev-beta");
}

#[test]
fn connect_fails_fast_on_network_id_mismatch() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    {
        RocksDbSettlementProvider::connect(dir.path(), "avalon-dev-gamma")
            .expect("first connect should create genesis");
    }
    let result = RocksDbSettlementProvider::connect(dir.path(), "avalon-mainnet-1");
    assert!(
        result.is_err(),
        "a mismatched network_id must never produce a usable provider"
    );
}

// --- Cross-backend hash equivalence ---

async fn postgres_test_pool() -> sqlx::PgPool {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

/// Proves the two backends aren't just individually correct but produce
/// *the same ledger*: identical event content committed from the same
/// starting state hashes identically regardless of which `SettlementProvider`
/// did the committing. This is what makes "the ledger" one well-defined
/// thing independent of its storage engine. `--ignored`: the Postgres side
/// needs live infra; the RocksDB side alone is exercised by every other
/// test in this file.
#[tokio::test]
#[ignore]
async fn postgres_and_rocksdb_backends_compute_identical_hashes() {
    let batch = sample_batch(3);

    let pg_pool = postgres_test_pool().await;
    let pg_chain = PostgresSettlementProvider::new(pg_pool, "avalon-test");
    let pg_commitment = pg_chain
        .commit(&batch)
        .await
        .expect("postgres commit should succeed");

    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let rocks_chain = RocksDbSettlementProvider::connect(dir.path(), "avalon-test").unwrap();
    let rocks_commitment = rocks_chain
        .commit(&batch)
        .await
        .expect("rocksdb commit should succeed");

    assert_eq!(
        pg_commitment.proof, rocks_commitment.proof,
        "both backends must compute the same batch root for identical input"
    );
}
