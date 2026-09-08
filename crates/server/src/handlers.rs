//! Identity registration and login.
//!
//! Two independent proofs happen at registration, never conflated (see
//! `crates/server/src/auth.rs` and `docs/architecture/identity.md`): a
//! WebAuthn ceremony (proves control of a passkey, gates interactive login
//! from here on) and a detached Ed25519 signature over the intended
//! `identity.created` event (proves the identity itself — not the node —
//! authored its own creation). Login is a WebAuthn ceremony alone; nothing
//! about logging in authors a durable event, so there's nothing for the
//! Ed25519 key to sign there.
//!
//! Uses runtime-checked `sqlx::query` throughout, not the `query!` macro —
//! same reasoning `avalon-chain`'s postgres.rs already documents for itself:
//! this shape is still evolving (issues #86, #38 both touch it next), and
//! the macro's compile-time schema check would require a live, migrated
//! database on every machine that so much as runs `cargo check`.

use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::ids::GlobalId;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;
use webauthn_rs::prelude::*;

use crate::auth::{generate_session_token, verify_event_signature};
use crate::error::AppError;
use crate::outbox;
use crate::state::AppState;

const SESSION_LIFETIME_DAYS: i64 = 30;
const CEREMONY_TTL_MINUTES: i64 = 5;

fn bearer_token(headers: &HeaderMap) -> Result<&str, AppError> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(AppError::Unauthorized)
}

/// Validates the bearer token against the `sessions` table and returns the
/// identity it belongs to. A missing, unknown, or expired token is always
/// `AppError::Unauthorized` — never distinguished in the response, so a
/// caller can't probe for which tokens once existed.
pub(crate) async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<Uuid, AppError> {
    authenticate_token(state, bearer_token(headers)?).await
}

/// The same check as [`authenticate`], taking the token directly rather than
/// pulling it from an `Authorization` header — for the one caller that
/// can't send that header at all: a browser's `WebSocket` constructor has no
/// way to set custom headers on the handshake request, so
/// `presence::presence_ws` authenticates off a `?token=` query parameter
/// instead. Every other route keeps using [`authenticate`]; this exists
/// only because the websocket upgrade genuinely can't.
pub(crate) async fn authenticate_token(state: &AppState, token: &str) -> Result<Uuid, AppError> {
    let row = sqlx::query("SELECT identity_id, expires_at FROM sessions WHERE token = $1")
        .bind(token)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::Unauthorized)?;

    let expires_at: OffsetDateTime = row.try_get("expires_at")?;
    if expires_at < OffsetDateTime::now_utc() {
        return Err(AppError::Unauthorized);
    }
    Ok(row.try_get("identity_id")?)
}

/// The exact bytes an `identity.created` claim's Ed25519 signature covers —
/// deliberately a small, explicit, versioned format rather than the full
/// `ProtocolEvent` envelope: `id`/`timestamp` are server-assigned metadata,
/// not something the identity itself is attesting to, so they're never part
/// of what gets signed. Both the client (signing) and the server
/// (verifying) must construct this identically.
fn identity_created_signing_bytes(identity_id: Uuid, display_name: &str) -> Vec<u8> {
    format!("avalon:identity.created:v1:{identity_id}:{display_name}").into_bytes()
}

/// Generates a 4-digit discriminator making `(display_name, discriminator)`
/// unique — the pair is what a friend handle (issue #128) actually is, e.g.
/// `alice#4821`. Retries on collision rather than failing immediately: with
/// ~10000 possible values per display name, a handful of retries only ever
/// matters once a single name is genuinely crowded.
async fn generate_unique_discriminator(
    state: &AppState,
    display_name: &str,
) -> Result<String, AppError> {
    use rand::Rng;
    const MAX_ATTEMPTS: u32 = 20;
    for _ in 0..MAX_ATTEMPTS {
        // `thread_rng()` is `!Send` and must not live across an `.await` —
        // dropping it within this statement (rather than binding it once
        // outside the loop) keeps this function's future `Send`, which
        // axum's `Handler` bound requires of every route it's awaited from.
        let candidate = format!("{:04}", rand::thread_rng().gen_range(0..10000));
        let taken =
            sqlx::query("SELECT 1 FROM profiles WHERE display_name = $1 AND discriminator = $2")
                .bind(display_name)
                .bind(&candidate)
                .fetch_optional(&state.pool)
                .await?;
        if taken.is_none() {
            return Ok(candidate);
        }
    }
    Err(AppError::HandleGenerationFailed)
}

/// Ceremony state persisted between `register/start` and `register/finish`
/// — bundles webauthn-rs's own `PasskeyRegistration` state with the
/// identity id/display name the client already committed to at `start`, so
/// `finish` doesn't need the client to resend them.
#[derive(Serialize, Deserialize)]
struct RegistrationCeremonyState {
    identity_id: Uuid,
    display_name: String,
    webauthn_state: PasskeyRegistration,
}

#[derive(Deserialize)]
pub struct RegisterStartRequest {
    /// Client-chosen, not server-assigned — identity is a wallet its holder
    /// creates themselves. Must also become the WebAuthn user handle, which
    /// is why it has to be decided here rather than at `finish`.
    pub identity_id: Uuid,
    pub display_name: String,
}

#[derive(Serialize)]
pub struct RegisterStartResponse {
    pub ticket_id: Uuid,
    pub challenge: CreationChallengeResponse,
}

pub async fn register_start(
    State(state): State<AppState>,
    Json(body): Json<RegisterStartRequest>,
) -> Result<Json<RegisterStartResponse>, AppError> {
    let existing = sqlx::query("SELECT 1 FROM identities WHERE id = $1")
        .bind(body.identity_id)
        .fetch_optional(&state.pool)
        .await?;
    if existing.is_some() {
        return Err(AppError::IdentityIdTaken);
    }

    // WebAuthn's `user.name` (2nd param) is what password managers key off of
    // for the credential's "username" — distinct from `user.displayName`
    // (3rd param), the purely cosmetic label. Login here is identity-id-first
    // (see this module's own docs above), so `user.name` must be the
    // identity id itself, not the display name — passing the display name
    // for both, as an earlier version of this did, made a password manager
    // save the display name as the login credential, which is wrong: the
    // display name is never enough to log in with.
    let (challenge, webauthn_state) = state
        .webauthn
        .start_passkey_registration(
            body.identity_id,
            &body.identity_id.to_string(),
            &body.display_name,
            None,
        )
        .map_err(|_| AppError::WebauthnFailed)?;

    let ceremony = RegistrationCeremonyState {
        identity_id: body.identity_id,
        display_name: body.display_name,
        webauthn_state,
    };
    let ticket_id = Uuid::new_v4();
    let expires_at = OffsetDateTime::now_utc() + time::Duration::minutes(CEREMONY_TTL_MINUTES);

    sqlx::query(
        "INSERT INTO webauthn_ceremonies (id, kind, state, expires_at) VALUES ($1, 'registration', $2, $3)",
    )
    .bind(ticket_id)
    .bind(serde_json::to_value(&ceremony).expect("ceremony state should serialize"))
    .bind(expires_at)
    .execute(&state.pool)
    .await?;

    Ok(Json(RegisterStartResponse {
        ticket_id,
        challenge,
    }))
}

#[derive(Deserialize)]
pub struct RegisterFinishRequest {
    pub ticket_id: Uuid,
    pub webauthn_credential: RegisterPublicKeyCredential,
    /// Base64-encoded raw Ed25519 public key — the identity's event-signing
    /// key, distinct from the WebAuthn passkey above. See module docs.
    pub event_signing_public_key: String,
    /// Base64-encoded Ed25519 signature over
    /// `identity_created_signing_bytes(identity_id, display_name)`.
    pub event_signature: String,
}

#[derive(Serialize)]
pub struct RegisterFinishResponse {
    pub identity_id: Uuid,
}

pub async fn register_finish(
    State(state): State<AppState>,
    Json(body): Json<RegisterFinishRequest>,
) -> Result<Json<RegisterFinishResponse>, AppError> {
    // Single-use: the DELETE...RETURNING both fetches and consumes the
    // ceremony row atomically, so the same ticket can't be replayed.
    let row = sqlx::query(
        "DELETE FROM webauthn_ceremonies WHERE id = $1 AND kind = 'registration' RETURNING state, expires_at",
    )
    .bind(body.ticket_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::CeremonyNotFound)?;

    let expires_at: OffsetDateTime = row.try_get("expires_at")?;
    if expires_at < OffsetDateTime::now_utc() {
        return Err(AppError::CeremonyExpired);
    }
    let state_json: serde_json::Value = row.try_get("state")?;
    let ceremony: RegistrationCeremonyState =
        serde_json::from_value(state_json).map_err(|_| AppError::CeremonyNotFound)?;

    let passkey = state
        .webauthn
        .finish_passkey_registration(&body.webauthn_credential, &ceremony.webauthn_state)
        .map_err(|_| AppError::WebauthnFailed)?;

    let public_key_bytes = BASE64
        .decode(&body.event_signing_public_key)
        .map_err(|_| AppError::InvalidEventSignature)?;
    let signature_bytes = BASE64
        .decode(&body.event_signature)
        .map_err(|_| AppError::InvalidEventSignature)?;
    let signing_bytes =
        identity_created_signing_bytes(ceremony.identity_id, &ceremony.display_name);
    if !verify_event_signature(&public_key_bytes, &signing_bytes, &signature_bytes) {
        return Err(AppError::InvalidEventSignature);
    }

    let passkey_json = serde_json::to_value(&passkey).expect("Passkey should serialize");
    let credential_id: &[u8] = passkey.cred_id().as_ref();

    // Self-attributed, not network-attributed: the identity signed its own
    // creation, so the ledger entry's issuer says so — a hosted node cannot
    // fabricate this the way it could when the server itself was the
    // issuer of record. See docs/architecture/security-model.md.
    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "identity.created".to_string(),
        issuer: GlobalId::new(
            "identity",
            &ceremony.identity_id.to_string(),
            "self",
            "created",
        ),
        subject: GlobalId::new(
            "identity",
            &ceremony.identity_id.to_string(),
            "self",
            "created",
        ),
        payload: serde_json::json!({ "identity_id": ceremony.identity_id }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    };

    let mut tx = state.pool.begin().await?;

    let insert_identity = sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(ceremony.identity_id)
        .execute(&mut *tx)
        .await;
    if let Err(sqlx::Error::Database(db_err)) = &insert_identity {
        if db_err.is_unique_violation() {
            return Err(AppError::IdentityIdTaken);
        }
    }
    insert_identity?;

    let discriminator = generate_unique_discriminator(&state, &ceremony.display_name).await?;
    sqlx::query(
        "INSERT INTO profiles (identity_id, display_name, discriminator) VALUES ($1, $2, $3)",
    )
    .bind(ceremony.identity_id)
    .bind(&ceremony.display_name)
    .bind(&discriminator)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO identity_keys (identity_id, credential_id, passkey_data) VALUES ($1, $2, $3)",
    )
    .bind(ceremony.identity_id)
    .bind(credential_id)
    .bind(&passkey_json)
    .execute(&mut *tx)
    .await?;

    sqlx::query("INSERT INTO identity_signing_keys (identity_id, public_key) VALUES ($1, $2)")
        .bind(ceremony.identity_id)
        .bind(&public_key_bytes)
        .execute(&mut *tx)
        .await?;

    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(RegisterFinishResponse {
        identity_id: ceremony.identity_id,
    }))
}

/// Ceremony state persisted between `sessions/start` and `sessions/finish`.
///
/// Not discoverable/usernameless: `webauthn-rs`'s convenience
/// `start_passkey_registration` hardcodes `require_resident_key(false)`, so
/// a passkey created through it is never stored as a resident/discoverable
/// credential — discoverable login needs *attested resident keys*
/// specifically (a heavier, attestation-verifying registration path this
/// milestone doesn't build). `identity_id` is therefore still required here
/// to know which candidate passkeys to offer, same as any WebAuthn-as-
/// second-factor deployment — the meaningful win over the password design
/// it replaces is that there is still no shared secret and still a real
/// challenge-response proof, not that no identifier is ever asked for.
#[derive(Serialize, Deserialize)]
struct AuthenticationCeremonyState {
    identity_id: Uuid,
    webauthn_state: PasskeyAuthentication,
}

#[derive(Deserialize)]
pub struct SessionStartRequest {
    pub identity_id: Uuid,
}

#[derive(Serialize)]
pub struct SessionStartResponse {
    pub ticket_id: Uuid,
    pub challenge: RequestChallengeResponse,
}

async fn fetch_passkeys(state: &AppState, identity_id: Uuid) -> Result<Vec<Passkey>, AppError> {
    let key_rows = sqlx::query("SELECT passkey_data FROM identity_keys WHERE identity_id = $1")
        .bind(identity_id)
        .fetch_all(&state.pool)
        .await?;
    if key_rows.is_empty() {
        return Err(AppError::WebauthnFailed);
    }
    let mut passkeys = Vec::with_capacity(key_rows.len());
    for row in &key_rows {
        let data: serde_json::Value = row.try_get("passkey_data")?;
        let passkey: Passkey =
            serde_json::from_value(data).map_err(|_| AppError::WebauthnFailed)?;
        passkeys.push(passkey);
    }
    Ok(passkeys)
}

pub async fn session_start(
    State(state): State<AppState>,
    Json(body): Json<SessionStartRequest>,
) -> Result<Json<SessionStartResponse>, AppError> {
    let passkeys = fetch_passkeys(&state, body.identity_id).await?;

    let (challenge, webauthn_state) = state
        .webauthn
        .start_passkey_authentication(&passkeys)
        .map_err(|_| AppError::WebauthnFailed)?;

    let ceremony = AuthenticationCeremonyState {
        identity_id: body.identity_id,
        webauthn_state,
    };
    let ticket_id = Uuid::new_v4();
    let expires_at = OffsetDateTime::now_utc() + time::Duration::minutes(CEREMONY_TTL_MINUTES);

    sqlx::query(
        "INSERT INTO webauthn_ceremonies (id, kind, state, expires_at) VALUES ($1, 'authentication', $2, $3)",
    )
    .bind(ticket_id)
    .bind(serde_json::to_value(&ceremony).expect("ceremony state should serialize"))
    .bind(expires_at)
    .execute(&state.pool)
    .await?;

    Ok(Json(SessionStartResponse {
        ticket_id,
        challenge,
    }))
}

#[derive(Deserialize)]
pub struct SessionFinishRequest {
    pub ticket_id: Uuid,
    pub credential: PublicKeyCredential,
}

#[derive(Serialize)]
pub struct SessionFinishResponse {
    pub token: String,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

pub async fn session_finish(
    State(state): State<AppState>,
    Json(body): Json<SessionFinishRequest>,
) -> Result<Json<SessionFinishResponse>, AppError> {
    let row = sqlx::query(
        "DELETE FROM webauthn_ceremonies WHERE id = $1 AND kind = 'authentication' RETURNING state, expires_at",
    )
    .bind(body.ticket_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::CeremonyNotFound)?;

    let expires_at: OffsetDateTime = row.try_get("expires_at")?;
    if expires_at < OffsetDateTime::now_utc() {
        return Err(AppError::CeremonyExpired);
    }
    let state_json: serde_json::Value = row.try_get("state")?;
    let ceremony: AuthenticationCeremonyState =
        serde_json::from_value(state_json).map_err(|_| AppError::CeremonyNotFound)?;
    let identity_id = ceremony.identity_id;

    let mut passkeys = fetch_passkeys(&state, identity_id).await?;

    let auth_result = state
        .webauthn
        .finish_passkey_authentication(&body.credential, &ceremony.webauthn_state)
        .map_err(|_| AppError::WebauthnFailed)?;

    // Persist whichever passkey's counter/backup-state actually changed —
    // clone detection depends on this being kept current.
    for passkey in &mut passkeys {
        if passkey.update_credential(&auth_result).unwrap_or(false) {
            let updated = serde_json::to_value(&*passkey).expect("Passkey should serialize");
            let credential_id: &[u8] = passkey.cred_id().as_ref();
            sqlx::query("UPDATE identity_keys SET passkey_data = $1 WHERE credential_id = $2")
                .bind(&updated)
                .bind(credential_id)
                .execute(&state.pool)
                .await?;
        }
    }

    let token = generate_session_token();
    let session_expires_at =
        OffsetDateTime::now_utc() + time::Duration::days(SESSION_LIFETIME_DAYS);

    sqlx::query("INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)")
        .bind(&token)
        .bind(identity_id)
        .bind(session_expires_at)
        .execute(&state.pool)
        .await?;

    Ok(Json(SessionFinishResponse {
        token,
        expires_at: session_expires_at,
    }))
}

#[derive(Serialize)]
pub struct ProfileResponse {
    pub identity_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub identity_created_at: OffsetDateTime,
    pub display_name: String,
    pub avatar_url: Option<String>,
    /// `display_name#discriminator` — the short handle issue #128 added for
    /// adding friends without pasting a raw identity id. Derived, not
    /// stored: always computed from the two columns so it can never drift
    /// out of sync with a display-name change.
    pub handle: String,
}

fn profile_row_to_response(
    identity_id: Uuid,
    row: &sqlx::postgres::PgRow,
) -> Result<ProfileResponse, AppError> {
    let display_name: String = row.try_get("display_name")?;
    let discriminator: String = row.try_get("discriminator")?;
    Ok(ProfileResponse {
        identity_id,
        identity_created_at: row.try_get("created_at")?,
        handle: format!("{display_name}#{discriminator}"),
        display_name,
        avatar_url: row.try_get("avatar_url")?,
    })
}

pub async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ProfileResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let row = sqlx::query(
        r#"
        SELECT p.display_name, p.discriminator, p.avatar_url, i.created_at
        FROM profiles p
        JOIN identities i ON i.id = p.identity_id
        WHERE p.identity_id = $1
        "#,
    )
    .bind(identity_id)
    .fetch_one(&state.pool)
    .await?;

    Ok(Json(profile_row_to_response(identity_id, &row)?))
}

/// One event from the caller's own protocol history (issue #121) — "what
/// does the network know about me." `subject` is included since not every
/// event an identity issues is *about* itself the same way (e.g.
/// `friend.requested` is issued by the requester but its subject is the
/// recipient); `payload` is passed through as-is rather than reduced to a
/// canned summary string, matching this repo's general preference for
/// exposing real data over a lossy client-unfriendly-format-agnostic gloss.
#[derive(Serialize)]
pub struct HistoryEntryResponse {
    pub event_id: Uuid,
    pub kind: String,
    pub subject: String,
    pub payload: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
}

/// Only the caller's own events, never another identity's — enforced by
/// construction, not by a filter a caller could omit: the issuer prefix is
/// always built from the authenticated identity id here, never accepted as
/// a request parameter. Reads the ledger directly (issuer-filtered, see
/// `avalon_chain::PostgresSettlementProvider::list_entries_for_issuer_prefix`)
/// rather than through the indexer — the indexer (`crates/indexer`) is still
/// scaffolding, not a real projection store yet, so a ledger read is the
/// only real read path that exists today. Revisit once #42/#43 land; see
/// docs/architecture/query-and-indexing.md.
pub async fn my_history(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<HistoryEntryResponse>>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;
    let issuer_prefix = format!("identity:{identity_id}:self:");

    let entries = state
        .chain
        .list_entries_for_issuer_prefix(&issuer_prefix)
        .await?;

    Ok(Json(
        entries
            .into_iter()
            .map(|e| HistoryEntryResponse {
                event_id: e.event_id,
                kind: e.kind,
                subject: e.subject,
                payload: e.payload,
                timestamp: e.event_timestamp,
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct UpdateProfileRequest {
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

/// Nothing renders `avatar_url` as an actual image anywhere in the Hub
/// today, so there's no live XSS path yet — but that's incidental, not a
/// guarantee: the field exists so a player-supplied avatar shows up
/// somewhere later (friends list, guild roster), and an unvalidated
/// `javascript:`/`data:`-scheme string sitting in storage is exactly the
/// kind of thing that becomes a real problem the moment something renders
/// it with `<img :src>` without re-checking this. Validated before the
/// `UPDATE profiles` write, never after.
const MAX_AVATAR_URL_LEN: usize = 2048;

/// An empty string is treated as "clear the avatar" (stored as `NULL`), not
/// rejected — the Hub's `Profile.vue` always sends this field as a plain
/// string, using empty to mean "no avatar" rather than omitting the field
/// the way a bare `None` does at the wire level (see `update_profile`'s use
/// of this alongside `avatar_url_provided`). A non-empty value must be an
/// `http`/`https` URL within the length cap, or the request is rejected —
/// never silently stored, never silently stripped.
fn validate_avatar_url(avatar_url: &str) -> Result<Option<String>, AppError> {
    if avatar_url.is_empty() {
        return Ok(None);
    }
    if avatar_url.len() > MAX_AVATAR_URL_LEN {
        return Err(AppError::InvalidAvatarUrl);
    }
    let parsed = Url::parse(avatar_url).map_err(|_| AppError::InvalidAvatarUrl)?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(AppError::InvalidAvatarUrl);
    }
    Ok(Some(avatar_url.to_string()))
}

/// A display-name change can collide with someone else's existing handle
/// (same name, same discriminator) — the discriminator itself never changes
/// on its own, but if the *new* name collides under it, a fresh one has to
/// be picked so `(display_name, discriminator)` stays unique. No collision
/// (the common case) keeps the identity's existing discriminator, so a
/// player's handle doesn't churn just because they tweaked their name.
async fn discriminator_for_rename(
    state: &AppState,
    identity_id: Uuid,
    new_display_name: &str,
) -> Result<String, AppError> {
    let row = sqlx::query("SELECT discriminator FROM profiles WHERE identity_id = $1")
        .bind(identity_id)
        .fetch_one(&state.pool)
        .await?;
    let current: String = row.try_get("discriminator")?;

    let taken = sqlx::query(
        "SELECT 1 FROM profiles WHERE display_name = $1 AND discriminator = $2 AND identity_id <> $3",
    )
    .bind(new_display_name)
    .bind(&current)
    .bind(identity_id)
    .fetch_optional(&state.pool)
    .await?;

    if taken.is_none() {
        Ok(current)
    } else {
        generate_unique_discriminator(state, new_display_name).await
    }
}

pub async fn update_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UpdateProfileRequest>,
) -> Result<Json<ProfileResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let discriminator = match &body.display_name {
        Some(new_name) => Some(discriminator_for_rename(&state, identity_id, new_name).await?),
        None => None,
    };

    // Three states, not two: `avatar_url` omitted (leave the column alone),
    // provided as `""` (clear it to NULL), or provided as a real value
    // (validate, then set it). A single `COALESCE($n, ...)` bind can't tell
    // "omitted" apart from "explicitly clear" — both are SQL NULL — so
    // `avatar_url_provided` carries that distinction into the query
    // separately from the (possibly NULL) value itself.
    let avatar_url_provided = body.avatar_url.is_some();
    let avatar_url = match &body.avatar_url {
        Some(raw) => validate_avatar_url(raw)?,
        None => None,
    };

    let row = sqlx::query(
        r#"
        UPDATE profiles p
        SET display_name = COALESCE($2, p.display_name),
            avatar_url = CASE WHEN $5 THEN $3 ELSE p.avatar_url END,
            discriminator = COALESCE($4, p.discriminator)
        FROM identities i
        WHERE p.identity_id = $1 AND i.id = p.identity_id
        RETURNING p.display_name, p.discriminator, p.avatar_url, i.created_at
        "#,
    )
    .bind(identity_id)
    .bind(body.display_name)
    .bind(avatar_url)
    .bind(discriminator)
    .bind(avatar_url_provided)
    .fetch_one(&state.pool)
    .await?;

    Ok(Json(profile_row_to_response(identity_id, &row)?))
}

#[cfg(test)]
mod tests {
    //! Pure validation logic only — no live Postgres reachable here. The
    //! full `PATCH /me` request/response round trip (including the
    //! avatar-url-clearing three-state SQL) is covered by
    //! `crates/server/tests/friends.rs`-style `--ignored` integration
    //! coverage where it exists; this module exercises
    //! `validate_avatar_url` directly since it's a pure function.

    use super::*;

    #[test]
    fn accepts_a_valid_https_url() {
        assert_eq!(
            validate_avatar_url("https://example.com/avatar.png").unwrap(),
            Some("https://example.com/avatar.png".to_string())
        );
    }

    #[test]
    fn accepts_a_valid_http_url() {
        assert_eq!(
            validate_avatar_url("http://example.com/avatar.png").unwrap(),
            Some("http://example.com/avatar.png".to_string())
        );
    }

    #[test]
    fn empty_string_means_clear_the_avatar_not_an_error() {
        assert_eq!(validate_avatar_url("").unwrap(), None);
    }

    #[test]
    fn rejects_a_javascript_scheme() {
        assert!(matches!(
            validate_avatar_url("javascript:alert(1)"),
            Err(AppError::InvalidAvatarUrl)
        ));
    }

    #[test]
    fn rejects_a_data_scheme() {
        assert!(matches!(
            validate_avatar_url("data:text/html,<script>alert(1)</script>"),
            Err(AppError::InvalidAvatarUrl)
        ));
    }

    #[test]
    fn rejects_a_string_that_is_not_a_url_at_all() {
        assert!(matches!(
            validate_avatar_url("not a url"),
            Err(AppError::InvalidAvatarUrl)
        ));
    }

    #[test]
    fn rejects_an_over_length_url() {
        let overlong = format!("https://example.com/{}", "a".repeat(MAX_AVATAR_URL_LEN));
        assert!(matches!(
            validate_avatar_url(&overlong),
            Err(AppError::InvalidAvatarUrl)
        ));
    }

    #[test]
    fn accepts_a_url_exactly_at_the_length_cap() {
        let path_len = MAX_AVATAR_URL_LEN - "https://example.com/".len();
        let exactly_at_cap = format!("https://example.com/{}", "a".repeat(path_len));
        assert_eq!(exactly_at_cap.len(), MAX_AVATAR_URL_LEN);
        assert!(validate_avatar_url(&exactly_at_cap).is_ok());
    }
}
