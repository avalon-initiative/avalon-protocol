//! Per-principal rate limit applied after a credential has been verified.
//! Keys are only ever a verified identity id or verified integrator id, so a
//! client can never choose its own bucket.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use governor::clock::{Clock, DefaultClock};
use governor::{DefaultKeyedRateLimiter, Quota};
use uuid::Uuid;

use crate::error::AppError;
use crate::redis_limits::RedisLimiterState;

const DEFAULT_PRINCIPAL_RATE_LIMIT_PER_MINUTE: u64 = 600;
/// Tracked-key count above which idle buckets are dropped on the next check.
const PRUNE_THRESHOLD: usize = 10_000;

pub(crate) fn principal_rate_limit_per_minute_from_env() -> u64 {
    std::env::var("AVALON_PRINCIPAL_RATE_LIMIT_PER_MINUTE")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_PRINCIPAL_RATE_LIMIT_PER_MINUTE)
}

/// The principal kind is part of the bucket key so an identity id and an
/// integrator id can never share a bucket.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Principal {
    Identity(Uuid),
    Integrator(Uuid),
}

impl Principal {
    pub(crate) fn key(&self) -> String {
        match self {
            Principal::Identity(id) => format!("identity:{id}"),
            Principal::Integrator(id) => format!("integrator:{id}"),
        }
    }
}

#[derive(Clone)]
pub struct PrincipalLimiter {
    backend: Backend,
}

#[derive(Clone)]
enum Backend {
    InProcess(Arc<DefaultKeyedRateLimiter<String>>),
    Redis(RedisLimiterState),
}

fn quota(per_minute: u64) -> Quota {
    let burst =
        NonZeroU32::new(per_minute.min(u32::MAX as u64) as u32).expect("per_minute is always > 0");
    Quota::with_period(Duration::from_secs_f64(60.0 / per_minute as f64))
        .expect("period is non-zero")
        .allow_burst(burst)
}

impl PrincipalLimiter {
    /// Redis-backed when a Redis limiter is configured, in-process otherwise.
    pub fn from_env(redis: Option<&RedisLimiterState>) -> Self {
        Self::with_limit(redis, principal_rate_limit_per_minute_from_env())
    }

    pub fn with_limit(redis: Option<&RedisLimiterState>, per_minute: u64) -> Self {
        let backend = match redis {
            Some(state) => Backend::Redis(state.clone()),
            None => Backend::InProcess(Arc::new(DefaultKeyedRateLimiter::keyed(quota(per_minute)))),
        };
        Self { backend }
    }

    pub(crate) async fn check(&self, principal: Principal) -> Result<(), AppError> {
        let key = principal.key();
        match &self.backend {
            Backend::InProcess(limiter) => {
                if limiter.len() > PRUNE_THRESHOLD {
                    limiter.retain_recent();
                    limiter.shrink_to_fit();
                }
                limiter.check_key(&key).map_err(|not_until| {
                    let wait = not_until.wait_time_from(DefaultClock::default().now());
                    AppError::PrincipalRateLimited {
                        retry_after_secs: wait.as_secs().max(1),
                    }
                })
            }
            Backend::Redis(state) => {
                if state.check_principal(&key).await {
                    Ok(())
                } else {
                    Err(AppError::PrincipalRateLimited {
                        retry_after_secs: 60,
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn principals_have_independent_budgets() {
        let limiter = PrincipalLimiter::with_limit(None, 2);
        let a = Principal::Identity(Uuid::new_v4());
        let b = Principal::Identity(Uuid::new_v4());
        assert!(limiter.check(a).await.is_ok());
        assert!(limiter.check(a).await.is_ok());
        assert!(limiter.check(a).await.is_err());
        assert!(limiter.check(b).await.is_ok());
    }

    #[tokio::test]
    async fn identity_and_integrator_ids_do_not_share_a_bucket() {
        let limiter = PrincipalLimiter::with_limit(None, 1);
        let id = Uuid::new_v4();
        assert!(limiter.check(Principal::Identity(id)).await.is_ok());
        assert!(limiter.check(Principal::Integrator(id)).await.is_ok());
        assert!(limiter.check(Principal::Identity(id)).await.is_err());
    }
}
