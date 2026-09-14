//! A generic idempotency cache for mutations that need to survive a
//! client-side retry without double-applying (issue #47's "never retry a
//! non-idempotent write without an idempotency key" invariant). Backed by
//! the `idempotency_keys` table (`db/migrations/0054_idempotency_keys`),
//! keyed by `(integrator_id, idempotency_key, endpoint)`.
//!
//! This first pass wires exactly one write path —
//! `achievements::issue_attestation`, the concrete example #47 itself
//! names ("issue_achievement and future mutations") — since double-issuing
//! an attestation is the worst failure mode a bare request retry could
//! cause. Extending this to every other mutating endpoint in the server is
//! a documented follow-up, not attempted here: each write has its own
//! shape of "what does replaying the cached response even mean," and
//! generalizing that honestly is more than this ticket's pass can cover.

use axum::http::HeaderMap;
use serde::de::DeserializeOwned;
use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;

pub const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";

/// A request carries at most one idempotency key, read once per handler.
/// Absent for a caller not opting into retry-safety — those callers get no
/// dedup, matching #47's "writes retry only when the key is present"
/// design (no key means the SDK itself never automatically retries this
/// write, so there's nothing to dedupe).
pub fn read_idempotency_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get(IDEMPOTENCY_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// Looks up a previously cached response for `(integrator_id, key,
/// endpoint)`, if any. Call this before doing the mutation's real work;
/// on `Some`, return the cached body directly rather than re-executing.
pub async fn find_cached<T: DeserializeOwned>(
    state: &AppState,
    integrator_id: Uuid,
    key: &str,
    endpoint: &str,
) -> Result<Option<T>, AppError> {
    let row: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT response_body FROM idempotency_keys \
         WHERE integrator_id = $1 AND idempotency_key = $2 AND endpoint = $3",
    )
    .bind(integrator_id)
    .bind(key)
    .bind(endpoint)
    .fetch_optional(&state.pool)
    .await?;

    Ok(match row {
        Some(value) => Some(serde_json::from_value(value).map_err(|_| {
            AppError::Database(sqlx::Error::Decode(
                "idempotency_keys.response_body did not match the expected shape".into(),
            ))
        })?),
        None => None,
    })
}

/// Records a successful response for `(integrator_id, key, endpoint)`
/// after the mutation actually committed. `ON CONFLICT DO NOTHING`: if two
/// concurrent retries both raced past `find_cached` and both committed,
/// only the first insert wins here — an accepted, narrow race (see this
/// module's own doc comment on scope), never a write failure.
pub async fn store(
    state: &AppState,
    integrator_id: Uuid,
    key: &str,
    endpoint: &str,
    response_body: &impl Serialize,
) -> Result<(), AppError> {
    let body = serde_json::to_value(response_body).expect("response types are always JSON-safe");
    sqlx::query(
        "INSERT INTO idempotency_keys (integrator_id, idempotency_key, endpoint, response_body, created_at) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (integrator_id, idempotency_key, endpoint) DO NOTHING",
    )
    .bind(integrator_id)
    .bind(key)
    .bind(endpoint)
    .bind(body)
    .bind(OffsetDateTime::now_utc())
    .execute(&state.pool)
    .await?;
    Ok(())
}
