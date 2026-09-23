//! Identity registration and login.
//!
//! Two independent proofs happen at registration, never conflated (see
//! `crates/server/src/auth.rs` and `docs/projects/backend-server/architecture/identity.md`): a
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

use avalon_indexer::projections::profiles as profile_reads;
use avalon_protocol::event_payloads::{
    IdentityCreatedPayload, IdentityPasskeyRegisteredPayload, IdentitySigningKeyAddedPayload,
    ProfileUpdatedPayload,
};
use avalon_protocol::events::{ProtocolEvent, ProtocolEventKindVariant};
use avalon_protocol::identity::{
    Genre, MAX_BIO_LEN, MAX_FAVORITE_GENRES, MAX_LINKS, MAX_LINK_LEN, MAX_LOCATION_LEN,
    MAX_PRONOUNS_LEN, MAX_STATUS_LEN, MAX_TIMEZONE_LEN,
};
use avalon_protocol::ids::GlobalId;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use url::Url;
use utoipa::ToSchema;
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
///
/// Issue #525: `token` may also be a self-signed session-continuation
/// assertion (`avalon_protocol::continuation::WIRE_PREFIX`-prefixed)
/// instead of an opaque `sessions`-table bearer token — additive, not a
/// replacement; every existing caller of [`authenticate`]/[`authenticate_token`]
/// gets continuation-token support for free, with no per-route changes,
/// since both credential kinds resolve to the same `Uuid` return type.
pub(crate) async fn authenticate_token(state: &AppState, token: &str) -> Result<Uuid, AppError> {
    if let Some(body) = token.strip_prefix(avalon_protocol::continuation::WIRE_PREFIX) {
        return crate::continuation::verify(state, body).await;
    }

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

#[derive(Deserialize, ToSchema)]
pub struct RegisterStartRequest {
    /// Client-chosen, not server-assigned — identity is a wallet its holder
    /// creates themselves. Must also become the WebAuthn user handle, which
    /// is why it has to be decided here rather than at `finish`.
    pub identity_id: Uuid,
    pub display_name: String,
}

#[derive(Serialize, ToSchema)]
pub struct RegisterStartResponse {
    pub ticket_id: Uuid,
    /// `webauthn-rs`'s own WebAuthn creation-challenge type — opaque here
    /// since it's an external crate's type with no `ToSchema` impl of its
    /// own; the real, authoritative shape is `webauthn-rs`'s
    /// `CreationChallengeResponse`, not this placeholder.
    #[schema(value_type = Object)]
    pub challenge: CreationChallengeResponse,
}

#[utoipa::path(
    post,
    path = "/identities/register/start",
    tag = "identity",
    request_body = RegisterStartRequest,
    responses((status = 200, body = RegisterStartResponse)),
)]
pub async fn register_start(
    State(state): State<AppState>,
    Json(body): Json<RegisterStartRequest>,
) -> Result<Json<RegisterStartResponse>, AppError> {
    // Issue #629, implementing #622's decision: a shard past its
    // bootstrap grace period with too few independently-confirmed
    // mirrors doesn't get to accept a brand-new identity — checked first,
    // before this handler burns a WebAuthn ceremony or even touches the
    // display-name/identity-id uniqueness checks below, since none of
    // that matters if the shard itself isn't eligible. See
    // `crate::replication`'s own module doc comment; never affects an
    // identity already registered here.
    let now = OffsetDateTime::now_utc();
    let confirmed_mirror_count = state.mirror_confirmations.confirmed_count(
        &state.own_shard_id,
        now - crate::replication::CONFIRMATION_FRESHNESS_WINDOW,
    );
    if !crate::replication::registration_eligible(
        state.shard_registry.first_seen_at(&state.own_shard_id),
        now,
        state.replication_gate.grace_period,
        confirmed_mirror_count,
        state.replication_gate.min_confirmations,
    ) {
        return Err(AppError::ShardBelowMinimumReplication);
    }

    let existing = sqlx::query("SELECT 1 FROM identities WHERE id = $1")
        .bind(body.identity_id)
        .fetch_optional(&state.pool)
        .await?;
    if existing.is_some() {
        return Err(AppError::IdentityIdTaken);
    }

    // Issue #510: advisory only, purely a fail-fast UX check before a
    // client burns a whole WebAuthn ceremony on a name that's already
    // gone — the real, atomic enforcement is `profiles_display_name_lower_idx`
    // at `register_finish`'s actual write (see
    // `avalon_indexer::projections::profiles`'s module doc). A TOCTOU race
    // against this specific check is harmless: the worst case is a client
    // completes the ceremony and then hits the real conflict at `finish`
    // anyway, exactly the same outcome as never having this check at all.
    if profile_reads::is_display_name_taken(&state.pool, &body.display_name, None).await? {
        return Err(AppError::DisplayNameTaken);
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

#[derive(Deserialize, ToSchema)]
pub struct RegisterFinishRequest {
    pub ticket_id: Uuid,
    #[schema(value_type = Object)]
    pub webauthn_credential: RegisterPublicKeyCredential,
    /// Base64-encoded raw Ed25519 public key — the identity's event-signing
    /// key, distinct from the WebAuthn passkey above. See module docs.
    pub event_signing_public_key: String,
    /// Base64-encoded Ed25519 signature over
    /// `identity_created_signing_bytes(identity_id, display_name)`.
    pub event_signature: String,
    /// A user-chosen label for the device completing this ceremony (e.g.
    /// "Work laptop") — purely descriptive, never part of what's signed.
    /// Issue #145: previously the first device's `identity_signing_keys`
    /// row was always unlabeled, unlike every device added later through
    /// #135's grant flow (which does carry a label).
    pub device_label: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct RegisterFinishResponse {
    pub identity_id: Uuid,
}

#[utoipa::path(
    post,
    path = "/identities/register/finish",
    tag = "identity",
    request_body = RegisterFinishRequest,
    responses((status = 200, body = RegisterFinishResponse)),
)]
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
    // issuer of record. See docs/projects/backend-server/architecture/security-model.md.
    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::IdentityCreated
            .as_str()
            .to_string(),
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
        // The initial promised-durable profile state rides along —
        // `display_name` is the identity's public face, not a login
        // credential (no `username` exists anywhere), and without it
        // `profiles` couldn't be rebuilt from history. It's also
        // the identity's globally-unique handle in its own right —
        // no discriminator.
        payload: serde_json::to_value(IdentityCreatedPayload {
            identity_id: ceremony.identity_id,
            display_name: ceremony.display_name.clone(),
        })
        .expect("IdentityCreatedPayload should serialize"),
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

    // `profiles` is a projection: the row is written by the
    // indexer applying `event` below, not by an `INSERT` here. Calling
    // `apply_in_tx` against this same transaction — rather than
    // `state.indexer.apply`, which would open its own — keeps the
    // identity/profile/outbox rows committing or rolling back together.
    //
    // Issue #510: this is the real, atomic display_name-uniqueness
    // enforcement point (`register_start`'s own check was advisory only) —
    // mapped to a clean rejection here, same pattern `insert_identity`'s
    // own unique-violation handling just above already establishes for
    // `identity_id`.
    if let Err(err) = state.indexer.apply_in_tx(&mut tx, &event).await {
        if matches!(err, avalon_indexer::IndexError::DisplayNameTaken) {
            return Err(AppError::DisplayNameTaken);
        }
        return Err(err.into());
    }

    let passkey_row = sqlx::query(
        "INSERT INTO identity_keys (identity_id, credential_id, passkey_data) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(ceremony.identity_id)
    .bind(credential_id)
    .bind(&passkey_json)
    .fetch_one(&mut *tx)
    .await?;
    let passkey_id: Uuid = passkey_row.try_get("id")?;

    // Issue #523: the identity's very first passkey — the common case most
    // identities will only ever have — gets the same durable, mirrorable
    // event `passkeys::register_finish` emits for every later one, so a
    // freshly-created identity is portable from the start rather than only
    // once a second passkey happens to be registered.
    let passkey_event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::IdentityPasskeyRegistered
            .as_str()
            .to_string(),
        issuer: GlobalId::new(
            "identity",
            &ceremony.identity_id.to_string(),
            "self",
            "passkey_registered",
        ),
        subject: GlobalId::new(
            "identity",
            &ceremony.identity_id.to_string(),
            "self",
            "passkey_registered",
        ),
        payload: serde_json::to_value(IdentityPasskeyRegisteredPayload {
            passkey_id,
            identity_id: ceremony.identity_id,
            credential_id: BASE64.encode(credential_id),
            passkey_data: passkey_json.clone(),
            label: None,
        })
        .expect("IdentityPasskeyRegisteredPayload should serialize"),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    };
    outbox::enqueue(&mut tx, &passkey_event).await?;
    state.indexer.apply_in_tx(&mut tx, &passkey_event).await?;

    let signing_key_row = sqlx::query(
        "INSERT INTO identity_signing_keys (identity_id, public_key, label) VALUES ($1, $2, $3) RETURNING id, added_at",
    )
    .bind(ceremony.identity_id)
    .bind(&public_key_bytes)
    .bind(&body.device_label)
    .fetch_one(&mut *tx)
    .await?;
    let signing_key_id: Uuid = signing_key_row.try_get("id")?;
    let signing_key_added_at: OffsetDateTime = signing_key_row.try_get("added_at")?;

    // Issue #525 (Part 2 of #521's decision) surfaced this gap: the
    // identity's very first signing key — every identity has exactly this
    // one at minimum — never got a durable event of its own, only
    // `identity.created` did. Session continuation depends on any node
    // being able to look up an identity's active signing key from replayed
    // history alone, so this can no longer be silent. `approved_by_signing_key_id`
    // is self-referential (this key approved itself) since there is no
    // separate approver yet at identity creation — distinct from
    // `devices.rs`'s grant-approval flow, where a *different*,
    // already-trusted key does the approving.
    let signing_key_event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::IdentitySigningKeyAdded
            .as_str()
            .to_string(),
        issuer: GlobalId::new(
            "identity",
            &ceremony.identity_id.to_string(),
            "self",
            "signing_key_added",
        ),
        subject: GlobalId::new(
            "identity",
            &ceremony.identity_id.to_string(),
            "self",
            "signing_key_added",
        ),
        payload: serde_json::to_value(IdentitySigningKeyAddedPayload {
            signing_key_id,
            public_key: BASE64.encode(&public_key_bytes),
            device_label: body.device_label.clone(),
            approved_by_signing_key_id: signing_key_id,
            identity_id: ceremony.identity_id,
        })
        .expect("IdentitySigningKeyAddedPayload should serialize"),
        timestamp: signing_key_added_at,
        version: 1,
    };
    outbox::enqueue(&mut tx, &signing_key_event).await?;
    state
        .indexer
        .apply_in_tx(&mut tx, &signing_key_event)
        .await?;

    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;
    state.indexer.apply_after_commit(&event).await?;
    state.indexer.apply_after_commit(&passkey_event).await?;
    state.indexer.apply_after_commit(&signing_key_event).await?;

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

#[derive(Deserialize, ToSchema)]
pub struct SessionStartRequest {
    pub identity_id: Uuid,
}

#[derive(Serialize, ToSchema)]
pub struct SessionStartResponse {
    pub ticket_id: Uuid,
    #[schema(value_type = Object)]
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

#[utoipa::path(
    post,
    path = "/sessions/start",
    tag = "identity",
    request_body = SessionStartRequest,
    responses((status = 200, body = SessionStartResponse)),
)]
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

#[derive(Deserialize, ToSchema)]
pub struct SessionFinishRequest {
    pub ticket_id: Uuid,
    #[schema(value_type = Object)]
    pub credential: PublicKeyCredential,
}

#[derive(Serialize, ToSchema)]
pub struct SessionFinishResponse {
    pub token: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub expires_at: OffsetDateTime,
}

#[utoipa::path(
    post,
    path = "/sessions/finish",
    tag = "identity",
    request_body = SessionFinishRequest,
    responses((status = 200, body = SessionFinishResponse)),
)]
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

#[derive(Serialize, ToSchema)]
pub struct ProfileResponse {
    pub identity_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub identity_created_at: OffsetDateTime,
    /// This identity's globally-unique, case-insensitive
    /// handle in its own right — no separate `handle`/discriminator field
    /// exists any more. Add-friend-by-handle
    /// resolves this field directly.
    pub display_name: String,
    pub avatar_url: Option<String>,
    /// Small, user-optional self-description fields — same
    /// promised-durable tier and same public exposure level as
    /// `display_name`/`avatar_url` above (no capability gate, no integrator ever
    /// sees more of it than `GET /me`/`GET /identities/profiles` already
    /// expose).
    pub bio: Option<String>,
    pub favorite_genres: Vec<Genre>,
    pub pronouns: Option<String>,
    /// Expanded self-described fields — same promised-durable
    /// tier and same public exposure level as `bio`/`favorite_genres`/
    /// `pronouns` above.
    pub banner_url: Option<String>,
    pub status: Option<String>,
    pub links: Vec<String>,
    pub timezone: Option<String>,
    pub theme_color: Option<String>,
    /// Self-described only — never IP-derived or geocoded. See
    /// `avalon_protocol::identity::Profile::location`'s doc comment.
    pub location: Option<String>,
    /// A self-chosen pointer to one of this identity's own current guild
    /// memberships (no ticket — see
    /// `avalon_protocol::identity::Profile::main_guild`'s doc comment).
    /// `None` means "not explicitly set," not "no guild" — see
    /// `effective_main_guild` below for the resolved value a UI should
    /// actually build around.
    pub main_guild: Option<Uuid>,
    /// `main_guild` if explicitly set, otherwise the guild this identity
    /// joined earliest (by `guild_members.joined_at`), computed at read
    /// time and never stored — `None` only when the identity has no guild
    /// memberships at all. This, not `main_guild`, is what an integrator
    /// building a single-guild UI should read.
    pub effective_main_guild: Option<Uuid>,
    /// Issue #205's opt-in global search toggle — `true` means this
    /// identity currently matches `GET /identities/search`. Surfaced here
    /// (rather than requiring a separate read) so the Hub's "you are
    /// currently publicly searchable" indicator never drifts out of sync
    /// with the actual `discovery_preferences` row — same reasoning
    /// `handle` is derived rather than separately fetched.
    pub discoverable: bool,
    /// Issue #87 — this identity's own presence-visibility setting
    /// (`"public"`/`"authenticated_only"`/`"friends"`/`"guild_members"`/`"private"`).
    /// Self-only, same posture `discoverable` takes — see this endpoint's
    /// own doc comment on why `PublicIdentityProfileResponse` omits both.
    pub presence_visibility: String,
}

fn profile_view_to_response(
    identity_id: Uuid,
    view: profile_reads::ProfileView,
    effective_main_guild: Option<Uuid>,
) -> ProfileResponse {
    ProfileResponse {
        identity_id,
        identity_created_at: view.identity_created_at,
        display_name: view.display_name,
        avatar_url: view.avatar_url,
        bio: view.bio,
        // Stored genre strings are already server-validated at write time
        // (`validate_favorite_genres`), so an unparseable value here would
        // mean data corruption, not a client error — dropped rather than
        // failing the whole read.
        favorite_genres: view
            .favorite_genres
            .iter()
            .filter_map(|g| Genre::parse(g))
            .collect(),
        pronouns: view.pronouns,
        banner_url: view.banner_url,
        status: view.status,
        links: view.links,
        timezone: view.timezone,
        theme_color: view.theme_color,
        location: view.location,
        main_guild: view.main_guild,
        effective_main_guild,
        discoverable: view.discoverable,
        presence_visibility: view.presence_visibility,
    }
}

/// The guild `identity_id` joined earliest (by `guild_members.joined_at`),
/// or `None` if it has no memberships — the read-time default
/// `ProfileResponse::effective_main_guild` falls back to when
/// `Profile::main_guild` itself is unset. Deliberately not written into
/// `profiles.main_guild` (see that field's doc comment): computed fresh on
/// every read via any executor (the shared pool for a plain `GET /me`, or
/// an open transaction for `update_profile`'s own read-after-write), so it
/// always reflects the true earliest membership even as guilds are joined
/// or left.
async fn earliest_joined_guild<'e, E>(
    executor: E,
    identity_id: Uuid,
) -> Result<Option<Uuid>, AppError>
where
    E: sqlx::PgExecutor<'e>,
{
    let memberships =
        avalon_indexer::projections::guild_rosters::memberships_for(executor, identity_id).await?;
    Ok(memberships.first().map(|m| m.guild_id))
}

#[utoipa::path(
    get,
    path = "/me",
    tag = "identity",
    responses((status = 200, body = ProfileResponse)),
)]
pub async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ProfileResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    // `profile_reads::fetch` returning `None` here would mean an
    // authenticated identity has no `profiles` row — data corruption, not
    // a normal "not found" — so this maps to the same `RowNotFound`-shaped
    // error `fetch_one` used to produce directly.
    let view = profile_reads::fetch(&state.pool, identity_id)
        .await?
        .ok_or(AppError::Database(sqlx::Error::RowNotFound))?;
    let effective_main_guild = match view.main_guild {
        Some(guild_id) => Some(guild_id),
        None => earliest_joined_guild(&state.pool, identity_id).await?,
    };

    Ok(Json(profile_view_to_response(
        identity_id,
        view,
        effective_main_guild,
    )))
}

const PROFILE_LOOKUP_MAX_IDS: usize = 100;

#[derive(Deserialize, utoipa::IntoParams)]
pub struct ProfilesQuery {
    /// Comma-separated identity ids, e.g. `?ids=<uuid>,<uuid>` — same shape
    /// `presence::PresenceQuery` already established for a batched read.
    pub ids: String,
}

/// Another identity's *public* profile fields only — never anything a
/// stranger couldn't already learn via `friends::resolve_handle`'s
/// name-to-id lookup run in reverse. No bio, no email, nothing beyond what
/// `display_name`/`avatar_url` already are: the least-sensitive public-face
/// fields. This endpoint has no further visibility gating (any session can
/// batch-resolve arbitrary identity ids), so `bio`/`favorite_genres`/
/// `pronouns` and `banner_url`/`status`/`links`/`timezone`/
/// `theme_color`/`location` are deliberately withheld here even
/// though they're unauthenticated-readable on one's own `GET /me` — batch
/// stranger lookup is a materially wider exposure than a single
/// self-disclosed profile view, and widening it is a scoping decision for
/// its own ticket, not a side effect of adding the columns.
#[derive(Serialize, ToSchema)]
pub struct PublicProfileResponse {
    pub identity_id: Uuid,
    pub display_name: String,
    pub avatar_url: Option<String>,
}

/// Another identity's full self-description profile — a decided
/// widening of the read-only profile card. Deliberately a **separate,
/// single-identity endpoint** rather than a widened `list_profiles`: the
/// batch endpoint above stays exactly as narrow as it already is (any
/// session can resolve arbitrarily many ids at once, so it only ever
/// returns the least-sensitive public-face fields), while this endpoint
/// exposes the same fields `GET /me` already does — `bio`/`favorite_genres`/
/// `pronouns` and `banner_url`/`status`/`links`/`timezone`/
/// `theme_color`/`location` — but only for one identity per request,
/// matching a real profile-card view rather than a roster resolve. Omits
/// `discoverable` and `presence_visibility`: both describe the *viewed*
/// identity's own settings preferences, not something the viewer needs
/// once they've already found the profile.
#[derive(Serialize, ToSchema)]
pub struct PublicIdentityProfileResponse {
    pub identity_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub identity_created_at: OffsetDateTime,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub bio: Option<String>,
    pub favorite_genres: Vec<Genre>,
    pub pronouns: Option<String>,
    pub banner_url: Option<String>,
    pub status: Option<String>,
    pub links: Vec<String>,
    pub timezone: Option<String>,
    pub theme_color: Option<String>,
    pub location: Option<String>,
    pub main_guild: Option<Uuid>,
    pub effective_main_guild: Option<Uuid>,
}

/// `GET /identities/{id}/profile` — issue #403. Session-authenticated, no
/// further visibility gating (same posture as `list_profiles`): every field
/// here is already unauthenticated-readable on the viewed identity's own
/// `GET /me`, so a single-identity read of the same fields adds no new
/// exposure, only a more convenient shape than "batch-resolve one id."
#[utoipa::path(
    get,
    path = "/identities/{id}/profile",
    tag = "identity",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = PublicIdentityProfileResponse)),
)]
pub async fn get_identity_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<PublicIdentityProfileResponse>, AppError> {
    authenticate(&state, &headers).await?;

    let view = profile_reads::fetch(&state.pool, id)
        .await?
        .ok_or(AppError::IdentityNotFound)?;

    let effective_main_guild = match view.main_guild {
        Some(guild_id) => Some(guild_id),
        None => earliest_joined_guild(&state.pool, id).await?,
    };

    Ok(Json(PublicIdentityProfileResponse {
        identity_id: id,
        identity_created_at: view.identity_created_at,
        display_name: view.display_name,
        avatar_url: view.avatar_url,
        bio: view.bio,
        favorite_genres: view
            .favorite_genres
            .iter()
            .filter_map(|g| Genre::parse(g))
            .collect(),
        pronouns: view.pronouns,
        banner_url: view.banner_url,
        status: view.status,
        links: view.links,
        timezone: view.timezone,
        theme_color: view.theme_color,
        location: view.location,
        main_guild: view.main_guild,
        effective_main_guild,
    }))
}

/// `GET /identities/profiles?ids=…` — issue #161. Closes the gap every
/// roster-shaped surface built so far (`friends::list_friends`,
/// `guilds::list_members`, and their SDK/Hub consumers) has had to leave as
/// a raw identity id: there was never an endpoint that resolved *another*
/// identity's display name. Session-authenticated only, no further
/// visibility gating — display name and avatar are already the
/// least-sensitive public-face fields, same exposure level
/// `friends::resolve_handle` already has. Unknown ids are silently omitted
/// rather than erroring, so one bad id in a roster doesn't 500 the whole
/// batch.
#[utoipa::path(
    get,
    path = "/identities/profiles",
    tag = "identity",
    params(ProfilesQuery),
    responses((status = 200, body = Vec<PublicProfileResponse>)),
)]
pub async fn list_profiles(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ProfilesQuery>,
) -> Result<Json<Vec<PublicProfileResponse>>, AppError> {
    authenticate(&state, &headers).await?;

    let ids: Vec<Uuid> = query
        .ids
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<Uuid>().map_err(|_| AppError::InvalidProfileQuery))
        .collect::<Result<_, _>>()?;

    if ids.len() > PROFILE_LOOKUP_MAX_IDS {
        return Err(AppError::InvalidProfileQuery);
    }
    if ids.is_empty() {
        return Ok(Json(Vec::new()));
    }

    let summaries = profile_reads::fetch_many(&state.pool, &ids).await?;
    let profiles = summaries
        .into_iter()
        .map(|s| PublicProfileResponse {
            identity_id: s.identity_id,
            display_name: s.display_name,
            avatar_url: s.avatar_url,
        })
        .collect();
    Ok(Json(profiles))
}

/// One event from the caller's own protocol history — "what
/// does the network know about me." `subject` is included since not every
/// event an identity issues is *about* itself the same way (e.g.
/// `friend.requested` is issued by the requester but its subject is the
/// recipient); `payload` is passed through as-is rather than reduced to a
/// canned summary string, matching this repo's general preference for
/// exposing real data over a lossy client-unfriendly-format-agnostic gloss.
#[derive(Serialize, ToSchema)]
pub struct HistoryEntryResponse {
    pub event_id: Uuid,
    pub kind: String,
    pub subject: String,
    /// `null` if this event's payload has been pruned locally (issue
    /// #208, a hot-tier node) — the event's existence and `kind` are still
    /// reported, just not its content.
    #[schema(value_type = Object, nullable)]
    pub payload: Option<serde_json::Value>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub timestamp: OffsetDateTime,
}

/// Only the caller's own events, never another identity's — enforced by
/// construction, not by a filter a caller could omit: the issuer prefix is
/// always built from the authenticated identity id here, never accepted as
/// a request parameter. Reads the ledger directly (issuer-filtered, see
/// `avalon_chain::PostgresSettlementProvider::list_entries_for_issuer_prefix`)
/// — a deliberate, permanent exception to issue #44's "no request handler
/// queries the ledger" invariant, not a stopgap: this endpoint's whole job
/// is exposing the raw append-only history itself, which is fundamentally
/// a settlement-native read (a full historical log), not a current-state
/// one — projecting the entire per-issuer event history into the indexer
/// just to re-serve it here would duplicate the ledger, not replace it.
/// `crates/server/tests/read_model_boundary.rs`'s guard test names this
/// function as the one allowed exception; any other `PostgresSettlementProvider`
/// call added to this file should not be.
#[utoipa::path(
    get,
    path = "/me/history",
    tag = "identity",
    responses((status = 200, body = Vec<HistoryEntryResponse>)),
)]
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

#[derive(Deserialize, ToSchema)]
pub struct UpdateProfileRequest {
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    /// Three states, same as `avatar_url`: omitted (untouched), `Some("")`
    /// (clear to `NULL`), `Some(nonempty)` (validate against
    /// [`MAX_BIO_LEN`], then set).
    pub bio: Option<String>,
    /// Two states, not three: omitted (untouched) or `Some(list)`, which
    /// always fully replaces the stored list — including `Some(vec![])` to
    /// clear it. Each entry must parse as a [`Genre`]; an unknown value is
    /// rejected outright rather than silently dropped.
    pub favorite_genres: Option<Vec<String>>,
    /// Three states, same as `bio`.
    pub pronouns: Option<String>,
    /// Three states, same as `avatar_url` — a second image slot, separate
    /// from the avatar, for the Hub profile page header.
    pub banner_url: Option<String>,
    /// Three states, same as `bio`, capped at
    /// [`avalon_protocol::identity::MAX_STATUS_LEN`].
    pub status: Option<String>,
    /// Two states, not three: omitted (untouched) or `Some(list)`, which
    /// always fully replaces the stored list — including `Some(vec![])` to
    /// clear it. Same shape as `favorite_genres`, but each entry is a
    /// free-form URL rather than a fixed vocabulary value.
    pub links: Option<Vec<String>>,
    /// Three states, same as `bio`. Length-checked only, not validated
    /// against the real IANA time zone database — see
    /// `avalon_protocol::identity::Profile::timezone`'s doc comment.
    pub timezone: Option<String>,
    /// Three states, same as `bio`. Must match `^#[0-9a-fA-F]{6}$` when
    /// non-empty.
    pub theme_color: Option<String>,
    /// Three states, same as `bio`. Self-described free text only — never
    /// IP-derived or geocoded.
    pub location: Option<String>,
    /// Three states, same as `bio`: omitted (untouched), `""` (clear to
    /// `NULL`), or a guild id string. Unlike every other three-state field
    /// here, a non-empty value also needs a database check — it must name
    /// a guild `identity_id` is currently a member of (`AppError::NotGuildMember`
    /// otherwise) — so its validation lives in `validate_main_guild` rather
    /// than one of the pure `validate_*` functions above.
    pub main_guild: Option<String>,
    /// Opt-in global search toggle. Two states, not three
    /// (there's no "clear" state for a plain boolean): `None` leaves the
    /// existing preference untouched, `Some(bool)` sets it. Off by
    /// default for every identity (no row in `discovery_preferences` at
    /// all reads as `false`) — this field is the only way it ever
    /// becomes `true`. Not part of `profile.updated`/durable history, and
    /// not written through the same transaction as the rest of this
    /// request's changes — see `discovery::set_discoverable`'s doc
    /// comment for why, matching `presence_preferences.hide_active_in`'s
    /// identical precedent.
    pub discoverable: Option<bool>,
    /// `"public"`/`"authenticated_only"`/`"friends"` (the
    /// default)/`"private"` — who may read this identity's presence via
    /// `GET /presence`. `"guild_members"` is accepted (presence has no
    /// guild context, so it behaves like `"private"` — nobody but the
    /// subject — see `crate::presence::presence_visible`). Same
    /// not-durable-history treatment as `discoverable`: applied outside
    /// this request's transaction, no `profile.updated` payload entry.
    pub presence_visibility: Option<String>,
}

/// Nothing renders `avatar_url` as an actual image anywhere in the Hub
/// today, so there's no live XSS path yet — but that's incidental, not a
/// guarantee: the field exists so a user-supplied avatar shows up
/// somewhere later (friends list, guild roster), and an unvalidated
/// `javascript:`/`data:`-scheme string sitting in storage is exactly the
/// kind of thing that becomes a real problem the moment something renders
/// it with `<img :src>` without re-checking this. Validated before the
/// `profiles` write (via `PostgresIndexer::apply_in_tx`), never after.
const MAX_AVATAR_URL_LEN: usize = 2048;

/// Shared `http`/`https`-URL validation: non-empty, at most `max_len`
/// characters, and parses as an `http`/`https` URL. Used by
/// [`validate_avatar_url`] below and, since issue #153, by
/// `crates/server/src/guilds.rs`'s guild `banner` validation — same rule,
/// same reasoning ("a user/guild-supplied URL sitting in storage becomes
/// a real problem the moment something renders it with `<img :src>` without
/// re-checking this"), so the check lives in one place rather than being
/// copied.
pub(crate) fn is_http_url(url: &str, max_len: usize) -> bool {
    if url.is_empty() || url.len() > max_len {
        return false;
    }
    match Url::parse(url) {
        Ok(parsed) => parsed.scheme() == "http" || parsed.scheme() == "https",
        Err(_) => false,
    }
}

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
    if !is_http_url(avatar_url, MAX_AVATAR_URL_LEN) {
        return Err(AppError::InvalidAvatarUrl);
    }
    Ok(Some(avatar_url.to_string()))
}

/// Same empty-string-means-clear convention as [`validate_avatar_url`]. A
/// non-empty value must be within [`MAX_BIO_LEN`] characters or the request
/// is rejected — never silently truncated.
fn validate_bio(bio: &str) -> Result<Option<String>, AppError> {
    if bio.is_empty() {
        return Ok(None);
    }
    if bio.chars().count() > MAX_BIO_LEN {
        return Err(AppError::InvalidBio);
    }
    Ok(Some(bio.to_string()))
}

/// Same empty-string-means-clear convention as [`validate_avatar_url`]. A
/// non-empty value must be within [`MAX_PRONOUNS_LEN`] characters or the
/// request is rejected — never silently truncated.
fn validate_pronouns(pronouns: &str) -> Result<Option<String>, AppError> {
    if pronouns.is_empty() {
        return Ok(None);
    }
    if pronouns.chars().count() > MAX_PRONOUNS_LEN {
        return Err(AppError::InvalidPronouns);
    }
    Ok(Some(pronouns.to_string()))
}

/// `favorite_genres` is a fixed, small controlled vocabulary, not free text —
/// an unknown value is rejected outright, not silently
/// dropped, the same reasoning `GuildPermission` parsing already
/// established: a bad value here is more likely a real client bug than a
/// schema drift. Also enforces the [`MAX_FAVORITE_GENRES`] count cap and
/// de-duplicates (a repeated genre isn't an error, just collapsed).
fn validate_favorite_genres(genres: &[String]) -> Result<Vec<Genre>, AppError> {
    if genres.len() > MAX_FAVORITE_GENRES {
        return Err(AppError::TooManyFavoriteGenres);
    }
    let mut parsed = Vec::with_capacity(genres.len());
    for raw in genres {
        let genre = Genre::parse(raw).ok_or(AppError::InvalidGenre)?;
        if !parsed.contains(&genre) {
            parsed.push(genre);
        }
    }
    Ok(parsed)
}

/// Same empty-string-means-clear convention as [`validate_avatar_url`]. A
/// non-empty value must be within [`MAX_STATUS_LEN`] characters or the
/// request is rejected — never silently truncated.
fn validate_status(status: &str) -> Result<Option<String>, AppError> {
    if status.is_empty() {
        return Ok(None);
    }
    if status.chars().count() > MAX_STATUS_LEN {
        return Err(AppError::InvalidStatus);
    }
    Ok(Some(status.to_string()))
}

/// `links` is two-state like `favorite_genres`, not three: no per-entry
/// clearing, `Some(list)` (including `Some(vec![])`) always fully replaces
/// the stored list. Each entry must be a non-empty `http`/`https` URL within
/// [`MAX_LINK_LEN`] characters, and the whole list is capped at [`MAX_LINKS`]
/// entries — an invalid entry rejects the whole request rather than being
/// silently dropped, same reasoning `validate_favorite_genres` uses.
fn validate_links(links: &[String]) -> Result<Vec<String>, AppError> {
    if links.len() > MAX_LINKS {
        return Err(AppError::TooManyLinks);
    }
    let mut parsed = Vec::with_capacity(links.len());
    for raw in links {
        if !is_http_url(raw, MAX_LINK_LEN) {
            return Err(AppError::InvalidLink);
        }
        parsed.push(raw.clone());
    }
    Ok(parsed)
}

/// Same empty-string-means-clear convention as [`validate_avatar_url`]. A
/// non-empty value must be within [`MAX_TIMEZONE_LEN`] characters. This is a
/// length check only — NOT validated against the real IANA time zone
/// database, since no such crate exists in this workspace today;
/// a known, documented gap, not silently pretended-correct.
fn validate_timezone(timezone: &str) -> Result<Option<String>, AppError> {
    if timezone.is_empty() {
        return Ok(None);
    }
    if timezone.chars().count() > MAX_TIMEZONE_LEN {
        return Err(AppError::InvalidTimezone);
    }
    Ok(Some(timezone.to_string()))
}

/// Same empty-string-means-clear convention as [`validate_avatar_url`]. A
/// non-empty value must match `^#[0-9a-fA-F]{6}$` exactly.
fn validate_theme_color(theme_color: &str) -> Result<Option<String>, AppError> {
    if theme_color.is_empty() {
        return Ok(None);
    }
    let is_valid_hex_color = theme_color.len() == 7
        && theme_color.starts_with('#')
        && theme_color[1..].chars().all(|c| c.is_ascii_hexdigit());
    if !is_valid_hex_color {
        return Err(AppError::InvalidThemeColor);
    }
    Ok(Some(theme_color.to_string()))
}

/// Same empty-string-means-clear convention as [`validate_avatar_url`]. A
/// non-empty value must be within [`MAX_LOCATION_LEN`] characters. Free text
/// only — this function never derives a value from an IP address or any
/// other signal; it only validates what the caller already typed.
fn validate_location(location: &str) -> Result<Option<String>, AppError> {
    if location.is_empty() {
        return Ok(None);
    }
    if location.chars().count() > MAX_LOCATION_LEN {
        return Err(AppError::InvalidLocation);
    }
    Ok(Some(location.to_string()))
}

/// Same empty-string-means-clear convention as [`validate_avatar_url`], but
/// a non-empty value needs a database round trip on top of the parse: it
/// must be a well-formed guild id (`AppError::InvalidMainGuild` otherwise)
/// naming a guild `identity_id` is currently a member of
/// (`AppError::NotGuildMember` otherwise) — checked against `guild_members`
/// directly rather than through `crates/server/src/guilds.rs`, since this
/// is a plain membership fact, not anything permission-gated. Not one of
/// the pure `validate_*` functions above for that reason.
async fn validate_main_guild(
    pool: &sqlx::PgPool,
    identity_id: Uuid,
    main_guild: &str,
) -> Result<Option<Uuid>, AppError> {
    if main_guild.is_empty() {
        return Ok(None);
    }
    let guild_id: Uuid = main_guild.parse().map_err(|_| AppError::InvalidMainGuild)?;
    let is_member =
        avalon_indexer::projections::guild_rosters::is_member(pool, guild_id, identity_id).await?;
    if !is_member {
        return Err(AppError::NotGuildMember);
    }
    Ok(Some(guild_id))
}

/// The payload `profile.updated` carries: only the
/// fields this request actually changed. An explicitly cleared
/// `avatar_url`/`bio`/`pronouns` is `null`; an untouched one is absent —
/// the same three-state distinction `update_profile` itself makes.
/// `favorite_genres` has only two states: absent (untouched) or present
/// (the new, complete list, including `[]` to clear it).
#[allow(clippy::too_many_arguments)]
pub(crate) fn profile_updated_payload(
    display_name: Option<&str>,
    avatar_url: Option<Option<&str>>,
    bio: Option<Option<&str>>,
    favorite_genres: Option<&[Genre]>,
    pronouns: Option<Option<&str>>,
    banner_url: Option<Option<&str>>,
    status: Option<Option<&str>>,
    links: Option<&[String]>,
    timezone: Option<Option<&str>>,
    theme_color: Option<Option<&str>>,
    location: Option<Option<&str>>,
    main_guild: Option<Option<Uuid>>,
) -> serde_json::Value {
    // Issue #82: built through the typed `ProfileUpdatedPayload`
    // (`avalon_protocol::event_payloads`) rather than hand-inserting keys
    // into a `serde_json::Map` — this function's own return type stays
    // `serde_json::Value` since every call site (and this module's own
    // unit tests below) already expects that shape, and the wire output
    // is byte-for-byte identical either way.
    let payload = ProfileUpdatedPayload {
        display_name: display_name.map(str::to_string),
        avatar_url: avatar_url.map(|v| v.map(str::to_string)),
        bio: bio.map(|v| v.map(str::to_string)),
        favorite_genres: favorite_genres
            .map(|genres| genres.iter().map(|g| g.as_str().to_string()).collect()),
        pronouns: pronouns.map(|v| v.map(str::to_string)),
        banner_url: banner_url.map(|v| v.map(str::to_string)),
        status: status.map(|v| v.map(str::to_string)),
        links: links.map(<[String]>::to_vec),
        timezone: timezone.map(|v| v.map(str::to_string)),
        theme_color: theme_color.map(|v| v.map(str::to_string)),
        location: location.map(|v| v.map(str::to_string)),
        main_guild,
    };
    serde_json::to_value(payload).expect("ProfileUpdatedPayload should serialize")
}

#[utoipa::path(
    patch,
    path = "/me",
    tag = "identity",
    request_body = UpdateProfileRequest,
    responses((status = 200, body = ProfileResponse)),
)]
pub async fn update_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UpdateProfileRequest>,
) -> Result<Json<ProfileResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    // Issue #510: advisory only, same reasoning `register_start`'s own
    // check documents — the real enforcement is `apply_in_tx`'s write
    // further down, mapped to a clean rejection there.
    if let Some(new_name) = &body.display_name {
        if profile_reads::is_display_name_taken(&state.pool, new_name, Some(identity_id)).await? {
            return Err(AppError::DisplayNameTaken);
        }
    }

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

    let bio_provided = body.bio.is_some();
    let bio = match &body.bio {
        Some(raw) => validate_bio(raw)?,
        None => None,
    };

    let pronouns_provided = body.pronouns.is_some();
    let pronouns = match &body.pronouns {
        Some(raw) => validate_pronouns(raw)?,
        None => None,
    };

    let favorite_genres_provided = body.favorite_genres.is_some();
    let favorite_genres = match &body.favorite_genres {
        Some(raw) => validate_favorite_genres(raw)?,
        None => Vec::new(),
    };

    let banner_url_provided = body.banner_url.is_some();
    let banner_url = match &body.banner_url {
        Some(raw) => validate_avatar_url(raw)?,
        None => None,
    };

    let status_provided = body.status.is_some();
    let status = match &body.status {
        Some(raw) => validate_status(raw)?,
        None => None,
    };

    let links_provided = body.links.is_some();
    let links = match &body.links {
        Some(raw) => validate_links(raw)?,
        None => Vec::new(),
    };

    let timezone_provided = body.timezone.is_some();
    let timezone = match &body.timezone {
        Some(raw) => validate_timezone(raw)?,
        None => None,
    };

    let theme_color_provided = body.theme_color.is_some();
    let theme_color = match &body.theme_color {
        Some(raw) => validate_theme_color(raw)?,
        None => None,
    };

    let location_provided = body.location.is_some();
    let location = match &body.location {
        Some(raw) => validate_location(raw)?,
        None => None,
    };

    let main_guild_provided = body.main_guild.is_some();
    let main_guild = match &body.main_guild {
        Some(raw) => validate_main_guild(&state.pool, identity_id, raw).await?,
        None => None,
    };

    // `discoverable` is deliberately handled outside the
    // transaction below, the same way `presence::update_my_presence`
    // handles `hide_active_in`: it's a user preference, not durable
    // protocol history, so it has no `profile.updated` payload and needs
    // no atomicity with the rest of this request's changes. Applied only
    // now, after every fallible validation above has already succeeded —
    // never before them. The `is_display_name_taken` check/`validate_*`
    // calls above can each still fail and abort this handler with an
    // `AppError`; running this
    // write any earlier would let an otherwise-failed PATCH /me (a bad
    // avatar_url, an invalid genre, ...) leave `discoverable` flipped
    // anyway, which is exactly the kind of partial-write a default-off,
    // no-exceptions preference must never have. Still applied before the
    // `profile_reads::fetch` read further down, so that read already
    // reflects it.
    if let Some(discoverable) = body.discoverable {
        crate::discovery::set_discoverable(&state, identity_id, discoverable).await?;
    }
    if let Some(raw) = &body.presence_visibility {
        raw.parse::<avalon_protocol::permissions::Visibility>()
            .map_err(|_| AppError::InvalidVisibility)?;
        crate::visibility::set_presence_visibility(&state, identity_id, raw).await?;
    }

    // `display_name`, `avatar_url`, `bio`, `favorite_genres`, and `pronouns`
    // are all promised-durable (see the table in
    // docs/projects/backend-server/architecture/identity.md), so a change to
    // any of them emits `profile.updated` in the same transaction as the
    // row — through the outbox, exactly like `register_finish`. A request
    // that changes nothing emits nothing. Network-attributed rather than
    // signed by the identity's own key: the same milestone-1 stand-in
    // `friends.rs` uses, since no general per-event signing ceremony exists
    // yet.
    let event = (body.display_name.is_some()
        || avatar_url_provided
        || bio_provided
        || favorite_genres_provided
        || pronouns_provided
        || banner_url_provided
        || status_provided
        || links_provided
        || timezone_provided
        || theme_color_provided
        || location_provided
        || main_guild_provided)
        .then(|| ProtocolEvent {
            id: Uuid::new_v4(),
            kind: ProtocolEventKindVariant::ProfileUpdated
                .as_str()
                .to_string(),
            issuer: GlobalId::new(
                "identity",
                &identity_id.to_string(),
                "self",
                "profile_updated",
            ),
            subject: GlobalId::new(
                "identity",
                &identity_id.to_string(),
                "self",
                "profile_updated",
            ),
            payload: profile_updated_payload(
                body.display_name.as_deref(),
                avatar_url_provided.then_some(avatar_url.as_deref()),
                bio_provided.then_some(bio.as_deref()),
                favorite_genres_provided.then_some(favorite_genres.as_slice()),
                pronouns_provided.then_some(pronouns.as_deref()),
                banner_url_provided.then_some(banner_url.as_deref()),
                status_provided.then_some(status.as_deref()),
                links_provided.then_some(links.as_slice()),
                timezone_provided.then_some(timezone.as_deref()),
                theme_color_provided.then_some(theme_color.as_deref()),
                location_provided.then_some(location.as_deref()),
                main_guild_provided.then_some(main_guild),
            ),
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        });

    let mut tx = state.pool.begin().await?;

    // `profiles` is a projection: the write below happens
    // through the indexer applying `event`, in this same transaction, not
    // through a bespoke `UPDATE` here — matching `register_finish`. A
    // request that changed nothing has no event, so nothing to apply; the
    // `SELECT` after this still returns the (unchanged) current row.
    if let Some(event) = &event {
        // Issue #510: the real, atomic display_name-uniqueness enforcement
        // point — the advisory check above this function's start can't
        // guarantee this doesn't fail here too.
        if let Err(err) = state.indexer.apply_in_tx(&mut tx, event).await {
            if matches!(err, avalon_indexer::IndexError::DisplayNameTaken) {
                return Err(AppError::DisplayNameTaken);
            }
            return Err(err.into());
        }
        outbox::enqueue(&mut tx, event).await?;
    }

    let view = profile_reads::fetch(&mut *tx, identity_id)
        .await?
        .ok_or(AppError::Database(sqlx::Error::RowNotFound))?;
    let effective_main_guild = match view.main_guild {
        Some(guild_id) => Some(guild_id),
        None => earliest_joined_guild(&mut *tx, identity_id).await?,
    };

    tx.commit().await?;
    if let Some(event) = &event {
        state.indexer.apply_after_commit(event).await?;
    }

    Ok(Json(profile_view_to_response(
        identity_id,
        view,
        effective_main_guild,
    )))
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
    fn profile_updated_payload_carries_only_the_changed_fields() {
        let payload = profile_updated_payload(
            Some("nova"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(payload, serde_json::json!({ "display_name": "nova" }));
        assert!(payload.get("avatar_url").is_none());
        assert!(payload.get("bio").is_none());
        assert!(payload.get("favorite_genres").is_none());
        assert!(payload.get("pronouns").is_none());
    }

    #[test]
    fn profile_updated_payload_distinguishes_a_cleared_avatar_from_an_untouched_one() {
        let cleared = profile_updated_payload(
            None,
            Some(None),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(cleared, serde_json::json!({ "avatar_url": null }));

        let set = profile_updated_payload(
            None,
            Some(Some("https://example.com/a.png")),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            set,
            serde_json::json!({ "avatar_url": "https://example.com/a.png" })
        );

        let untouched = profile_updated_payload(
            Some("nova"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert!(untouched.get("avatar_url").is_none());
    }

    #[test]
    fn profile_updated_payload_distinguishes_a_cleared_bio_from_an_untouched_one() {
        let cleared = profile_updated_payload(
            None,
            None,
            Some(None),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(cleared, serde_json::json!({ "bio": null }));

        let set = profile_updated_payload(
            None,
            None,
            Some(Some("hello")),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(set, serde_json::json!({ "bio": "hello" }));

        let untouched = profile_updated_payload(
            None, None, None, None, None, None, None, None, None, None, None, None,
        );
        assert!(untouched.get("bio").is_none());
    }

    #[test]
    fn profile_updated_payload_carries_favorite_genres_as_strings() {
        let payload = profile_updated_payload(
            None,
            None,
            None,
            Some(&[Genre::Rpg, Genre::Puzzle]),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            payload,
            serde_json::json!({ "favorite_genres": ["rpg", "puzzle"] })
        );
    }

    #[test]
    fn profile_updated_payload_carries_an_empty_favorite_genres_clear() {
        let payload = profile_updated_payload(
            None,
            None,
            None,
            Some(&[]),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(payload, serde_json::json!({ "favorite_genres": [] }));
    }

    #[test]
    fn profile_updated_payload_distinguishes_a_cleared_banner_from_an_untouched_one() {
        let cleared = profile_updated_payload(
            None,
            None,
            None,
            None,
            None,
            Some(None),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(cleared, serde_json::json!({ "banner_url": null }));

        let set = profile_updated_payload(
            None,
            None,
            None,
            None,
            None,
            Some(Some("https://example.com/b.png")),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            set,
            serde_json::json!({ "banner_url": "https://example.com/b.png" })
        );
    }

    #[test]
    fn profile_updated_payload_carries_links_as_a_full_replace() {
        let links = vec!["https://example.com".to_string()];
        let payload = profile_updated_payload(
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(links.as_slice()),
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            payload,
            serde_json::json!({ "links": ["https://example.com"] })
        );
    }

    #[test]
    fn profile_updated_payload_distinguishes_a_cleared_location_from_an_untouched_one() {
        let cleared = profile_updated_payload(
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(None),
            None,
        );
        assert_eq!(cleared, serde_json::json!({ "location": null }));

        let set = profile_updated_payload(
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(Some("Pacific Northwest")),
            None,
        );
        assert_eq!(set, serde_json::json!({ "location": "Pacific Northwest" }));
    }

    #[test]
    fn accepts_a_url_exactly_at_the_length_cap() {
        let path_len = MAX_AVATAR_URL_LEN - "https://example.com/".len();
        let exactly_at_cap = format!("https://example.com/{}", "a".repeat(path_len));
        assert_eq!(exactly_at_cap.len(), MAX_AVATAR_URL_LEN);
        assert!(validate_avatar_url(&exactly_at_cap).is_ok());
    }

    #[test]
    fn validate_bio_treats_empty_string_as_clear_not_an_error() {
        assert_eq!(validate_bio("").unwrap(), None);
    }

    #[test]
    fn validate_bio_accepts_a_bio_within_the_cap() {
        assert_eq!(
            validate_bio("just here for the guild raids").unwrap(),
            Some("just here for the guild raids".to_string())
        );
    }

    #[test]
    fn validate_bio_rejects_an_over_length_bio() {
        let overlong = "a".repeat(MAX_BIO_LEN + 1);
        assert!(matches!(validate_bio(&overlong), Err(AppError::InvalidBio)));
    }

    #[test]
    fn validate_bio_accepts_a_bio_exactly_at_the_cap() {
        let exactly_at_cap = "a".repeat(MAX_BIO_LEN);
        assert!(validate_bio(&exactly_at_cap).is_ok());
    }

    #[test]
    fn validate_pronouns_treats_empty_string_as_clear_not_an_error() {
        assert_eq!(validate_pronouns("").unwrap(), None);
    }

    #[test]
    fn validate_pronouns_rejects_an_over_length_value() {
        let overlong = "a".repeat(MAX_PRONOUNS_LEN + 1);
        assert!(matches!(
            validate_pronouns(&overlong),
            Err(AppError::InvalidPronouns)
        ));
    }

    #[test]
    fn validate_favorite_genres_accepts_known_values() {
        let genres = validate_favorite_genres(&["rpg".to_string(), "puzzle".to_string()]).unwrap();
        assert_eq!(genres, vec![Genre::Rpg, Genre::Puzzle]);
    }

    #[test]
    fn validate_favorite_genres_rejects_an_unknown_value() {
        assert!(matches!(
            validate_favorite_genres(&["visual_novel".to_string()]),
            Err(AppError::InvalidGenre)
        ));
    }

    #[test]
    fn validate_favorite_genres_rejects_too_many_entries() {
        let too_many: Vec<String> = Genre::ALL
            .iter()
            .chain(Genre::ALL.iter())
            .take(MAX_FAVORITE_GENRES + 1)
            .map(|g| g.as_str().to_string())
            .collect();
        assert!(matches!(
            validate_favorite_genres(&too_many),
            Err(AppError::TooManyFavoriteGenres)
        ));
    }

    #[test]
    fn validate_favorite_genres_empty_list_stays_empty_without_error() {
        assert_eq!(validate_favorite_genres(&[]).unwrap(), Vec::new());
    }

    #[test]
    fn validate_favorite_genres_deduplicates_repeats() {
        let genres = validate_favorite_genres(&["rpg".to_string(), "rpg".to_string()]).unwrap();
        assert_eq!(genres, vec![Genre::Rpg]);
    }

    #[test]
    fn validate_status_treats_empty_string_as_clear_not_an_error() {
        assert_eq!(validate_status("").unwrap(), None);
    }

    #[test]
    fn validate_status_rejects_an_over_length_value() {
        let overlong = "a".repeat(MAX_STATUS_LEN + 1);
        assert!(matches!(
            validate_status(&overlong),
            Err(AppError::InvalidStatus)
        ));
    }

    #[test]
    fn validate_status_accepts_a_value_within_the_cap() {
        assert_eq!(
            validate_status("raiding tonight").unwrap(),
            Some("raiding tonight".to_string())
        );
    }

    #[test]
    fn validate_links_accepts_valid_urls() {
        let links = validate_links(&[
            "https://example.com".to_string(),
            "http://example.org/x".to_string(),
        ])
        .unwrap();
        assert_eq!(
            links,
            vec![
                "https://example.com".to_string(),
                "http://example.org/x".to_string()
            ]
        );
    }

    #[test]
    fn validate_links_empty_list_stays_empty_without_error() {
        assert_eq!(validate_links(&[]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn validate_links_rejects_too_many_entries() {
        let too_many: Vec<String> = (0..MAX_LINKS + 1)
            .map(|i| format!("https://example.com/{i}"))
            .collect();
        assert!(matches!(
            validate_links(&too_many),
            Err(AppError::TooManyLinks)
        ));
    }

    #[test]
    fn validate_links_rejects_a_non_url_entry() {
        assert!(matches!(
            validate_links(&["not a url".to_string()]),
            Err(AppError::InvalidLink)
        ));
    }

    #[test]
    fn validate_links_rejects_a_javascript_scheme() {
        assert!(matches!(
            validate_links(&["javascript:alert(1)".to_string()]),
            Err(AppError::InvalidLink)
        ));
    }

    #[test]
    fn validate_timezone_treats_empty_string_as_clear_not_an_error() {
        assert_eq!(validate_timezone("").unwrap(), None);
    }

    #[test]
    fn validate_timezone_accepts_a_value_within_the_cap() {
        assert_eq!(
            validate_timezone("America/New_York").unwrap(),
            Some("America/New_York".to_string())
        );
    }

    #[test]
    fn validate_timezone_rejects_an_over_length_value() {
        let overlong = "a".repeat(MAX_TIMEZONE_LEN + 1);
        assert!(matches!(
            validate_timezone(&overlong),
            Err(AppError::InvalidTimezone)
        ));
    }

    #[test]
    fn validate_theme_color_treats_empty_string_as_clear_not_an_error() {
        assert_eq!(validate_theme_color("").unwrap(), None);
    }

    #[test]
    fn validate_theme_color_accepts_a_valid_hex_color() {
        assert_eq!(
            validate_theme_color("#a1b2c3").unwrap(),
            Some("#a1b2c3".to_string())
        );
    }

    #[test]
    fn validate_theme_color_rejects_a_missing_hash() {
        assert!(matches!(
            validate_theme_color("a1b2c3"),
            Err(AppError::InvalidThemeColor)
        ));
    }

    #[test]
    fn validate_theme_color_rejects_wrong_length() {
        assert!(matches!(
            validate_theme_color("#a1b2c"),
            Err(AppError::InvalidThemeColor)
        ));
    }

    #[test]
    fn validate_theme_color_rejects_non_hex_characters() {
        assert!(matches!(
            validate_theme_color("#zzzzzz"),
            Err(AppError::InvalidThemeColor)
        ));
    }

    #[test]
    fn validate_location_treats_empty_string_as_clear_not_an_error() {
        assert_eq!(validate_location("").unwrap(), None);
    }

    #[test]
    fn validate_location_accepts_a_value_within_the_cap() {
        assert_eq!(
            validate_location("Pacific Northwest").unwrap(),
            Some("Pacific Northwest".to_string())
        );
    }

    #[test]
    fn validate_location_rejects_an_over_length_value() {
        let overlong = "a".repeat(MAX_LOCATION_LEN + 1);
        assert!(matches!(
            validate_location(&overlong),
            Err(AppError::InvalidLocation)
        ));
    }
}
