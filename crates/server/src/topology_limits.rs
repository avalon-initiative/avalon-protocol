//! Shared building blocks for the probe and trace endpoints: the error
//! shape, a per-IP request-rate limiter, an in-flight cap, and client-address
//! derivation. Limits here are in addition to the node-wide per-IP ceiling.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::trusted_proxies::TrustedProxies;

/// Upper bound on distinct client addresses tracked at once.
const MAX_TRACKED_IPS: usize = 10_000;

/// Error body `{ "error": <message>, "code": <snake_case code> }` with an
/// optional `Retry-After`.
#[derive(Debug)]
pub struct TopologyError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub retry_after_secs: Option<u64>,
}

impl TopologyError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retry_after_secs: None,
        }
    }

    pub fn rate_limited(retry_after_secs: u64) -> Self {
        Self {
            retry_after_secs: Some(retry_after_secs.max(1)),
            ..Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "too many requests, retry later",
            )
        }
    }

    pub fn busy() -> Self {
        Self {
            retry_after_secs: Some(1),
            ..Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_in_flight",
                "too many requests in flight on this node, retry later",
            )
        }
    }
}

impl IntoResponse for TopologyError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(json!({ "error": self.message, "code": self.code })),
        )
            .into_response();
        if let Some(secs) = self.retry_after_secs {
            if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
                response.headers_mut().insert("retry-after", v);
            }
        }
        response
    }
}

/// Sliding-window request counter per client address.
pub struct IpRateLimiter {
    limit: usize,
    window: Duration,
    hits: Mutex<HashMap<IpAddr, VecDeque<Instant>>>,
}

impl IpRateLimiter {
    pub fn new(limit_per_window: usize, window: Duration) -> Self {
        Self {
            limit: limit_per_window.max(1),
            window,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Records a request from `ip`, or returns how long to wait before the
    /// next one would be admitted.
    pub fn check(&self, ip: IpAddr, now: Instant) -> Result<(), Duration> {
        let mut hits = self.hits.lock().expect("rate limiter lock poisoned");
        if hits.len() >= MAX_TRACKED_IPS && !hits.contains_key(&ip) {
            hits.retain(|_, q| {
                while q
                    .front()
                    .is_some_and(|t| now.duration_since(*t) >= self.window)
                {
                    q.pop_front();
                }
                !q.is_empty()
            });
            if hits.len() >= MAX_TRACKED_IPS {
                return Err(self.window);
            }
        }
        let q = hits.entry(ip).or_default();
        while q
            .front()
            .is_some_and(|t| now.duration_since(*t) >= self.window)
        {
            q.pop_front();
        }
        if q.len() >= self.limit {
            let oldest = *q.front().expect("non-empty at limit");
            return Err(self.window.saturating_sub(now.duration_since(oldest)));
        }
        q.push_back(now);
        Ok(())
    }
}

/// Cap on concurrently running requests of one kind on this node.
#[derive(Clone)]
pub struct InFlightGate {
    permits: Arc<Semaphore>,
}

impl InFlightGate {
    pub fn new(max: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(max.max(1))),
        }
    }

    pub fn try_enter(&self) -> Result<OwnedSemaphorePermit, TopologyError> {
        self.permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| TopologyError::busy())
    }
}

/// Rate limiter plus in-flight cap for one endpoint.
pub struct EndpointLimits {
    pub rate: IpRateLimiter,
    pub in_flight: InFlightGate,
}

impl EndpointLimits {
    pub fn new(per_minute: usize, max_in_flight: usize) -> Self {
        Self {
            rate: IpRateLimiter::new(per_minute, Duration::from_secs(60)),
            in_flight: InFlightGate::new(max_in_flight),
        }
    }

    pub fn from_env(
        rate_var: &str,
        rate_default: usize,
        cap_var: &str,
        cap_default: usize,
    ) -> Self {
        Self::new(
            env_usize(rate_var, rate_default),
            env_usize(cap_var, cap_default),
        )
    }

    /// Applies the per-IP rate limit.
    pub fn admit(&self, ip: IpAddr) -> Result<(), TopologyError> {
        self.rate.check(ip, Instant::now()).map_err(|wait| {
            TopologyError::rate_limited(wait.as_secs() + u64::from(wait.subsec_nanos() > 0))
        })
    }
}

fn env_usize(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

/// Client address used as the rate-limit key, honoring `AVALON_TRUSTED_PROXIES`.
pub fn client_ip(peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
    static PROXIES: OnceLock<TrustedProxies> = OnceLock::new();
    PROXIES
        .get_or_init(TrustedProxies::from_env)
        .client_ip(peer.ip(), headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn limiter_admits_up_to_the_limit_then_reports_wait() {
        let l = IpRateLimiter::new(2, Duration::from_secs(60));
        let t0 = Instant::now();
        assert!(l.check(ip("1.1.1.1"), t0).is_ok());
        assert!(l.check(ip("1.1.1.1"), t0 + Duration::from_secs(10)).is_ok());
        let wait = l
            .check(ip("1.1.1.1"), t0 + Duration::from_secs(20))
            .unwrap_err();
        assert_eq!(wait, Duration::from_secs(40));
        assert!(l.check(ip("2.2.2.2"), t0 + Duration::from_secs(20)).is_ok());
        assert!(l.check(ip("1.1.1.1"), t0 + Duration::from_secs(61)).is_ok());
    }

    #[test]
    fn gate_refuses_past_the_cap_and_frees_on_drop() {
        let g = InFlightGate::new(1);
        let p = g.try_enter().unwrap();
        let err = g.try_enter().unwrap_err();
        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(err.retry_after_secs, Some(1));
        drop(p);
        assert!(g.try_enter().is_ok());
    }

    #[test]
    fn rate_limited_response_carries_retry_after() {
        let r = TopologyError::rate_limited(7).into_response();
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(r.headers().get("retry-after").unwrap(), "7");
    }
}
