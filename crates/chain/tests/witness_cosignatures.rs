//! Exercises `PostgresSettlementProvider::store_witness_cosignature`/
//! `list_witness_cosignatures`/`cosigned_tree_head_at` against a
//! real Postgres instance — the `witness_cosignatures` table migration
//! (`0073_witness_cosignatures`) plus the read/write pair built on top of
//! it. Gated `--ignored`, same convention as `crates/chain/tests/settlement.rs`.
//!
//! Uses its own isolated `network_id` per test (never `"avalon-test"`) so
//! this file never needs `ledger_test_lock` or a real `commit` — a
//! directly-inserted `signed_tree_heads` row is all `cosigned_tree_head_at`
//! needs to assemble a `CosignedTreeHead` from.

use avalon_chain::PostgresSettlementProvider;
use avalon_protocol::cosigned_sth::verify_cosigned_tree_head;
use avalon_protocol::witness::{sign_witness_cosignature, WitnessCosignature};
use ed25519_dalek::SigningKey;
use rand::RngExt;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

/// `signed_tree_heads.tree_size` is a global primary key (one row per
/// tree_size across every network_id sharing this Postgres instance), not
/// scoped per network — so a live test needs a `tree_size` that's
/// vanishingly unlikely to collide with a real ledger's, rather than a
/// small fixed constant.
fn fresh_test_tree_size() -> i64 {
    rand::rng().random_range(1_000_000_000..2_000_000_000)
}

async fn test_pool() -> PgPool {
    avalon_devenv::load();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

async fn insert_test_sth(pool: &PgPool, network_id: &str, tree_size: i64, root_hash: &str) {
    sqlx::query(
        r#"
        INSERT INTO signed_tree_heads (tree_size, root_hash, network_id, signing_key_id, signature, created_at)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(tree_size)
    .bind(root_hash)
    .bind(network_id)
    .bind("test-key")
    .bind("00".repeat(64))
    .bind(OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1_800_000_000))
    .execute(pool)
    .await
    .expect("failed to insert a fixture signed_tree_heads row");
}

#[tokio::test]
#[ignore]
async fn store_then_read_back_assembles_a_verifiable_cosigned_head() {
    let pool = test_pool().await;
    let network_id = format!("avalon-witness-cosign-live-test-{}", Uuid::new_v4());
    let chain = PostgresSettlementProvider::new(pool.clone(), network_id.clone());

    let tree_size = fresh_test_tree_size();
    let root_hash = "ab".repeat(32);
    insert_test_sth(&pool, &network_id, tree_size, &root_hash).await;

    let author_created_at = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1_800_000_000);
    let observed_at = author_created_at + time::Duration::seconds(5);

    let witness_key = SigningKey::generate(&mut rand::rng());
    let cosig = sign_witness_cosignature(
        &witness_key,
        "witness-live-1",
        tree_size,
        &root_hash,
        &network_id,
        author_created_at,
        observed_at,
    );
    chain
        .store_witness_cosignature("core", &cosig)
        .await
        .expect("storing a fresh cosignature should succeed");

    // Idempotent replay of the exact same cosignature is a no-op.
    chain
        .store_witness_cosignature("core", &cosig)
        .await
        .expect("replaying the same cosignature should not error");

    let stored = chain
        .list_witness_cosignatures(&network_id, "core", tree_size)
        .await
        .expect("listing cosignatures should succeed");
    assert_eq!(stored, vec![cosig.clone()]);

    let head = chain
        .cosigned_tree_head_at("core", tree_size)
        .await
        .expect("assembling the cosigned head should succeed")
        .expect("an STH exists at this tree_size, so a head should be assembled");
    assert_eq!(head.sth.root_hash, root_hash);
    assert_eq!(head.cosignatures, vec![cosig]);

    // Degenerate case: a known list of one still verifies purely off the
    // author signature — this fixture never set a real settlement key, so
    // author verification legitimately fails, proving the STH check (not
    // the cosignature check) is actually driving the decision here.
    let bogus_author_key = SigningKey::generate(&mut rand::rng()).verifying_key();
    let known_list = vec![("witness-live-1".to_string(), witness_key.verifying_key())];
    assert!(!verify_cosigned_tree_head(
        &bogus_author_key,
        &head,
        &known_list,
        author_created_at,
        observed_at + time::Duration::seconds(1),
    ));
}

#[tokio::test]
#[ignore]
async fn a_conflicting_cosignature_from_the_same_witness_is_rejected() {
    let pool = test_pool().await;
    let network_id = format!("avalon-witness-cosign-live-test-{}", Uuid::new_v4());
    let chain = PostgresSettlementProvider::new(pool.clone(), network_id.clone());

    let tree_size = fresh_test_tree_size();
    let root_hash = "ab".repeat(32);
    insert_test_sth(&pool, &network_id, tree_size, &root_hash).await;

    let author_created_at = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1_800_000_000);
    let observed_at = author_created_at + time::Duration::seconds(5);
    let witness_key = SigningKey::generate(&mut rand::rng());

    let first = sign_witness_cosignature(
        &witness_key,
        "witness-live-2",
        tree_size,
        &root_hash,
        &network_id,
        author_created_at,
        observed_at,
    );
    chain
        .store_witness_cosignature("core", &first)
        .await
        .expect("storing the first cosignature should succeed");

    // A different signature for the same (network_id, tree_size, witness) —
    // e.g. a bug or an equivocation — must not silently overwrite the
    // first, whichever one happens to be genuine.
    let mut conflicting: WitnessCosignature = first.clone();
    conflicting.signature = "ff".repeat(64);
    let result = chain.store_witness_cosignature("core", &conflicting).await;
    assert!(
        result.is_err(),
        "a conflicting cosignature must be rejected"
    );

    let stored = chain
        .list_witness_cosignatures(&network_id, "core", tree_size)
        .await
        .expect("listing cosignatures should succeed");
    assert_eq!(stored, vec![first]);
}
