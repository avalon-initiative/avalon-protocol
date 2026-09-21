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
    avalon_devenv::load();
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

/// How many leaf indices to sample per measurement — bounded regardless of
/// `tree_size` so this test's runtime depends only on this constant, never
/// on how large an already-existing ledger happens to be (issue #358: this
/// used to iterate every leaf in `0..tree_size`, so against a long-lived
/// dev database with thousands of pre-existing entries it scaled with the
/// *entire* accumulated ledger rather than the ~1000 entries the test
/// itself commits, and only got slower every time it ran).
const PROOF_SAMPLE_SIZE: i64 = 50;

/// Evenly-spaced leaf indices across `0..tree_size`, at most
/// `PROOF_SAMPLE_SIZE` of them — a representative sample of proof cost at
/// this tree size without visiting every leaf.
fn sample_leaf_indices(tree_size: i64) -> Vec<i64> {
    let step = (tree_size / PROOF_SAMPLE_SIZE).max(1);
    (0..tree_size).step_by(step as usize).collect()
}

/// Average per-leaf inclusion-proof time at `tree_size`, sampled across
/// [`sample_leaf_indices`] — all served from the in-memory incremental tree
/// this same process already built via `commit_n`, so this isolates
/// algorithmic cost from network/DB round-trip noise.
async fn avg_inclusion_proof_micros(chain: &PostgresSettlementProvider, tree_size: i64) -> f64 {
    let samples = sample_leaf_indices(tree_size);
    let start = Instant::now();
    for leaf_index in &samples {
        chain
            .inclusion_proof(*leaf_index, tree_size)
            .await
            .expect("proof should succeed");
    }
    start.elapsed().as_micros() as f64 / samples.len() as f64
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
    let samples = sample_leaf_indices(tree_size);
    let start = Instant::now();
    for leaf_index in &samples {
        let _proof = merkle::inclusion_proof_of_hex_hashes(*leaf_index as usize, &leaves)
            .expect("proof should succeed");
    }
    start.elapsed().as_micros() as f64 / samples.len() as f64
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

    // Compared against `old_ratio` measured in this same run (issue #358),
    // rather than a fixed absolute threshold: against a long-lived dev
    // database, `small_size`/`large_size` are dominated by a pre-existing
    // `baseline` and aren't reliably ~10x apart in absolute tree size, so a
    // fixed "ratio < 5.0" silently loses its ability to catch a real O(n)
    // regression once ambient ledger size dwarfs the ~1000 entries this
    // test commits — a genuine O(n) implementation's `ratio` would shrink
    // right along with it. `old_ratio` is measured against the exact same
    // `small_size`/`large_size` in the exact same run, so it always tracks
    // whatever the real size ratio happens to be; the incremental tree's
    // O(log n) cost should stay flat regardless, so `ratio` should always
    // land well under `old_ratio` even when neither is anywhere near 10x.
    assert!(
        ratio < (old_ratio / 2.0).max(1.5),
        "per-proof cost grew {ratio:.2}x while the old from-scratch approach grew \
         {old_ratio:.2}x over the same tree-size increase — regression toward O(n)"
    );
}
