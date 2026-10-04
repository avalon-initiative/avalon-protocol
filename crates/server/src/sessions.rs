//! Opaque bearer sessions: stored as the SHA-256 of the token (a database read yields nothing
//! usable), tied to the credential that produced them, and ended on logout, credential
//! revocation, recovery, or expiry.

use std::time::Duration;

use avalon_protocol::ids::IdentityId;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{PgExecutor, Row};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::generate_session_token;
use crate::error::AppError;
use crate::handlers::{authenticate, bearer_token};
use crate::state::AppState;

pub const SESSION_LIFETIME_DAYS: i64 = 30;

const PRUNE_INTERVAL: Duration = Duration::from_secs(600);
const PRUNE_BATCH: i64 = 1000;

/// The credential that produced a session; revoking it ends the session.
#[derive(Clone, Copy, Default)]
pub struct SessionOrigin {
    pub passkey_id: Option<Uuid>,
    pub signing_key_id: Option<Uuid>,
}

pub struct MintedSession {
    pub token: String,
    pub expires_at: OffsetDateTime,
}

pub fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// Inserts a new session and returns its plaintext token, which exists nowhere else.
pub async fn mint<'e>(
    executor: impl PgExecutor<'e>,
    identity_id: IdentityId,
    origin: SessionOrigin,
) -> Result<MintedSession, AppError> {
    if origin.passkey_id.is_none() && origin.signing_key_id.is_none() {
        return Err(AppError::Unauthorized);
    }
    let token = generate_session_token();
    let expires_at = OffsetDateTime::now_utc() + time::Duration::days(SESSION_LIFETIME_DAYS);
    sqlx::query(
        "INSERT INTO sessions (token_hash, identity_id, expires_at, origin_passkey_id, origin_signing_key_id) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(hash_token(&token).to_vec())
    .bind(identity_id)
    .bind(expires_at)
    .bind(origin.passkey_id)
    .bind(origin.signing_key_id)
    .execute(executor)
    .await?;
    Ok(MintedSession { token, expires_at })
}

/// The identity a live session token belongs to. Expired, unknown, and revoked-origin sessions
/// are indistinguishable. The revocation tables are the mirrored record, so a credential revoked
/// on any node ends the sessions it produced here.
pub async fn resolve(pool: &sqlx::PgPool, token: &str) -> Result<IdentityId, AppError> {
    let hash = hash_token(token);
    let row = sqlx::query(
        "SELECT s.token_hash, s.identity_id FROM sessions s \
         WHERE s.token_hash = $1 AND s.expires_at > now() \
           AND NOT EXISTS (SELECT 1 FROM indexer_identity_signing_key_revocations r \
                           WHERE r.identity_id = s.identity_id AND r.signing_key_id = s.origin_signing_key_id) \
           AND NOT EXISTS (SELECT 1 FROM indexer_identity_passkey_revocations r \
                           WHERE r.identity_id = s.identity_id AND r.passkey_id = s.origin_passkey_id)",
    )
    .bind(hash.to_vec())
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::Unauthorized)?;
    let stored: Vec<u8> = row.try_get("token_hash")?;
    if !bool::from(stored.as_slice().ct_eq(&hash)) {
        return Err(AppError::Unauthorized);
    }
    Ok(row.try_get("identity_id")?)
}

pub async fn end_for_passkey<'e>(
    executor: impl PgExecutor<'e>,
    identity_id: IdentityId,
    passkey_id: Uuid,
) -> Result<u64, AppError> {
    let ended =
        sqlx::query("DELETE FROM sessions WHERE identity_id = $1 AND origin_passkey_id = $2")
            .bind(identity_id)
            .bind(passkey_id)
            .execute(executor)
            .await?;
    Ok(ended.rows_affected())
}

pub async fn end_for_signing_key<'e>(
    executor: impl PgExecutor<'e>,
    identity_id: IdentityId,
    signing_key_id: Uuid,
) -> Result<u64, AppError> {
    let ended =
        sqlx::query("DELETE FROM sessions WHERE identity_id = $1 AND origin_signing_key_id = $2")
            .bind(identity_id)
            .bind(signing_key_id)
            .execute(executor)
            .await?;
    Ok(ended.rows_affected())
}

/// Ends every session of the identity and drops its pending or approved pairing and cross-node
/// login requests, which would otherwise mint a fresh session on their next poll.
pub async fn end_all_for_identity(
    tx: &mut sqlx::PgConnection,
    identity_id: IdentityId,
) -> Result<u64, AppError> {
    sqlx::query("DELETE FROM device_pairings WHERE identity_id = $1")
        .bind(identity_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM cross_node_login_requests WHERE identity_id = $1")
        .bind(identity_id)
        .execute(&mut *tx)
        .await?;
    let ended = sqlx::query("DELETE FROM sessions WHERE identity_id = $1")
        .bind(identity_id)
        .execute(&mut *tx)
        .await?;
    Ok(ended.rows_affected())
}

/// Whether `signing_key_id` may still back a new session, checked inside the minting
/// transaction. A local key row is share-locked so a concurrent revocation serialises with the
/// mint; a key with no local row (approved on another node) is accepted only when
/// `require_local` is false and no mirrored revocation names it.
pub async fn signing_key_usable(
    tx: &mut sqlx::PgConnection,
    identity_id: IdentityId,
    signing_key_id: Uuid,
    require_local: bool,
) -> Result<bool, AppError> {
    let local: Option<Option<OffsetDateTime>> = sqlx::query_scalar(
        "SELECT revoked_at FROM identity_signing_keys WHERE id = $1 AND identity_id = $2 FOR SHARE",
    )
    .bind(signing_key_id)
    .bind(identity_id)
    .fetch_optional(&mut *tx)
    .await?;
    match local {
        Some(Some(_)) => return Ok(false),
        None if require_local => return Ok(false),
        _ => {}
    }
    let revoked: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM indexer_identity_signing_key_revocations \
         WHERE identity_id = $1 AND signing_key_id = $2)",
    )
    .bind(identity_id)
    .bind(signing_key_id)
    .fetch_one(&mut *tx)
    .await?;
    Ok(!revoked)
}

/// Deletes at most one batch of expired sessions, and of long-finished pairing and cross-node
/// login requests; returns how many sessions went.
pub async fn prune_expired(pool: &sqlx::PgPool) -> Result<u64, AppError> {
    sqlx::query(
        "DELETE FROM device_pairings WHERE id IN \
         (SELECT id FROM device_pairings WHERE expires_at < now() - interval '1 day' LIMIT $1)",
    )
    .bind(PRUNE_BATCH)
    .execute(pool)
    .await?;
    sqlx::query(
        "DELETE FROM cross_node_login_requests WHERE id IN \
         (SELECT id FROM cross_node_login_requests WHERE expires_at < now() - interval '1 day' LIMIT $1)",
    )
    .bind(PRUNE_BATCH)
    .execute(pool)
    .await?;
    let pruned = sqlx::query(
        "DELETE FROM sessions WHERE id IN \
         (SELECT id FROM sessions WHERE expires_at < now() LIMIT $1)",
    )
    .bind(PRUNE_BATCH)
    .execute(pool)
    .await?;
    Ok(pruned.rows_affected())
}

pub async fn run_prune_worker(state: AppState) {
    loop {
        loop {
            match prune_expired(&state.pool).await {
                Ok(count) => {
                    if count > 0 {
                        tracing::info!(count, "pruned expired sessions");
                    }
                    // A full batch means more may remain.
                    if count < PRUNE_BATCH as u64 {
                        break;
                    }
                }
                Err(err) => {
                    tracing::error!("session prune worker: {err}");
                    break;
                }
            }
        }
        tokio::time::sleep(PRUNE_INTERVAL).await;
    }
}

#[derive(Serialize, ToSchema)]
pub struct SessionSummary {
    pub id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub expires_at: OffsetDateTime,
    /// Whether this is the session the request itself authenticated with.
    pub current: bool,
    /// The passkey that produced this session, when it came from a passkey login.
    pub origin_passkey_id: Option<Uuid>,
    /// The signing key that approved this session, when it came from device pairing or a
    /// cross-node login.
    pub origin_signing_key_id: Option<Uuid>,
}

#[derive(Serialize, ToSchema)]
pub struct ListSessionsResponse {
    pub sessions: Vec<SessionSummary>,
}

/// `POST /sessions/logout` — ends the session the request authenticated with.
#[utoipa::path(
    post,
    path = "/sessions/logout",
    tag = "identity",
    responses((status = 200, description = "The presented session no longer exists")),
)]
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<(), AppError> {
    let token = bearer_token(&headers)?;
    authenticate(&state, &headers).await?;
    let ended = sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
        .bind(hash_token(token).to_vec())
        .execute(&state.pool)
        .await?;
    if ended.rows_affected() == 0 {
        return Err(AppError::Unauthorized);
    }
    Ok(())
}

/// `GET /me/sessions` — the caller's live sessions, newest first.
#[utoipa::path(
    get,
    path = "/me/sessions",
    tag = "identity",
    responses((status = 200, description = "The caller's live sessions", body = ListSessionsResponse)),
)]
pub async fn list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ListSessionsResponse>, AppError> {
    let token = bearer_token(&headers)?;
    let identity_id = authenticate(&state, &headers).await?;
    let current_hash = hash_token(token).to_vec();
    let rows = sqlx::query(
        "SELECT id, token_hash, created_at, expires_at, origin_passkey_id, origin_signing_key_id \
         FROM sessions WHERE identity_id = $1 AND expires_at > now() ORDER BY created_at DESC",
    )
    .bind(identity_id)
    .fetch_all(&state.pool)
    .await?;
    let sessions = rows
        .iter()
        .map(|row| {
            let hash: Vec<u8> = row.try_get("token_hash")?;
            Ok(SessionSummary {
                id: row.try_get("id")?,
                created_at: row.try_get("created_at")?,
                expires_at: row.try_get("expires_at")?,
                current: hash == current_hash,
                origin_passkey_id: row.try_get("origin_passkey_id")?,
                origin_signing_key_id: row.try_get("origin_signing_key_id")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
    Ok(Json(ListSessionsResponse { sessions }))
}

/// `POST /me/sessions/{id}/revoke` — ends one of the caller's own sessions (404 for any other).
#[utoipa::path(
    post,
    path = "/me/sessions/{id}/revoke",
    tag = "identity",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, description = "Session revoked"),
        (status = 404, description = "SESSION_NOT_FOUND"),
    ),
)]
pub async fn revoke_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<Uuid>,
) -> Result<(), AppError> {
    let identity_id = authenticate(&state, &headers).await?;
    let ended = sqlx::query("DELETE FROM sessions WHERE id = $1 AND identity_id = $2")
        .bind(session_id)
        .bind(identity_id)
        .execute(&state.pool)
        .await?;
    if ended.rows_affected() == 0 {
        return Err(AppError::SessionNotFound);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_hash_is_the_sha256_of_the_token_bytes() {
        assert_eq!(
            hex::encode(hash_token("abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
