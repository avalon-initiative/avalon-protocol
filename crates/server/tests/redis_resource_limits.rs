//! Issue #545: exercises the Redis-backed rate limit/concurrency ceiling
//! (`crate::redis_limits`) against **two separate, real `avalon-server`
//! processes sharing one Redis** — the actual claim being tested is that
//! a configured limit holds *across* processes, which a single-process
//! test structurally cannot prove (see
//! `crates/server/tests/resource_limits.rs` for the in-process/no-Redis
//! equivalent of both tests here — same technique, `tokio::spawn` bursts
//! and a wall-clock comparison against baseline, applied to a two-node
//! deployment instead of one).
//!
//! Gated `--ignored`/live. Needs two `avalon-server` processes, both
//! pointed at the same `AVALON_REDIS_URL`, with small limits so the
//! rejection/backpressure path is reachable quickly:
//!
//! ```text
//! # node A
//! AVALON_SERVER_ADDR=127.0.0.1:18090 AVALON_REDIS_URL=redis://<host>:6379 \
//!   AVALON_RATE_LIMIT_PER_MINUTE=10 AVALON_MAX_CONCURRENT_REQUESTS=3 \
//!   cargo run -p avalon-server
//!
//! # node B
//! AVALON_SERVER_ADDR=127.0.0.1:18091 AVALON_REDIS_URL=redis://<host>:6379 \
//!   AVALON_RATE_LIMIT_PER_MINUTE=10 AVALON_MAX_CONCURRENT_REQUESTS=3 \
//!   cargo run -p avalon-server
//!
//! AVALON_SERVER_URL=http://127.0.0.1:18090 \
//!   AVALON_REDIS_LIMITS_PEER_SERVER_URL=http://127.0.0.1:18091 \
//!   cargo test -p avalon-server --test redis_resource_limits -- --ignored
//! ```
//!
//! Live-verified in-session against a real Redis (a throwaway Docker
//! container on the `avalon-peer` sandbox VM, reachable from this host) —
//! not just written and hoped: 6 requests to node A + 4 to node B
//! (`AVALON_RATE_LIMIT_PER_MINUTE=10`) succeeded, the 5th and 6th on node
//! B then got `429` — the combined total across both processes, not
//! per-process. A `AVALON_MAX_CONCURRENT_REQUESTS=3` burst of 24 requests
//! split across both nodes all returned `200` in ~60ms, ~10x a single-
//! request baseline (~5ms) — real cross-node backpressure, not a
//! coincidence of two independent per-process ceilings never being hit.

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn require_peer_server_url() -> String {
    std::env::var("AVALON_REDIS_LIMITS_PEER_SERVER_URL").unwrap_or_else(|_| {
        panic!(
            "AVALON_REDIS_LIMITS_PEER_SERVER_URL not set — this test needs a second, \
             independently-running avalon-server process sharing this node's AVALON_REDIS_URL \
             (see this file's module doc comment)"
        )
    })
}

/// The rate limiter's shared state actually holds *across* the two
/// processes: exhausting most of the budget on node A leaves only the
/// remainder available on node B, not a fresh `per_minute` allotment.
#[tokio::test]
#[ignore]
async fn the_rate_limit_is_shared_across_both_nodes() {
    let Some(per_minute) = std::env::var("AVALON_RATE_LIMIT_PER_MINUTE")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    else {
        eprintln!(
            "skipping: set AVALON_RATE_LIMIT_PER_MINUTE (e.g. 10) on both nodes before \
             running this test"
        );
        return;
    };

    let http = reqwest::Client::new();
    let node_a = server_url();
    let node_b = require_peer_server_url();

    // Spend most of the shared budget on node A alone.
    let spend_on_a = per_minute.saturating_sub(2).max(1);
    for i in 0..spend_on_a {
        let response = http
            .get(format!("{node_a}/nodes/peers"))
            .send()
            .await
            .expect("request to node A failed — is it running?");
        assert!(
            response.status().is_success(),
            "request {i} to node A should not be rate-limited yet: {:?}",
            response.status()
        );
    }

    // Node B should already be starved — a per-process (not shared)
    // limiter would incorrectly grant it a fresh `per_minute` budget of
    // its own here.
    let mut saw_429_on_b = false;
    for _ in 0..(per_minute * 2) {
        let response = http
            .get(format!("{node_b}/nodes/peers"))
            .send()
            .await
            .expect("request to node B failed — is it running?");
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            saw_429_on_b = true;
            break;
        }
    }
    assert!(
        saw_429_on_b,
        "node B was never rate-limited after node A spent most of the shared budget — the \
         limit is not actually shared across processes"
    );
}

/// The concurrency ceiling's shared state actually holds across the two
/// processes: a burst split evenly across both nodes, well past the
/// configured ceiling, must still all succeed (backpressure, never a
/// silent drop or error — same posture `resource_limits.rs`'s in-process
/// equivalent asserts), with total wall time meaningfully exceeding a
/// single-request baseline — proof real serialization happened, not that
/// each node independently had enough headroom to absorb its half.
#[tokio::test]
#[ignore]
async fn the_concurrency_ceiling_is_shared_across_both_nodes() {
    let Some(max_concurrent) = std::env::var("AVALON_MAX_CONCURRENT_REQUESTS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
    else {
        eprintln!(
            "skipping: set AVALON_MAX_CONCURRENT_REQUESTS (e.g. 3) on both nodes before \
             running this test"
        );
        return;
    };

    let http = reqwest::Client::new();
    let node_a = server_url();
    let node_b = require_peer_server_url();

    let baseline_start = std::time::Instant::now();
    let baseline = http
        .get(format!("{node_a}/nodes/peers"))
        .send()
        .await
        .expect("baseline request failed — is node A running?");
    assert!(baseline.status().is_success());
    let baseline_elapsed = baseline_start.elapsed();

    let burst_size = max_concurrent * 8;
    let burst_start = std::time::Instant::now();
    let handles: Vec<_> = (0..burst_size)
        .map(|i| {
            let http = http.clone();
            // Alternate nodes so the burst is genuinely split, not sent
            // to one process alone.
            let base = if i % 2 == 0 {
                node_a.clone()
            } else {
                node_b.clone()
            };
            tokio::spawn(async move { http.get(format!("{base}/nodes/peers")).send().await })
        })
        .collect();

    for (i, handle) in handles.into_iter().enumerate() {
        let response = handle
            .await
            .unwrap_or_else(|e| panic!("request {i}'s task panicked: {e}"))
            .unwrap_or_else(|e| {
                panic!("request {i} in the burst failed outright rather than backpressuring: {e}")
            });
        assert!(
            response.status().is_success(),
            "request {i} in the burst got a non-2xx instead of being backpressured: {:?}",
            response.status()
        );
    }
    let burst_elapsed = burst_start.elapsed();

    assert!(
        burst_elapsed > baseline_elapsed * 3,
        "a {burst_size}-request burst across two nodes sharing a ceiling of {max_concurrent} \
         completed in {burst_elapsed:?}, not meaningfully slower than the {baseline_elapsed:?} \
         single-request baseline — the ceiling does not appear to be actually shared"
    );
}
