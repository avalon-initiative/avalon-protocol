//! Exercises the hoster-configurable resource limits (issue #363,
//! implementing #287's decided shape) against a real, running
//! `avalon-server`. Gated `--ignored` since it needs live infra — see
//! `make test-live` / `make start`.
//!
//! Both tests here need a *small* configured limit to exercise the
//! rejection/backpressure path quickly rather than needing hundreds of
//! real requests against the (deliberately generous) production defaults
//! — same "skip unless a small override is set" pattern
//! `crates/server/tests/presence.rs`'s `stale_presence_expires_to_offline`
//! and `crates/server/tests/achievement_write_quota.rs` already use.
//!
//! The two limits interact (`GovernorLayer` wraps `ConcurrencyLimitLayer`
//! in `crate::router`, so a burst of concurrent requests is rate-limited
//! *before* it is concurrency-limited), so exercising them precisely needs
//! two separate server configurations — run each test in its own `make
//! start` invocation:
//!
//! ```text
//! AVALON_RATE_LIMIT_PER_MINUTE=5 make start
//! cargo test -p avalon-server --test resource_limits rate_limit -- --ignored
//!
//! AVALON_MAX_CONCURRENT_REQUESTS=2 make start
//! cargo test -p avalon-server --test resource_limits concurrency -- --ignored
//! ```

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

#[tokio::test]
#[ignore]
async fn requests_past_the_rate_limit_get_429_with_retry_after() {
    let Some(per_minute) = std::env::var("AVALON_RATE_LIMIT_PER_MINUTE")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    else {
        eprintln!(
            "skipping: set AVALON_RATE_LIMIT_PER_MINUTE (e.g. 5) before `make start` to run this test"
        );
        return;
    };

    let http = reqwest::Client::new();
    let base = server_url();

    // The configured burst is exactly `per_minute` requests (crate::router's
    // `.burst_size(rate_limit_per_minute as u32)`) — a public, unauthenticated,
    // cheap endpoint so no integrator/session setup is needed; every call
    // here shares the loopback IP key (`IntegratorOrIpKeyExtractor`'s
    // fallback, since none of these calls carry
    // `x-avalon-integrator-key-id`).
    let mut last_status = reqwest::StatusCode::OK;
    let mut saw_429 = false;
    for i in 0..(per_minute * 2) {
        let response = http
            .get(format!("{base}/ledger/sth/latest"))
            .send()
            .await
            .expect("request failed — is `make start` running?");
        last_status = response.status();
        if last_status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            assert!(
                response
                    .headers()
                    .contains_key(reqwest::header::RETRY_AFTER),
                "a 429 must always carry Retry-After (request {i})"
            );
            saw_429 = true;
            break;
        }
        assert!(
            last_status.is_success(),
            "request {i} failed with an unexpected status: {last_status:?}"
        );
    }

    assert!(
        saw_429,
        "expected at least one 429 within {} requests against a burst of {per_minute} — last status: {last_status:?}",
        per_minute * 2
    );
}

#[tokio::test]
#[ignore]
async fn a_burst_beyond_the_concurrency_ceiling_backpressures_rather_than_failing() {
    let Some(max_concurrent) = std::env::var("AVALON_MAX_CONCURRENT_REQUESTS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
    else {
        eprintln!(
            "skipping: set AVALON_MAX_CONCURRENT_REQUESTS (e.g. 2) before `make start` to run this test"
        );
        return;
    };

    let http = reqwest::Client::new();
    let base = server_url();

    // Baseline: one request's own latency, unconcurrent.
    let baseline_start = std::time::Instant::now();
    let baseline = http
        .get(format!("{base}/ledger/sth/latest"))
        .send()
        .await
        .expect("baseline request failed — is `make start` running?");
    assert!(baseline.status().is_success());
    let baseline_elapsed = baseline_start.elapsed();

    // A burst well past the ceiling: every request must still succeed
    // (concurrency limiting backpressures — bounded wait — never a silent
    // drop or an error response), but with `max_concurrent` this small
    // relative to the burst size, they can't all be served in parallel, so
    // total wall time should meaningfully exceed the single-request
    // baseline.
    let burst_size = max_concurrent * 8;
    let burst_start = std::time::Instant::now();
    let handles: Vec<_> = (0..burst_size)
        .map(|_| {
            let http = http.clone();
            let base = base.clone();
            tokio::spawn(async move { http.get(format!("{base}/ledger/sth/latest")).send().await })
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
        burst_elapsed > baseline_elapsed * 2,
        "a burst of {burst_size} requests against a concurrency ceiling of {max_concurrent} \
         should take meaningfully longer than one unconcurrent request (baseline {baseline_elapsed:?}), \
         got {burst_elapsed:?} — did the ceiling actually apply?"
    );
}
