//! Optional, per-hoster shared rate-limit/concurrency-ceiling backend
//! (issue #545, resolving #537's gap: `AVALON_RATE_LIMIT_PER_MINUTE`/
//! `AVALON_MAX_CONCURRENT_REQUESTS` were purely in-process state, so a
//! hoster running more than one `avalon-server` process got that many
//! independent copies of each ceiling instead of one that actually held
//! across their whole deployment).
//!
//! **Strictly per-hoster, never network-wide.** `AVALON_REDIS_URL`, when
//! set, points at *that operator's own* Redis instance for *their own*
//! processes — the same way `DATABASE_URL` already does for Postgres.
//! Nothing here coordinates, shares state with, or is even aware of any
//! other hoster's deployment; two independent operators running Avalon
//! nodes never share a Redis, any more than they'd share a Postgres. A
//! network-wide shared limiter would recreate exactly the single-point-of-
//! control problem #527/#535's decisions spent this whole session's
//! thread removing — this is the opposite of that: it only lets one
//! operator's own multiple processes agree with each other.
//!
//! **Unset (the default): zero behavior change.** `AVALON_REDIS_URL`
//! unset means `crate::router` keeps using exactly the `tower_governor`/
//! `tower::limit::ConcurrencyLimitLayer` in-process path it always has —
//! no new dependency, no new failure mode, for a solo hoster or anyone
//! who hasn't opted in.
//!
//! **Why hand-rolled, not `tower_governor` with a different backend.**
//! `tower_governor` is hard-wired to the `governor` crate's in-memory
//! keyed state store — there is no pluggable backend to swap in. Rather
//! than fork or wrap it, this module implements the two algorithms
//! directly against Redis, as `axum::middleware::from_fn_with_state`
//! layers used *instead of* (not alongside) the in-process ones when
//! configured:
//!
//! - **Rate limit**: fixed-window counter, `INCR`+`PEXPIRE` in one Lua
//!   script for atomicity (a plain `INCR` then a separate `PEXPIRE` has a
//!   window where a crash or concurrent request could leave a key with no
//!   expiry). A known, honest simplification: fixed-window admits up to
//!   ~2x the configured rate right at a window boundary (a request at
//!   `:59` and another at `:01` are different windows) — the same
//!   tradeoff a first Redis-backed limiter almost always starts with, and
//!   still strictly better than today's real gap (no cross-process limit
//!   at all). A token-bucket/GCRA Lua implementation is a strict
//!   improvement path if the boundary imprecision ever actually matters
//!   in practice, not designed here.
//! - **Concurrency ceiling**: a sorted set of in-flight request ids scored
//!   by start time. Admission prunes anything older than a staleness
//!   threshold (a process that crashed mid-request without releasing its
//!   slot self-heals on the *next* request's admission check, not stuck
//!   forever), then admits iff the live count is under the ceiling.
//!   Backpressures like the in-process `ConcurrencyLimitLayer` does
//!   (bounded retry-with-backoff), never silently drops — only returns
//!   `503` if still full after the wait budget.

use std::net::SocketAddr;
use std::sync::LazyLock;
use std::time::Duration;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use uuid::Uuid;

/// A live-in-flight request is presumed dead (its owning process crashed
/// without releasing its slot) if it's been "in flight" longer than this —
/// generous relative to any real request, so a slow-but-alive request is
/// never mistaken for a leaked one.
const STALE_REQUEST_THRESHOLD_MS: i64 = 60_000;
/// How long the admission loop backpressures before giving up and
/// returning `503` — matches the spirit of `ConcurrencyLimitLayer`'s
/// bounded wait, not an indefinite hang.
const CONCURRENCY_WAIT_BUDGET: Duration = Duration::from_secs(5);
const CONCURRENCY_RETRY_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Clone)]
pub struct RedisLimiterState {
    conn: ConnectionManager,
    rate_limit_per_minute: u64,
    max_concurrent_requests: usize,
}

impl RedisLimiterState {
    /// `AVALON_REDIS_URL` unset (the default) returns `None`, leaving
    /// `crate::router` on its existing in-process layers — see module doc
    /// comment. Reads `AVALON_RATE_LIMIT_PER_MINUTE`/
    /// `AVALON_MAX_CONCURRENT_REQUESTS` itself (same resolution
    /// `crate::router` uses for the in-process layers) so both backends
    /// enforce the identical configured ceiling — only *where* that
    /// ceiling is tracked changes.
    pub async fn from_env() -> Option<Self> {
        let url = std::env::var("AVALON_REDIS_URL")
            .ok()
            .filter(|s| !s.is_empty())?;
        let client =
            redis::Client::open(url).expect("AVALON_REDIS_URL must be a valid redis:// URL");
        let conn = ConnectionManager::new(client).await.expect(
            "failed to connect to AVALON_REDIS_URL — check the Redis instance is reachable",
        );
        Some(Self {
            conn,
            rate_limit_per_minute: crate::rate_limit_per_minute_from_env(),
            max_concurrent_requests: crate::max_concurrent_requests_from_env(),
        })
    }
}

/// Same key shape `IntegratorOrIpKeyExtractor` (`crate::lib`) uses for the
/// in-process limiter — kept independently here rather than shared, since
/// that extractor is tied to `tower_governor`'s `KeyExtractor` trait and
/// this middleware isn't a `tower_governor` layer at all.
fn rate_limit_key(req: &Request) -> String {
    if let Some(key_id) = req
        .headers()
        .get("x-avalon-integrator-key-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
    {
        return format!("integrator:{key_id}");
    }
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| format!("ip:{}", addr.ip()))
        .unwrap_or_else(|| "ip:unknown".to_string())
}

static RATE_LIMIT_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r#"
        local current = redis.call('INCR', KEYS[1])
        if current == 1 then
            redis.call('PEXPIRE', KEYS[1], ARGV[1])
        end
        return current
        "#,
    )
});

/// `axum::middleware::from_fn_with_state` layer — the Redis-backed
/// replacement for `tower_governor`'s `GovernorLayer` when
/// `AVALON_REDIS_URL` is configured. Fixed-window, `AVALON_RATE_LIMIT_PER_MINUTE`
/// requests per rolling 60s window per key — see module doc comment for
/// the fixed-window-boundary tradeoff.
pub async fn rate_limit_middleware(
    State(mut limiter): State<RedisLimiterState>,
    req: Request,
    next: Next,
) -> Response {
    let key = format!("avalon:ratelimit:{}", rate_limit_key(&req));
    let current: u64 = match RATE_LIMIT_SCRIPT
        .key(&key)
        .arg(60_000i64)
        .invoke_async(&mut limiter.conn)
        .await
    {
        Ok(v) => v,
        Err(err) => {
            // Redis itself is unreachable — fail open rather than taking
            // the whole node down over a rate-limit backend outage; the
            // in-process limiter has no equivalent failure mode to match,
            // so "let the request through, log it" is the safer default
            // than "reject every request until Redis comes back."
            tracing::warn!(error = %err, "redis rate limit: Redis unreachable, failing open");
            return next.run(req).await;
        }
    };

    if current > limiter.rate_limit_per_minute {
        let mut response = StatusCode::TOO_MANY_REQUESTS.into_response();
        if let Ok(value) = HeaderValue::from_str("60") {
            response.headers_mut().insert("retry-after", value);
        }
        return response;
    }

    next.run(req).await
}

/// Releases this request's concurrency-ceiling slot when dropped —
/// guarantees the `ZREM` happens even if the handler panics or returns
/// early, the same "never leak a permit" guarantee `ConcurrencyLimitLayer`
/// gives via RAII on its own semaphore permit. Async cleanup can't run
/// inside a synchronous `Drop`, so this spawns it — a released-but-not-
/// yet-processed slot is, worst case, indistinguishable from one that's
/// about to be pruned as stale, never a permanent leak either way.
struct ConcurrencySlotGuard {
    conn: ConnectionManager,
    key: String,
    member: String,
}

impl Drop for ConcurrencySlotGuard {
    fn drop(&mut self) {
        let mut conn = self.conn.clone();
        let key = self.key.clone();
        let member = self.member.clone();
        tokio::spawn(async move {
            let _: Result<i64, _> = conn.zrem(&key, &member).await;
        });
    }
}

static CONCURRENCY_ADMIT_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r#"
        local key = KEYS[1]
        local now = tonumber(ARGV[1])
        local stale_before = now - tonumber(ARGV[2])
        local limit = tonumber(ARGV[3])
        local member = ARGV[4]
        redis.call('ZREMRANGEBYSCORE', key, '-inf', stale_before)
        local count = redis.call('ZCARD', key)
        if count < limit then
            redis.call('ZADD', key, now, member)
            redis.call('PEXPIRE', key, ARGV[2])
            return 1
        else
            return 0
        end
        "#,
    )
});

/// `axum::middleware::from_fn_with_state` layer — the Redis-backed
/// replacement for `tower::limit::ConcurrencyLimitLayer` when
/// `AVALON_REDIS_URL` is configured. See module doc comment for the
/// staleness-pruning/backpressure design.
pub async fn concurrency_middleware(
    State(limiter): State<RedisLimiterState>,
    req: Request,
    next: Next,
) -> Response {
    let key = "avalon:concurrency".to_string();
    let member = Uuid::new_v4().to_string();
    let deadline = tokio::time::Instant::now() + CONCURRENCY_WAIT_BUDGET;

    loop {
        let mut conn = limiter.conn.clone();
        let now = (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64;
        let admitted: Result<i64, redis::RedisError> = CONCURRENCY_ADMIT_SCRIPT
            .key(&key)
            .arg(now)
            .arg(STALE_REQUEST_THRESHOLD_MS)
            .arg(limiter.max_concurrent_requests)
            .arg(&member)
            .invoke_async(&mut conn)
            .await;

        match admitted {
            Ok(1) => {
                let _guard = ConcurrencySlotGuard {
                    conn: limiter.conn.clone(),
                    key,
                    member,
                };
                return next.run(req).await;
            }
            Ok(_) => {
                if tokio::time::Instant::now() >= deadline {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                tokio::time::sleep(CONCURRENCY_RETRY_INTERVAL).await;
            }
            Err(err) => {
                // Same "fail open over Redis-backend outage" posture the
                // rate limiter takes above.
                tracing::warn!(error = %err, "redis concurrency limit: Redis unreachable, failing open");
                return next.run(req).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;

    #[test]
    fn rate_limit_key_prefers_integrator_header_over_ip() {
        let req = HttpRequest::builder()
            .header("x-avalon-integrator-key-id", "abc123")
            .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1234))))
            .body(Body::empty())
            .unwrap();
        assert_eq!(rate_limit_key(&req), "integrator:abc123");
    }

    #[test]
    fn rate_limit_key_falls_back_to_ip() {
        let req = HttpRequest::builder()
            .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1234))))
            .body(Body::empty())
            .unwrap();
        assert_eq!(rate_limit_key(&req), "ip:127.0.0.1");
    }
}
