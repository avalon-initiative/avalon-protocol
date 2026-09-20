//! Cross-node login pending-request lifecycle and verification — epic #623,
//! issue #634, the server-side counterpart of
//! `avalon_protocol::cross_node_login::CrossNodeLoginGrant`. See that
//! module's doc comment for the wire shape and the security posture this
//! preserves.
//!
//! Structurally close to `crate::device_pairing`'s create/poll/approve
//! shape, but genuinely cross-node: approval never requires a live session
//! on *this* (the requesting) node at all, unlike #307's same-node pairing
//! — the grant is signed and submitted from wherever the identity's own
//! signing key lives, which may be a browser tab that has never talked to
//! this node before.
//!
//! **Same-device fast path**: if the client attempting login already holds
//! the identity's signing key locally, it can skip `start`/`poll` entirely
//! and call [`submit`] directly with a grant it minted itself — the
//! `start`+poll dance below exists only for the cross-device case (an
//! unfamiliar browser, a console, a friend's machine), same distinction
//! epic #623's own scope note draws.

use avalon_indexer::projections::identity_signing_keys;
use avalon_protocol::cross_node_login::CrossNodeLoginGrant;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::auth::{generate_session_token, verify_event_signature};
use crate::error::AppError;
use crate::state::AppState;

const REQUEST_TTL_MINUTES: i64 = 10;
/// Same lifetime `device_pairing::approve_pairing` mints — a session minted
/// through cross-node login is an ordinary session in every respect.
const SESSION_LIFETIME_DAYS: i64 = 30;
const POLL_MIN_INTERVAL_SECONDS: i64 = 5;
/// Same unambiguous-glyph alphabet `device_pairing::USER_CODE_ALPHABET` uses.
const USER_CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const USER_CODE_LEN: usize = 8;
/// Same allowance `crate::continuation::CLOCK_SKEW_ALLOWANCE` documents.
const CLOCK_SKEW_ALLOWANCE: Duration = Duration::seconds(5);

fn generate_user_code() -> String {
    let mut rng = rand::rng();
    (0..USER_CODE_LEN)
        .map(|_| USER_CODE_ALPHABET[rng.random_range(0..USER_CODE_ALPHABET.len())] as char)
        .collect()
}

fn bearer_token(headers: &HeaderMap) -> Result<&str, AppError> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(AppError::Unauthorized)
}

/// This node's own advertised base URL — what a submitted grant's
/// `destination_base_url` is checked against. A node with no configured
/// `own_base_url` can't participate in cross-node login at all (there'd be
/// nothing for a remote approver to bind their approval to).
fn own_base_url(state: &AppState) -> Result<&str, AppError> {
    state.own_base_url.as_deref().ok_or(AppError::Unauthorized)
}

#[derive(Serialize)]
pub struct StartCrossNodeLoginResponse {
    pub request_code: String,
    pub user_code: String,
    pub requesting_context: String,
    pub expires_in: i64,
    pub poll_interval: i64,
}

/// `POST /auth/cross-node/start` — unauthenticated, called on the
/// requesting node. Mints an opaque `request_code` (known only to this
/// client and this server) and a short human-typeable `user_code` (shown
/// as a QR code / typed on the approving device), and stores a pending row
/// binding this node's own `base_url` into what the approver will
/// eventually sign over.
pub async fn start(
    State(state): State<AppState>,
) -> Result<Json<StartCrossNodeLoginResponse>, AppError> {
    const MAX_ATTEMPTS: u32 = 20;
    let base_url = own_base_url(&state)?.to_string();
    let requesting_context = base_url.clone();
    let now = OffsetDateTime::now_utc();
    let expires_at = now + Duration::minutes(REQUEST_TTL_MINUTES);

    for _ in 0..MAX_ATTEMPTS {
        let request_code = generate_session_token();
        let user_code = generate_user_code();

        let inserted = sqlx::query(
            r#"
            INSERT INTO cross_node_login_requests
                (request_code, user_code, status, requesting_base_url, expires_at)
            VALUES ($1, $2, 'pending', $3, $4)
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(&request_code)
        .bind(&user_code)
        .bind(&base_url)
        .bind(expires_at)
        .execute(&state.pool)
        .await?;

        if inserted.rows_affected() == 1 {
            return Ok(Json(StartCrossNodeLoginResponse {
                request_code,
                user_code,
                requesting_context,
                expires_in: REQUEST_TTL_MINUTES * 60,
                poll_interval: POLL_MIN_INTERVAL_SECONDS,
            }));
        }
    }
    Err(AppError::CrossNodeLoginRequestCodeGenerationFailed)
}

#[derive(Deserialize)]
pub struct LookupQuery {
    pub user_code: String,
}

#[derive(Serialize)]
pub struct LookupCrossNodeLoginResponse {
    /// One of `pending`, `denied`, `expired`, `approved` — an approval
    /// screen only ever meaningfully acts on `pending`; the others let it
    /// show a clear "this code was already used/expired" state instead of
    /// a generic not-found.
    pub status: String,
    pub requesting_context: String,
    pub expires_in: i64,
}

/// `GET /auth/cross-node/lookup?user_code=...` — unauthenticated, epic
/// #623 issue #639's own gap: the Hub/mobile-hub approval screen has to
/// show real context (#642's decided phishing-context requirement)
/// *before* a human decides whether to approve, but `submit`/`deny` only
/// ever take a `user_code` with no read path to go with it. Deliberately
/// returns nothing beyond what's needed to render the prompt — never
/// `request_code` (the polling device's own bearer credential, not the
/// approver's business).
pub async fn lookup(
    State(state): State<AppState>,
    Query(query): Query<LookupQuery>,
) -> Result<Json<LookupCrossNodeLoginResponse>, AppError> {
    let now = OffsetDateTime::now_utc();
    let row = sqlx::query(
        "SELECT status, requesting_base_url, expires_at FROM cross_node_login_requests WHERE user_code = $1",
    )
    .bind(&query.user_code)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::CrossNodeLoginRequestNotFound)?;

    let mut status: String = row.try_get("status")?;
    let requesting_base_url: String = row.try_get("requesting_base_url")?;
    let expires_at: OffsetDateTime = row.try_get("expires_at")?;

    // Same lazy-expiry posture `poll`'s own handler already takes: nothing
    // proactively flips a stale `pending` row to `expired` on a schedule,
    // so a read has to reconcile it itself rather than trust the stored
    // status blindly.
    if status == "pending" && expires_at < now {
        status = "expired".to_string();
    }

    Ok(Json(LookupCrossNodeLoginResponse {
        status,
        requesting_context: requesting_base_url,
        expires_in: (expires_at - now).whole_seconds().max(0),
    }))
}

#[derive(Serialize)]
pub struct PollCrossNodeLoginResponse {
    /// One of `pending`, `slow_down`, `denied`, `expired`, `approved`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
}

fn pending_status(status: &str) -> PollCrossNodeLoginResponse {
    PollCrossNodeLoginResponse {
        status: status.to_string(),
        token: None,
        expires_at: None,
    }
}

/// `POST /auth/cross-node/poll` — unauthenticated; the bearer token here is
/// the opaque `request_code`, not a session. Same single-use-on-approved
/// shape as `device_pairing::poll_pairing`.
pub async fn poll(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PollCrossNodeLoginResponse>, AppError> {
    let request_code = bearer_token(&headers)?;
    let now = OffsetDateTime::now_utc();

    let row = sqlx::query(
        "SELECT status, expires_at, last_polled_at FROM cross_node_login_requests WHERE request_code = $1",
    )
    .bind(request_code)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::Unauthorized)?;

    let status: String = row.try_get("status")?;
    let expires_at: OffsetDateTime = row.try_get("expires_at")?;
    let last_polled_at: Option<OffsetDateTime> = row.try_get("last_polled_at")?;

    if status == "pending" && expires_at < now {
        sqlx::query(
            "UPDATE cross_node_login_requests SET status = 'expired' WHERE request_code = $1 AND status = 'pending'",
        )
        .bind(request_code)
        .execute(&state.pool)
        .await?;
        return Ok(Json(pending_status("expired")));
    }

    match status.as_str() {
        "expired" => Ok(Json(pending_status("expired"))),
        "denied" => Ok(Json(pending_status("denied"))),
        "pending" => {
            sqlx::query(
                "UPDATE cross_node_login_requests SET last_polled_at = $2 WHERE request_code = $1",
            )
            .bind(request_code)
            .bind(now)
            .execute(&state.pool)
            .await?;
            if let Some(last_polled_at) = last_polled_at {
                if now - last_polled_at < Duration::seconds(POLL_MIN_INTERVAL_SECONDS) {
                    return Ok(Json(pending_status("slow_down")));
                }
            }
            Ok(Json(pending_status("pending")))
        }
        "approved" => {
            let consumed = sqlx::query(
                r#"
                UPDATE cross_node_login_requests
                SET status = 'expired'
                WHERE request_code = $1 AND status = 'approved'
                RETURNING session_token
                "#,
            )
            .bind(request_code)
            .fetch_optional(&state.pool)
            .await?;

            let Some(consumed) = consumed else {
                return Ok(Json(pending_status("expired")));
            };
            let session_token: Option<String> = consumed.try_get("session_token")?;
            let Some(session_token) = session_token else {
                return Ok(Json(pending_status("expired")));
            };

            let session_row = sqlx::query("SELECT expires_at FROM sessions WHERE token = $1")
                .bind(&session_token)
                .fetch_optional(&state.pool)
                .await?;
            let Some(session_row) = session_row else {
                return Ok(Json(pending_status("expired")));
            };
            let session_expires_at: OffsetDateTime = session_row.try_get("expires_at")?;

            Ok(Json(PollCrossNodeLoginResponse {
                status: "approved".to_string(),
                token: Some(session_token),
                expires_at: Some(session_expires_at),
            }))
        }
        other => {
            debug_assert!(
                false,
                "unexpected cross_node_login_requests.status: {other}"
            );
            Ok(Json(pending_status("expired")))
        }
    }
}

#[derive(Deserialize)]
pub struct SubmitGrantRequest {
    /// Present for the cross-device flow: which pending `start`ed request
    /// this grant resolves. `None` for the same-device fast path, which
    /// mints a session directly with no pending row at all.
    #[serde(default)]
    pub user_code: Option<String>,
    pub grant: CrossNodeLoginGrant,
}

#[derive(Serialize)]
pub struct SubmitGrantResponse {
    /// Present only on the same-device fast path (`user_code: None`) — the
    /// cross-device path's session token is picked up via `poll`, same as
    /// `device_pairing::approve_pairing`, not returned here directly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
}

/// Verifies `grant` against `identity_signing_keys`, this node's own
/// `base_url`, and the anti-replay nonce table, returning the identity it
/// authenticates. Every failure is [`AppError::Unauthorized`], same
/// undifferentiated posture `crate::continuation::verify`'s own doc
/// comment already establishes for the same reason (never reveal *why* a
/// bearer credential didn't verify).
async fn verify_grant(state: &AppState, grant: &CrossNodeLoginGrant) -> Result<Uuid, AppError> {
    let now = OffsetDateTime::now_utc();
    if grant.expires_at < now || grant.issued_at > now + CLOCK_SKEW_ALLOWANCE {
        return Err(AppError::Unauthorized);
    }
    if grant.expires_at - grant.issued_at
        > Duration::seconds(avalon_protocol::cross_node_login::DEFAULT_TTL_SECONDS)
    {
        return Err(AppError::Unauthorized);
    }

    // Destination binding (#610's lesson, applied here): a grant approved
    // for a different node must never verify here, even with a perfectly
    // valid signature and nonce.
    if grant.destination_base_url != own_base_url(state)? {
        return Err(AppError::Unauthorized);
    }

    let key = identity_signing_keys::find_active_by_id(&state.pool, grant.signing_key_id)
        .await?
        .ok_or(AppError::Unauthorized)?;
    if key.identity_id != grant.identity_id {
        return Err(AppError::Unauthorized);
    }

    let signature_bytes = hex::decode(&grant.signature).map_err(|_| AppError::Unauthorized)?;
    if !verify_event_signature(&key.public_key, &grant.signing_bytes(), &signature_bytes) {
        return Err(AppError::Unauthorized);
    }

    sqlx::query("DELETE FROM consumed_cross_node_login_nonces WHERE expires_at < now()")
        .execute(&state.pool)
        .await?;
    let inserted = sqlx::query(
        "INSERT INTO consumed_cross_node_login_nonces (nonce, expires_at) VALUES ($1, $2) \
         ON CONFLICT DO NOTHING",
    )
    .bind(grant.nonce)
    .bind(grant.expires_at)
    .execute(&state.pool)
    .await?;
    if inserted.rows_affected() == 0 {
        return Err(AppError::Unauthorized);
    }

    Ok(key.identity_id)
}

/// `POST /auth/cross-node/submit` — unauthenticated (the grant itself is
/// the credential). Mints an ordinary session, same mechanism
/// `device_pairing::approve_pairing` uses. With `user_code` present,
/// attaches the session to that pending request for the waiting client to
/// pick up via `poll`; with `user_code` absent (same-device fast path),
/// returns the session token directly.
pub async fn submit(
    State(state): State<AppState>,
    Json(body): Json<SubmitGrantRequest>,
) -> Result<Json<SubmitGrantResponse>, AppError> {
    let identity_id = verify_grant(&state, &body.grant).await?;

    let token = generate_session_token();
    let session_expires_at = OffsetDateTime::now_utc() + Duration::days(SESSION_LIFETIME_DAYS);

    let mut tx = state.pool.begin().await?;
    sqlx::query("INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)")
        .bind(&token)
        .bind(identity_id)
        .bind(session_expires_at)
        .execute(&mut *tx)
        .await?;

    let Some(user_code) = body.user_code else {
        tx.commit().await?;
        return Ok(Json(SubmitGrantResponse {
            token: Some(token),
            expires_at: Some(session_expires_at),
        }));
    };

    let resolved = sqlx::query(
        r#"
        UPDATE cross_node_login_requests
        SET status = 'approved', identity_id = $2, session_token = $3
        WHERE user_code = $1 AND status = 'pending' AND expires_at > now()
        "#,
    )
    .bind(&user_code)
    .bind(identity_id)
    .bind(&token)
    .execute(&mut *tx)
    .await?;
    if resolved.rows_affected() == 0 {
        // Resolved (approved/denied/expired) by a concurrent request, or
        // the code never matched a pending row at all — either way this
        // freshly-minted session must not be left dangling unattached.
        return Err(AppError::CrossNodeLoginRequestNotFound);
    }

    tx.commit().await?;
    Ok(Json(SubmitGrantResponse {
        token: None,
        expires_at: None,
    }))
}

#[derive(Deserialize)]
pub struct UserCodeRequest {
    pub user_code: String,
}

#[derive(Serialize)]
pub struct DenyResponse {
    pub status: String,
}

/// `POST /auth/cross-node/deny` — deliberately unauthenticated, unlike
/// `device_pairing::deny_pairing`. That module's approver always already
/// holds a session *on the same node* the pairing was started on, so
/// requiring it there is free; here the approver's session (if any) lives
/// wherever the identity's signing key does, which is routinely a
/// *different* node than the one this request was started on — requiring
/// local auth would make denial impossible for the exact case cross-node
/// login exists to handle. Safe to leave unauthenticated regardless: a
/// denial grants nothing, so the worst case of a guessed `user_code` (8
/// chars from a 32-symbol alphabet) is griefing one's own pending request,
/// not a security bypass.
pub async fn deny(
    State(state): State<AppState>,
    Json(body): Json<UserCodeRequest>,
) -> Result<Json<DenyResponse>, AppError> {
    let resolved = sqlx::query(
        r#"
        UPDATE cross_node_login_requests
        SET status = 'denied'
        WHERE user_code = $1 AND status = 'pending' AND expires_at > now()
        "#,
    )
    .bind(&body.user_code)
    .execute(&state.pool)
    .await?;
    if resolved.rows_affected() == 0 {
        return Err(AppError::CrossNodeLoginRequestNotFound);
    }

    Ok(Json(DenyResponse {
        status: "denied".to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_code_uses_only_the_unambiguous_alphabet() {
        for _ in 0..200 {
            let code = generate_user_code();
            assert_eq!(code.len(), USER_CODE_LEN);
            for c in code.chars() {
                assert!(USER_CODE_ALPHABET.contains(&(c as u8)));
                assert!(!"0O1IL".contains(c));
            }
        }
    }
}
