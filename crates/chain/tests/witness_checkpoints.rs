//! Exercises `avalon_chain::mirror::witness_checkpoint_for`/
//! `record_witness_checkpoint` against a real Postgres instance — the
//! `witness_checkpoints` table migration (`0077_witness_checkpoints`) plus
//! the read/write pair a cosigning decision uses to track this node's own
//! last-cosigned checkpoint per network/shard. Gated `--ignored`, same
//! convention as `crates/chain/tests/witness_cosignatures.rs`.
//!
//! Uses its own isolated `network_id` per test so this file never collides
//! with another test file's fixtures against the same shared Postgres.

use avalon_chain::mirror::{record_witness_checkpoint, witness_checkpoint_for};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

fn fresh_network_id() -> String {
    format!("witness-checkpoint-test-{}", Uuid::new_v4())
}

async fn test_pool() -> PgPool {
    avalon_devenv::load();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

fn root(byte: u8) -> String {
    hex::encode([byte; 32])
}

#[tokio::test]
#[ignore]
async fn no_checkpoint_yet_is_none() {
    let pool = test_pool().await;
    let network_id = fresh_network_id();

    let found = witness_checkpoint_for(&pool, &network_id, "core")
        .await
        .expect("query should succeed");
    assert!(found.is_none());
}

#[tokio::test]
#[ignore]
async fn record_then_read_back_round_trips() {
    let pool = test_pool().await;
    let network_id = fresh_network_id();

    record_witness_checkpoint(&pool, &network_id, "core", 5, &root(1), "witness-1")
        .await
        .expect("record should succeed");

    let found = witness_checkpoint_for(&pool, &network_id, "core")
        .await
        .expect("query should succeed")
        .expect("checkpoint should now exist");
    assert_eq!(found.tree_size, 5);
    assert_eq!(found.root_hash, root(1));
    assert_eq!(found.witness_key_id, "witness-1");
}

#[tokio::test]
#[ignore]
async fn advancing_to_a_larger_tree_size_overwrites() {
    let pool = test_pool().await;
    let network_id = fresh_network_id();

    record_witness_checkpoint(&pool, &network_id, "core", 5, &root(1), "witness-1")
        .await
        .unwrap();
    record_witness_checkpoint(&pool, &network_id, "core", 9, &root(2), "witness-1")
        .await
        .unwrap();

    let found = witness_checkpoint_for(&pool, &network_id, "core")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.tree_size, 9);
    assert_eq!(found.root_hash, root(2));
}

#[tokio::test]
#[ignore]
async fn a_write_at_or_below_the_current_tree_size_never_regresses_the_checkpoint() {
    let pool = test_pool().await;
    let network_id = fresh_network_id();

    record_witness_checkpoint(&pool, &network_id, "core", 9, &root(2), "witness-1")
        .await
        .unwrap();
    // A stale/reordered write for a smaller or equal tree_size must never
    // clobber a more advanced checkpoint already on record.
    record_witness_checkpoint(&pool, &network_id, "core", 5, &root(1), "witness-1")
        .await
        .unwrap();
    record_witness_checkpoint(&pool, &network_id, "core", 9, &root(99), "witness-1")
        .await
        .unwrap();

    let found = witness_checkpoint_for(&pool, &network_id, "core")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.tree_size, 9);
    assert_eq!(found.root_hash, root(2));
}

#[tokio::test]
#[ignore]
async fn different_shards_of_the_same_network_have_independent_checkpoints() {
    let pool = test_pool().await;
    let network_id = fresh_network_id();

    record_witness_checkpoint(&pool, &network_id, "shard-a", 3, &root(1), "witness-1")
        .await
        .unwrap();
    record_witness_checkpoint(&pool, &network_id, "shard-b", 7, &root(2), "witness-1")
        .await
        .unwrap();

    let a = witness_checkpoint_for(&pool, &network_id, "shard-a")
        .await
        .unwrap()
        .unwrap();
    let b = witness_checkpoint_for(&pool, &network_id, "shard-b")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(a.tree_size, 3);
    assert_eq!(b.tree_size, 7);
}
