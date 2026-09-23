//! Live tests for `mirror::check_convergence`, the offline check that a
//! mirror's copy of a shard equals the authority's history. Each test seeds
//! an isolated, uniquely named network in the real database, so runs never
//! interfere with each other or with real mirrored data.

use avalon_chain::merkle;
use avalon_chain::mirror::{
    self, ConvergenceVerdict, EquivocationFinding, MirroredEntry, ObservedSth, CORE_SHARD_ID,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

const SOURCE: &str = "http://authority.invalid";

async fn test_pool() -> PgPool {
    avalon_devenv::load();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

fn entry_hash(seq: i64) -> String {
    hex::encode([seq as u8; 32])
}

/// Seeds `count` mirrored entries whose `prev_hash` links form an unbroken
/// chain, except at `break_at` (a seq) when set.
async fn seed_entries(pool: &PgPool, network_id: &str, count: i64, break_at: Option<i64>) {
    for seq in 1..=count {
        let prev_hash = if Some(seq) == break_at {
            "ff".repeat(32)
        } else if seq == 1 {
            "00".repeat(32)
        } else {
            entry_hash(seq - 1)
        };
        mirror::insert_mirrored_entry(
            pool,
            &MirroredEntry {
                source_url: SOURCE.to_string(),
                network_id: network_id.to_string(),
                shard_id: CORE_SHARD_ID.to_string(),
                seq,
                event_id: Uuid::new_v4(),
                kind: "identity.created".to_string(),
                issuer: "identity:11111111-1111-1111-1111-111111111111:self:created".to_string(),
                subject: format!("identity:{seq}"),
                payload: Some(serde_json::json!({})),
                event_timestamp: OffsetDateTime::UNIX_EPOCH,
                version: 1,
                prev_hash,
                entry_hash: entry_hash(seq),
                batch_id: Uuid::new_v4(),
                verified_tree_size: seq,
            },
        )
        .await
        .expect("insert_mirrored_entry failed");
    }
}

fn root_over(count: i64) -> String {
    let hashes: Vec<String> = (1..=count).map(entry_hash).collect();
    hex::encode(merkle::mth_of_hex_hashes(&hashes).expect("valid hex hashes"))
}

async fn observe(pool: &PgPool, network_id: &str, tree_size: i64, root_hash: String) {
    mirror::insert_observation(
        pool,
        &ObservedSth {
            source_url: SOURCE.to_string(),
            network_id: network_id.to_string(),
            shard_id: CORE_SHARD_ID.to_string(),
            tree_size,
            root_hash,
            signature: "00".repeat(64),
            signing_key_id: "test-key".to_string(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            observed_at: OffsetDateTime::UNIX_EPOCH,
        },
    )
    .await
    .expect("insert_observation failed");
}

async fn verdict(pool: &PgPool, network_id: &str) -> ConvergenceVerdict {
    mirror::check_convergence(pool, network_id, CORE_SHARD_ID, None)
        .await
        .expect("check_convergence failed")
        .verdict
}

fn network(label: &str) -> String {
    format!("avalon-test-convergence-{label}-{}", Uuid::new_v4())
}

#[tokio::test]
#[ignore]
async fn a_fully_mirrored_shard_matching_the_latest_sth_is_converged() {
    let pool = test_pool().await;
    let net = network("converged");
    seed_entries(&pool, &net, 5, None).await;
    observe(&pool, &net, 5, root_over(5)).await;
    assert_eq!(
        verdict(&pool, &net).await,
        ConvergenceVerdict::Converged { tree_size: 5 }
    );
}

#[tokio::test]
#[ignore]
async fn a_mirror_missing_the_tail_the_latest_sth_attests_to_is_behind() {
    let pool = test_pool().await;
    let net = network("behind");
    seed_entries(&pool, &net, 5, None).await;
    observe(&pool, &net, 5, root_over(5)).await;
    observe(&pool, &net, 8, "ab".repeat(32)).await;
    assert_eq!(
        verdict(&pool, &net).await,
        ConvergenceVerdict::Behind {
            mirrored: 5,
            observed: 8
        }
    );
}

#[tokio::test]
#[ignore]
async fn mirrored_data_whose_root_matches_no_observed_sth_is_a_mismatch() {
    let pool = test_pool().await;
    let net = network("mismatch");
    seed_entries(&pool, &net, 5, None).await;
    observe(&pool, &net, 5, "cd".repeat(32)).await;
    assert_eq!(
        verdict(&pool, &net).await,
        ConvergenceVerdict::RootMismatch { tree_size: 5 }
    );
}

#[tokio::test]
#[ignore]
async fn a_broken_prev_hash_link_is_reported_even_when_the_root_matches() {
    let pool = test_pool().await;
    let net = network("chain-break");
    seed_entries(&pool, &net, 5, Some(4)).await;
    observe(&pool, &net, 5, root_over(5)).await;
    assert_eq!(
        verdict(&pool, &net).await,
        ConvergenceVerdict::ChainBroken { breaks: 1 }
    );
}

#[tokio::test]
#[ignore]
async fn an_unresolved_equivocation_blocks_convergence() {
    let pool = test_pool().await;
    let net = network("equivocation");
    seed_entries(&pool, &net, 5, None).await;
    observe(&pool, &net, 5, root_over(5)).await;
    mirror::record_equivocation(
        &pool,
        &EquivocationFinding {
            network_id: net.clone(),
            shard_id: CORE_SHARD_ID.to_string(),
            tree_size: 5,
            source_a: "peer-a".to_string(),
            root_hash_a: root_over(5),
            source_b: "peer-b".to_string(),
            root_hash_b: "ee".repeat(32),
            resolved_at: None,
            resolved_root_hash: None,
        },
    )
    .await
    .expect("record_equivocation failed");
    assert_eq!(
        verdict(&pool, &net).await,
        ConvergenceVerdict::BlockedByEquivocation { findings: 1 }
    );
}

#[tokio::test]
#[ignore]
async fn a_network_with_nothing_mirrored_is_never_converged() {
    let pool = test_pool().await;
    let net = network("empty");
    assert_eq!(
        verdict(&pool, &net).await,
        ConvergenceVerdict::NothingMirrored
    );
}
