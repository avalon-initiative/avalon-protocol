//! Cross-device pairing: lets a WebAuthn-incapable client (a
//! game engine with no embedded browser, a console, a headless context)
//! bootstrap a real session without ever implementing a WebAuthn ceremony
//! itself.
//!
//! Structurally close to `crate::devices`'s request/approve/poll shape,
//! but a genuinely different problem: that module adds a
//! trusted signing device to an identity that's *already* authenticated
//! somewhere; this module bootstraps a session for a client that has *no*
//! prior session at all. Kept as its own module rather than bolted onto
//! `devices.rs` for that reason.
//!
//! The security boundary is deliberately not the `user_code`'s secrecy —
//! it's that `POST /auth/device/approve` requires the approver's own
//! already-authenticated session, the same `authenticate()` check every
//! other authenticated route in this crate uses. A leaked `user_code`
//! alone, with no logged-in session behind it, can never mint anything.
//! Approval mints an ordinary session via `auth::generate_session_token`/the
//! `sessions` table — not a second, differently-trusted token type.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::generate_session_token;
use crate::error::AppError;
use crate::handlers::authenticate;
use crate::signature_gate::{canonical_message, require_fresh_signature};
use crate::state::AppState;
use utoipa::ToSchema;

const PAIRING_TTL_MINUTES: i64 = 10;
/// Same lifetime `handlers::session_finish` mints for a normal WebAuthn
/// login — a session minted through pairing is an ordinary session in every
/// respect, including how long it lasts.
const SESSION_LIFETIME_DAYS: i64 = 30;
/// The minimum gap a client must leave between polls before getting
/// `slow_down` back — mirrors the standard OAuth device-authorization
/// grant's `slow_down` behavior so a waiting client can't hammer this route.
const POLL_MIN_INTERVAL_SECONDS: i64 = 5;
/// 8 chars from an alphabet with the visually-ambiguous characters
/// (`0`/`O`, `1`/`I`/`L`) removed, so a user can read this off one screen
/// and type it on another without guessing which glyph they saw.
const USER_CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const USER_CODE_LEN: usize = 8;

fn generate_user_code() -> String {
    // `rng()` is `!Send` and must not live across an `.await` — built fresh
    // and dropped within this call, since axum's `Handler` bound requires
    // this function's future to stay `Send`.
    let mut rng = rand::rng();
    (0..USER_CODE_LEN)
        .map(|_| USER_CODE_ALPHABET[rng.random_range(0..USER_CODE_ALPHABET.len())] as char)
        .collect()
}

fn hub_verification_uri(user_code: &str) -> String {
    let hub_origin = std::env::var("AVALON_HUB_ORIGIN")
        .ok()
        .and_then(|origins| origins.split(',').next().map(str::trim).map(str::to_string))
        .unwrap_or_else(|| "http://localhost:5173".to_string());
    format!("{hub_origin}/pair?user_code={user_code}")
}

fn bearer_token(headers: &HeaderMap) -> Result<&str, AppError> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(AppError::Unauthorized)
}

#[derive(Serialize, ToSchema)]
pub struct StartPairingResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: i64,
    pub poll_interval: i64,
}

/// `POST /auth/device/start` — unauthenticated. The waiting client's
/// entrypoint: mints an opaque `device_code` (known only to this client and
/// the server, never shown to the user) and a short human-typeable
/// `user_code` (shown to the user, e.g. as a QR code), and stores a
/// pending pairing row. Retries on a code collision — with 32^8 possible
/// `user_code`s this only ever matters once a huge number are pending at
/// once.
#[utoipa::path(
    post,
    path = "/auth/device/start",
    tag = "devices",
    responses((status = 200, body = StartPairingResponse)),
)]
pub async fn start_pairing(
    State(state): State<AppState>,
) -> Result<Json<StartPairingResponse>, AppError> {
    const MAX_ATTEMPTS: u32 = 20;
    let now = OffsetDateTime::now_utc();
    let expires_at = now + time::Duration::minutes(PAIRING_TTL_MINUTES);

    for _ in 0..MAX_ATTEMPTS {
        let device_code = generate_session_token();
        let user_code = generate_user_code();

        let inserted = sqlx::query(
            r#"
            INSERT INTO device_pairings (device_code, user_code, status, expires_at)
            VALUES ($1, $2, 'pending', $3)
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(&device_code)
        .bind(&user_code)
        .bind(expires_at)
        .execute(&state.pool)
        .await?;

        if inserted.rows_affected() == 1 {
            return Ok(Json(StartPairingResponse {
                device_code,
                verification_uri: hub_verification_uri(&user_code),
                user_code,
                expires_in: PAIRING_TTL_MINUTES * 60,
                poll_interval: POLL_MIN_INTERVAL_SECONDS,
            }));
        }
    }
    Err(AppError::DevicePairingCodeGenerationFailed)
}

#[derive(Serialize, ToSchema)]
pub struct PollPairingResponse {
    /// One of `pending`, `slow_down`, `denied`, `expired`, `approved`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = "date-time")]
    pub expires_at: Option<OffsetDateTime>,
}

fn pending_status(status: &str) -> PollPairingResponse {
    PollPairingResponse {
        status: status.to_string(),
        token: None,
        expires_at: None,
    }
}

/// `POST /auth/device/poll` — unauthenticated; the bearer token here is the
/// opaque `device_code` itself, not a session. Single-use on `approved`:
/// the winning poll atomically flips the row to `expired` in the same
/// `UPDATE ... RETURNING` that reads the session, so a concurrent or later
/// poll of the same `device_code` can never observe the token twice.
#[utoipa::path(
    post,
    path = "/auth/device/poll",
    tag = "devices",
    responses((status = 200, body = PollPairingResponse)),
)]
pub async fn poll_pairing(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PollPairingResponse>, AppError> {
    let device_code = bearer_token(&headers)?;
    let now = OffsetDateTime::now_utc();

    let row = sqlx::query(
        "SELECT status, expires_at, last_polled_at FROM device_pairings WHERE device_code = $1",
    )
    .bind(device_code)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::Unauthorized)?;

    let status: String = row.try_get("status")?;
    let expires_at: OffsetDateTime = row.try_get("expires_at")?;
    let last_polled_at: Option<OffsetDateTime> = row.try_get("last_polled_at")?;

    if status == "pending" && expires_at < now {
        sqlx::query(
            "UPDATE device_pairings SET status = 'expired' WHERE device_code = $1 AND status = 'pending'",
        )
        .bind(device_code)
        .execute(&state.pool)
        .await?;
        return Ok(Json(pending_status("expired")));
    }

    match status.as_str() {
        "expired" => Ok(Json(pending_status("expired"))),
        "denied" => Ok(Json(pending_status("denied"))),
        "pending" => {
            sqlx::query("UPDATE device_pairings SET last_polled_at = $2 WHERE device_code = $1")
                .bind(device_code)
                .bind(now)
                .execute(&state.pool)
                .await?;
            if let Some(last_polled_at) = last_polled_at {
                if now - last_polled_at < time::Duration::seconds(POLL_MIN_INTERVAL_SECONDS) {
                    return Ok(Json(pending_status("slow_down")));
                }
            }
            Ok(Json(pending_status("pending")))
        }
        "approved" => {
            let consumed = sqlx::query(
                r#"
                UPDATE device_pairings
                SET status = 'expired'
                WHERE device_code = $1 AND status = 'approved'
                RETURNING session_token
                "#,
            )
            .bind(device_code)
            .fetch_optional(&state.pool)
            .await?;

            let Some(consumed) = consumed else {
                // Lost a race against a concurrent poll of the same
                // device_code — the other poll already consumed the token.
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

            Ok(Json(PollPairingResponse {
                status: "approved".to_string(),
                token: Some(session_token),
                expires_at: Some(session_expires_at),
            }))
        }
        other => {
            debug_assert!(false, "unexpected device_pairings.status: {other}");
            Ok(Json(pending_status("expired")))
        }
    }
}

#[derive(Deserialize, ToSchema)]
pub struct UserCodeRequest {
    pub user_code: String,
}

/// `POST /auth/device/approve` mints a brand-new, independently-
/// usable session for a different device off nothing but the approver's
/// ambient session, so a signature-required tier closes that
/// gap. `signing_key_id`/`signature` are optional on the wire (so
/// deserialization never fails outright) but enforced as required by
/// [`require_fresh_signature`] below.
#[derive(Deserialize, ToSchema)]
pub struct ApprovePairingRequest {
    pub user_code: String,
    pub signing_key_id: Option<Uuid>,
    pub signature: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ResolvePairingResponse {
    pub status: String,
}

async fn fetch_pending_pairing_id(state: &AppState, user_code: &str) -> Result<Uuid, AppError> {
    let row = sqlx::query(
        "SELECT id, expires_at FROM device_pairings WHERE user_code = $1 AND status = 'pending'",
    )
    .bind(user_code)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::DevicePairingNotFound)?;

    let expires_at: OffsetDateTime = row.try_get("expires_at")?;
    if expires_at < OffsetDateTime::now_utc() {
        return Err(AppError::DevicePairingNotFound);
    }
    Ok(row.try_get("id")?)
}

/// `POST /auth/device/approve` — requires the approver's own existing
/// authenticated session (normal session-bearer auth). Mints a real session
/// for the approver's own identity via `auth::generate_session_token`/the
/// `sessions` table — the exact same mechanism `handlers::session_finish`
/// uses for a normal login — and attaches it to the pairing so the waiting
/// client picks it up on its next poll.
#[utoipa::path(
    post,
    path = "/auth/device/approve",
    tag = "devices",
    request_body = ApprovePairingRequest,
    responses((status = 200, body = ResolvePairingResponse)),
)]
pub async fn approve_pairing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ApprovePairingRequest>,
) -> Result<Json<ResolvePairingResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;
    let pairing_id = fetch_pending_pairing_id(&state, &body.user_code).await?;

    // Proof-of-possession of the identity's own signing key,
    // on top of the ambient session, before minting a second independently-
    // usable session for an entirely different device. Deliberately signs
    // over `user_code`/`identity_id` rather than the server-internal
    // `pairing_id` — the approving client (the Hub) never otherwise learns
    // `pairing_id`, and `user_code` alone already uniquely and non-
    // replayably identifies this one pending pairing (fresh per request,
    // single-use, TTL'd).
    let message = canonical_message(
        "device_pairing.approve",
        &[&identity_id.to_string(), &body.user_code],
    );
    require_fresh_signature(
        &state,
        identity_id,
        &message,
        body.signing_key_id,
        body.signature.as_deref(),
    )
    .await?;

    let token = generate_session_token();
    let session_expires_at =
        OffsetDateTime::now_utc() + time::Duration::days(SESSION_LIFETIME_DAYS);

    let mut tx = state.pool.begin().await?;

    sqlx::query("INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)")
        .bind(&token)
        .bind(identity_id)
        .bind(session_expires_at)
        .execute(&mut *tx)
        .await?;

    let resolved = sqlx::query(
        r#"
        UPDATE device_pairings
        SET status = 'approved', identity_id = $2, session_token = $3
        WHERE id = $1 AND status = 'pending'
        "#,
    )
    .bind(pairing_id)
    .bind(identity_id)
    .bind(&token)
    .execute(&mut *tx)
    .await?;
    if resolved.rows_affected() == 0 {
        // Resolved (approved/denied/expired) by a concurrent request
        // between the fetch above and here.
        return Err(AppError::DevicePairingNotFound);
    }

    tx.commit().await?;

    Ok(Json(ResolvePairingResponse {
        status: "approved".to_string(),
    }))
}

/// `POST /auth/device/deny` — same auth shape as [`approve_pairing`], the
/// explicit rejection path. No session is ever minted.
#[utoipa::path(
    post,
    path = "/auth/device/deny",
    tag = "devices",
    request_body = UserCodeRequest,
    responses((status = 200, body = ResolvePairingResponse)),
)]
pub async fn deny_pairing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UserCodeRequest>,
) -> Result<Json<ResolvePairingResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let resolved = sqlx::query(
        r#"
        UPDATE device_pairings
        SET status = 'denied', identity_id = $2
        WHERE user_code = $1 AND status = 'pending' AND expires_at > now()
        "#,
    )
    .bind(&body.user_code)
    .bind(identity_id)
    .execute(&state.pool)
    .await?;
    if resolved.rows_affected() == 0 {
        return Err(AppError::DevicePairingNotFound);
    }

    Ok(Json(ResolvePairingResponse {
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

    #[test]
    fn user_code_has_real_entropy_across_calls() {
        let codes: std::collections::HashSet<String> =
            (0..200).map(|_| generate_user_code()).collect();
        // 200 draws from 32^8 possibilities colliding would indicate a
        // broken generator, not bad luck.
        assert!(codes.len() > 190);
    }

    #[test]
    fn hub_verification_uri_embeds_the_user_code() {
        let uri = hub_verification_uri("ABCD1234");
        assert!(uri.contains("user_code=ABCD1234"));
    }
}
