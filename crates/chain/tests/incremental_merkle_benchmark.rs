//! Issue #349's own acceptance criterion: a live regression guard proving
//! commit/proof-serving cost doesn't scale linearly with total ledger size.
//! Gated `--ignored` (needs live Postgres — `make test-live` / `make start`);
//! drives `PostgresSettlementProvider` directly rather than over HTTP, same
//! pattern as `tests/settlement.rs`.

use avalon_chain::{merkle, PostgresSettlementProvider, SettlementProvider};
use avalon_protocol::events::{EventBatch, ProtocolEvent};
use avalon_protocol::ids::GlobalId;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::time::Instant;
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

fn sample_event() -> ProtocolEvent {
    let actor = Uuid::new_v4();
    ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "benchmark.entry".to_string(),
        issuer: GlobalId::new("identity", &actor.to_string(), "self", "benchmark_entry"),
        subject: GlobalId::new("identity", &actor.to_string(), "self", "benchmark_entry"),
        payload: json!({ "note": "issue #349 benchmark" }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    }
}

async fn commit_n(chain: &PostgresSettlementProvider, n: usize) {
    for _ in 0..n {
        let batch = EventBatch {
            id: Uuid::new_v4(),
            events: vec![sample_event()],
            created_at: OffsetDateTime::now_utc(),
        };
        chain.commit(&batch).await.expect("commit should succeed");
    }
}

/// Average per-leaf inclusion-proof time at `tree_size`, sampled across
/// every leaf index — all served from the in-memory incremental tree this
/// same process already built via `commit_n`, so this isolates algorithmic
/// cost from network/DB round-trip noise.
async fn avg_inclusion_proof_micros(chain: &PostgresSettlementProvider, tree_size: i64) -> f64 {
    let start = Instant::now();
    for leaf_index in 0..tree_size {
        chain
            .inclusion_proof(leaf_index, tree_size)
            .await
            .expect("proof should succeed");
    }
    start.elapsed().as_micros() as f64 / tree_size as f64
}

/// The old from-scratch approach this ticket replaced: same per-leaf
/// inclusion proof, computed by recomputing `mth`/`path` over the entire
/// leaf set every call — what `crates/server/src/settlement.rs` did before
/// issue #349. Kept only for this comparison; not used anywhere else.
async fn avg_old_style_inclusion_proof_micros(
    chain: &PostgresSettlementProvider,
    tree_size: i64,
) -> f64 {
    let leaves = chain
        .entry_hashes_up_to(tree_size)
        .await
        .expect("entry_hashes_up_to should work");
    let start = Instant::now();
    for leaf_index in 0..tree_size as usize {
        let _proof = merkle::inclusion_proof_of_hex_hashes(leaf_index, &leaves)
            .expect("proof should succeed");
    }
    start.elapsed().as_micros() as f64 / tree_size as f64
}

/// Issue #349's core regression guard: growing the ledger 10x must not
/// grow per-proof cost anywhere close to 10x (that's what the old
/// from-scratch `mth`/`path` recomputation did — O(n) per call). The
/// incremental tree's O(log n) cost should barely move.
#[tokio::test]
#[ignore]
async fn inclusion_proof_cost_does_not_scale_linearly_with_ledger_size() {
    let pool = test_pool().await;
    let network_id =
        std::env::var("AVALON_NETWORK_ID").unwrap_or_else(|_| "avalon-dev-local".to_string());
    let chain = PostgresSettlementProvider::new(pool, network_id);

    let baseline = chain.entry_count().await.expect("entry_count should work");

    commit_n(&chain, 100).await;
    let small_size = baseline + 100;
    let small_avg = avg_inclusion_proof_micros(&chain, small_size).await;

    commit_n(&chain, 900).await;
    let large_size = baseline + 1000;
    let large_avg = avg_inclusion_proof_micros(&chain, large_size).await;

    let ratio = large_avg / small_avg.max(0.001);
    println!(
        "avg inclusion-proof cost: {small_avg:.2}µs @ tree_size={small_size}, \
         {large_avg:.2}µs @ tree_size={large_size} (10x ledger growth, {ratio:.2}x proof-cost growth)"
    );

    let old_small_avg = avg_old_style_inclusion_proof_micros(&chain, small_size).await;
    let old_large_avg = avg_old_style_inclusion_proof_micros(&chain, large_size).await;
    let old_ratio = old_large_avg / old_small_avg.max(0.001);
    println!(
        "old from-scratch approach, same ledger: {old_small_avg:.2}µs @ tree_size={small_size}, \
         {old_large_avg:.2}µs @ tree_size={large_size} ({old_ratio:.2}x proof-cost growth)"
    );

    // A true O(n) recomputation would show ~10x growth here (matching the
    // 10x growth in ledger size). O(log n) should show barely any. A
    // generous 5x threshold still clearly distinguishes the two while
    // leaving headroom for machine noise on tiny microsecond timings.
    assert!(
        ratio < 5.0,
        "per-proof cost grew {ratio:.2}x for a 10x ledger size increase — regression toward O(n)"
    );
}
