//! Regression coverage for a small `AVALON_MAX_DB_CONNECTIONS` under
//! concurrent `PATCH /me` writes. Gated `--ignored`/live, same
//! manually-started-server convention `rate_limit_layers.rs` already
//! establishes for an env-configured server:
//!
//! ```text
//! AVALON_MAX_DB_CONNECTIONS=2 make start
//! cargo test -p avalon-server --test db_pool_write_concurrency -- --ignored --test-threads=1
//!
//! AVALON_MAX_DB_CONNECTIONS=3 make start
//! cargo test -p avalon-server --test db_pool_write_concurrency -- --ignored --test-threads=1
//! ```
//!
//! A pool this small used to let a single outbox-drain tick's nested
//! connection acquisition (`PostgresSettlementProvider::commit` opening
//! its own transaction, then querying the ledger tip through `self.pool`
//! again instead of that same transaction) starve every concurrent writer
//! until the pool's 30s acquire timeout. The fix keeps `commit`'s whole
//! write path on the one connection its transaction already holds, so
//! concurrent writers should queue and complete instead.

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::time::{Duration, Instant};
use time::OffsetDateTime;
use uuid::Uuid;

/// Comfortably above realistic queuing delay for a handful of writes on a
/// pool of 2-3 connections, and comfortably below the 30s acquire timeout
/// this test exists to make sure nobody hits.
const PER_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Total wall-clock budget for every write to complete — proves the writes
/// queued and finished rather than each independently riding out most of
/// the acquire timeout before failing.
const OVERALL_BUDGET: Duration = Duration::from_secs(25);
const CONCURRENT_WRITERS: usize = 20;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

async fn test_pool() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

async fn seed_identity_session(pool: &PgPool, label: &str) -> String {
    let identity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(identity_id)
        .execute(pool)
        .await
        .expect("failed to seed identity");
    sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(format!("db-pool-writes-{label}-{identity_id}"))
        .execute(pool)
        .await
        .expect("failed to seed profile");

    let token = format!("test-token-{identity_id}");
    let expires_at = OffsetDateTime::now_utc() + time::Duration::hours(1);
    sqlx::query("INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)")
        .bind(&token)
        .bind(identity_id)
        .bind(expires_at)
        .execute(pool)
        .await
        .expect("failed to seed session");

    token
}

/// `CONCURRENT_WRITERS` distinct identities each `PATCH /me` once, all in
/// flight at the same time — a small pool must queue these and let them
/// all complete, never let the outbox worker's own commit path starve
/// every one of them out to the acquire timeout.
#[tokio::test]
#[ignore]
async fn concurrent_profile_writes_queue_and_complete_on_a_small_pool() {
    let pool = test_pool().await;
    let base = server_url();

    let mut tokens = Vec::with_capacity(CONCURRENT_WRITERS);
    for i in 0..CONCURRENT_WRITERS {
        tokens.push(seed_identity_session(&pool, &i.to_string()).await);
    }

    let http = reqwest::Client::builder()
        .timeout(PER_REQUEST_TIMEOUT)
        .build()
        .expect("failed to build http client");

    let started = Instant::now();
    let mut requests = Vec::with_capacity(CONCURRENT_WRITERS);
    for (i, token) in tokens.into_iter().enumerate() {
        let http = http.clone();
        let base = base.clone();
        requests.push(tokio::spawn(async move {
            http.patch(format!("{base}/me"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "bio": format!("concurrent write {i}") }))
                .send()
                .await
        }));
    }

    let mut statuses = Vec::with_capacity(CONCURRENT_WRITERS);
    for request in requests {
        let response = request.await.expect("request task panicked").expect(
            "PATCH /me errored (connection refused, timeout, ...) — is `make start` running?",
        );
        statuses.push(response.status());
    }
    let elapsed = started.elapsed();

    let failed: Vec<_> = statuses.iter().filter(|s| !s.is_success()).collect();
    assert!(
        failed.is_empty(),
        "expected every concurrent PATCH /me to succeed, got statuses: {statuses:?}"
    );
    assert!(
        elapsed < OVERALL_BUDGET,
        "{CONCURRENT_WRITERS} concurrent writes took {elapsed:?}, expected under {OVERALL_BUDGET:?} \
         — this smells like writers queuing behind a starved connection pool rather than a small \
         pool just serializing them quickly"
    );
}
