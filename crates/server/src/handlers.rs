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
use avalon_protocol::identity::{
    Genre, MAX_BIO_LEN, MAX_FAVORITE_GENRES, MAX_LINKS, MAX_LINK_LEN, MAX_LOCATION_LEN,
    MAX_PRONOUNS_LEN, MAX_STATUS_LEN, MAX_TIMEZONE_LEN,
};
use avalon_protocol::ids::GlobalId;
use axum::extract::{Query, State};
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
    use rand::RngExt;
    const MAX_ATTEMPTS: u32 = 20;
    for _ in 0..MAX_ATTEMPTS {
        // `rng()` is `!Send` and must not live across an `.await` —
        // dropping it within this statement (rather than binding it once
        // outside the loop) keeps this function's future `Send`, which
        // axum's `Handler` bound requires of every route it's awaited from.
        let candidate = format!("{:04}", rand::rng().random_range(0..10000));
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
    /// A user-chosen label for the device completing this ceremony (e.g.
    /// "Work laptop") — purely descriptive, never part of what's signed.
    /// Issue #145: previously the first device's `identity_signing_keys`
    /// row was always unlabeled, unlike every device added later through
    /// #135's grant flow (which does carry a label).
    pub device_label: Option<String>,
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

    // Chosen before the event is built so the event can carry it: a rebuild
    // of `profiles` from history (#43) has to land on the same handle, and
    // the discriminator is server-chosen, not derivable from the name.
    let discriminator = generate_unique_discriminator(&state, &ceremony.display_name).await?;

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
        // The initial promised-durable profile state rides along (#86) —
        // `display_name` is the identity's public face, not a login
        // credential (no `username` exists anywhere), and without it and the
        // discriminator `profiles` couldn't be rebuilt from history.
        payload: serde_json::json!({
            "identity_id": ceremony.identity_id,
            "display_name": ceremony.display_name,
            "discriminator": discriminator,
        }),
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

    // `profiles` is a projection (issue #42): the row is written by the
    // indexer applying `event` below, not by an `INSERT` here. Calling
    // `apply_in_tx` against this same transaction — rather than
    // `state.indexer.apply`, which would open its own — keeps the
    // identity/profile/outbox rows committing or rolling back together.
    state.indexer.apply_in_tx(&mut tx, &event).await?;

    sqlx::query(
        "INSERT INTO identity_keys (identity_id, credential_id, passkey_data) VALUES ($1, $2, $3)",
    )
    .bind(ceremony.identity_id)
    .bind(credential_id)
    .bind(&passkey_json)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO identity_signing_keys (identity_id, public_key, label) VALUES ($1, $2, $3)",
    )
    .bind(ceremony.identity_id)
    .bind(&public_key_bytes)
    .bind(&body.device_label)
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
    /// Small, user-optional self-description fields (issue #155) — same
    /// promised-durable tier and same public exposure level as
    /// `display_name`/`avatar_url` above (no capability gate, no game ever
    /// sees more of it than `GET /me`/`GET /identities/profiles` already
    /// expose).
    pub bio: Option<String>,
    pub favorite_genres: Vec<Genre>,
    pub pronouns: Option<String>,
    /// Expanded self-described fields (issue #372) — same promised-durable
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
}

fn profile_row_to_response(
    identity_id: Uuid,
    row: &sqlx::postgres::PgRow,
    effective_main_guild: Option<Uuid>,
) -> Result<ProfileResponse, AppError> {
    let display_name: String = row.try_get("display_name")?;
    let discriminator: String = row.try_get("discriminator")?;
    let favorite_genres: Vec<String> = row.try_get("favorite_genres")?;
    let links: Vec<String> = row.try_get("links")?;
    let main_guild: Option<Uuid> = row.try_get("main_guild")?;
    Ok(ProfileResponse {
        identity_id,
        identity_created_at: row.try_get("created_at")?,
        handle: format!("{display_name}#{discriminator}"),
        display_name,
        avatar_url: row.try_get("avatar_url")?,
        bio: row.try_get("bio")?,
        // Stored genre strings are already server-validated at write time
        // (`validate_favorite_genres`), so an unparseable value here would
        // mean data corruption, not a client error — dropped rather than
        // failing the whole read.
        favorite_genres: favorite_genres
            .iter()
            .filter_map(|g| Genre::parse(g))
            .collect(),
        pronouns: row.try_get("pronouns")?,
        banner_url: row.try_get("banner_url")?,
        status: row.try_get("status")?,
        links,
        timezone: row.try_get("timezone")?,
        theme_color: row.try_get("theme_color")?,
        location: row.try_get("location")?,
        main_guild,
        effective_main_guild,
        discoverable: row.try_get("discoverable")?,
    })
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
    let guild_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT guild_id FROM guild_members WHERE identity_id = $1 ORDER BY joined_at ASC LIMIT 1",
    )
    .bind(identity_id)
    .fetch_optional(executor)
    .await?;
    Ok(guild_id)
}

/// `profiles` LEFT JOINed against `discovery_preferences` — a row there
/// only exists once an identity has toggled `discoverable` at least once
/// (see `discovery::set_discoverable`), so the join has to be outer, and
/// the missing-row case has to `COALESCE` down to `false`: absence means
/// "not discoverable", never NULL/unknown. Shared by [`me`] and
/// [`update_profile`] so the two reads can never drift.
const PROFILE_SELECT: &str = r#"
    SELECT p.display_name, p.discriminator, p.avatar_url, p.bio,
           p.favorite_genres, p.pronouns, p.banner_url, p.status, p.links,
           p.timezone, p.theme_color, p.location, p.main_guild, i.created_at,
           COALESCE(dp.discoverable, false) AS discoverable
    FROM profiles p
    JOIN identities i ON i.id = p.identity_id
    LEFT JOIN discovery_preferences dp ON dp.identity_id = p.identity_id
    WHERE p.identity_id = $1
    "#;

pub async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ProfileResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let row = sqlx::query(PROFILE_SELECT)
        .bind(identity_id)
        .fetch_one(&state.pool)
        .await?;
    let main_guild: Option<Uuid> = row.try_get("main_guild")?;
    let effective_main_guild = match main_guild {
        Some(guild_id) => Some(guild_id),
        None => earliest_joined_guild(&state.pool, identity_id).await?,
    };

    Ok(Json(profile_row_to_response(
        identity_id,
        &row,
        effective_main_guild,
    )?))
}

const PROFILE_LOOKUP_MAX_IDS: usize = 100;

#[derive(Deserialize)]
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
/// `pronouns` (#155) and `banner_url`/`status`/`links`/`timezone`/
/// `theme_color`/`location` (#372) are deliberately withheld here even
/// though they're unauthenticated-readable on one's own `GET /me` — batch
/// stranger lookup is a materially wider exposure than a single
/// self-disclosed profile view, and widening it is a scoping decision for
/// its own ticket, not a side effect of adding the columns.
#[derive(Serialize)]
pub struct PublicProfileResponse {
    pub identity_id: Uuid,
    pub display_name: String,
    pub discriminator: String,
    pub avatar_url: Option<String>,
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

    let rows = sqlx::query(
        "SELECT identity_id, display_name, discriminator, avatar_url \
         FROM profiles WHERE identity_id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(&state.pool)
    .await?;

    let mut profiles = Vec::with_capacity(rows.len());
    for row in rows {
        profiles.push(PublicProfileResponse {
            identity_id: row.try_get("identity_id")?,
            display_name: row.try_get("display_name")?,
            discriminator: row.try_get("discriminator")?,
            avatar_url: row.try_get("avatar_url")?,
        });
    }
    Ok(Json(profiles))
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
    /// `null` if this event's payload has been pruned locally (issue
    /// #208, a hot-tier node) — the event's existence and `kind` are still
    /// reported, just not its content.
    pub payload: Option<serde_json::Value>,
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
    /// Three states, same as `avatar_url`: omitted (untouched), `Some("")`
    /// (clear to `NULL`), `Some(nonempty)` (validate against
    /// [`MAX_BIO_LEN`], then set).
    pub bio: Option<String>,
    /// Two states, not three: omitted (untouched) or `Some(list)`, which
    /// always fully replaces the stored list — including `Some(vec![])` to
    /// clear it. Each entry must parse as a [`Genre`]; an unknown value is
    /// rejected outright rather than silently dropped (issue #155).
    pub favorite_genres: Option<Vec<String>>,
    /// Three states, same as `bio`.
    pub pronouns: Option<String>,
    /// Three states, same as `avatar_url` — a second image slot, separate
    /// from the avatar, for the Hub profile page header (issue #372).
    pub banner_url: Option<String>,
    /// Three states, same as `bio`, capped at
    /// [`avalon_protocol::identity::MAX_STATUS_LEN`].
    pub status: Option<String>,
    /// Two states, not three: omitted (untouched) or `Some(list)`, which
    /// always fully replaces the stored list — including `Some(vec![])` to
    /// clear it. Same shape as `favorite_genres`, but each entry is a
    /// free-form URL rather than a fixed vocabulary value (issue #372).
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
    /// Issue #205's opt-in global search toggle. Two states, not three
    /// (there's no "clear" state for a plain boolean): `None` leaves the
    /// existing preference untouched, `Some(bool)` sets it. Off by
    /// default for every identity (no row in `discovery_preferences` at
    /// all reads as `false`) — this field is the only way it ever
    /// becomes `true`. Not part of `profile.updated`/durable history, and
    /// not written through the same transaction as the rest of this
    /// request's changes — see `discovery::set_discoverable`'s doc
    /// comment for why, matching `presence_preferences.hide_playing`'s
    /// identical precedent.
    pub discoverable: Option<bool>,
}

/// Nothing renders `avatar_url` as an actual image anywhere in the Hub
/// today, so there's no live XSS path yet — but that's incidental, not a
/// guarantee: the field exists so a user-supplied avatar shows up
/// somewhere later (friends list, guild roster), and an unvalidated
/// `javascript:`/`data:`-scheme string sitting in storage is exactly the
/// kind of thing that becomes a real problem the moment something renders
/// it with `<img :src>` without re-checking this. Validated before the
/// `UPDATE profiles` write, never after.
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

/// `favorite_genres` is a fixed, small controlled vocabulary, not free text
/// (issue #155) — an unknown value is rejected outright, not silently
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
/// database, since no such crate exists in this workspace today (issue
/// #372); a known, documented gap, not silently pretended-correct.
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
    let is_member: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM guild_members WHERE guild_id = $1 AND identity_id = $2)",
    )
    .bind(guild_id)
    .bind(identity_id)
    .fetch_one(pool)
    .await?;
    if !is_member {
        return Err(AppError::NotGuildMember);
    }
    Ok(Some(guild_id))
}

/// A display-name change can collide with someone else's existing handle
/// (same name, same discriminator) — the discriminator itself never changes
/// on its own, but if the *new* name collides under it, a fresh one has to
/// be picked so `(display_name, discriminator)` stays unique. No collision
/// (the common case) keeps the identity's existing discriminator, so a
/// user's handle doesn't churn just because they tweaked their name.
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

/// The payload `profile.updated` carries (#86, widened by #155): only the
/// fields this request actually changed. `discriminator` rides along with a
/// display-name change because a rebuild of `profiles` from history (#43)
/// has to land on the same handle, and the discriminator is server-chosen,
/// not derivable from the name. An explicitly cleared `avatar_url`/`bio`/
/// `pronouns` is `null`; an untouched one is absent — the same three-state
/// distinction `update_profile` itself makes. `favorite_genres` has only two
/// states: absent (untouched) or present (the new, complete list, including
/// `[]` to clear it).
#[allow(clippy::too_many_arguments)]
pub(crate) fn profile_updated_payload(
    display_name: Option<&str>,
    discriminator: Option<&str>,
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
    let mut payload = serde_json::Map::new();
    if let Some(name) = display_name {
        payload.insert("display_name".into(), name.into());
    }
    if let Some(discriminator) = discriminator {
        payload.insert("discriminator".into(), discriminator.into());
    }
    if let Some(avatar_url) = avatar_url {
        payload.insert(
            "avatar_url".into(),
            avatar_url.map_or(serde_json::Value::Null, Into::into),
        );
    }
    if let Some(bio) = bio {
        payload.insert(
            "bio".into(),
            bio.map_or(serde_json::Value::Null, Into::into),
        );
    }
    if let Some(genres) = favorite_genres {
        payload.insert(
            "favorite_genres".into(),
            genres.iter().map(Genre::as_str).collect::<Vec<_>>().into(),
        );
    }
    if let Some(pronouns) = pronouns {
        payload.insert(
            "pronouns".into(),
            pronouns.map_or(serde_json::Value::Null, Into::into),
        );
    }
    if let Some(banner_url) = banner_url {
        payload.insert(
            "banner_url".into(),
            banner_url.map_or(serde_json::Value::Null, Into::into),
        );
    }
    if let Some(status) = status {
        payload.insert(
            "status".into(),
            status.map_or(serde_json::Value::Null, Into::into),
        );
    }
    if let Some(links) = links {
        payload.insert("links".into(), links.to_vec().into());
    }
    if let Some(timezone) = timezone {
        payload.insert(
            "timezone".into(),
            timezone.map_or(serde_json::Value::Null, Into::into),
        );
    }
    if let Some(theme_color) = theme_color {
        payload.insert(
            "theme_color".into(),
            theme_color.map_or(serde_json::Value::Null, Into::into),
        );
    }
    if let Some(location) = location {
        payload.insert(
            "location".into(),
            location.map_or(serde_json::Value::Null, Into::into),
        );
    }
    if let Some(main_guild) = main_guild {
        payload.insert(
            "main_guild".into(),
            main_guild.map_or(serde_json::Value::Null, |guild_id| {
                guild_id.to_string().into()
            }),
        );
    }
    serde_json::Value::Object(payload)
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

    // `discoverable` (#205) is deliberately handled outside the
    // transaction below, the same way `presence::update_my_presence`
    // handles `hide_playing`: it's a user preference, not durable
    // protocol history, so it has no `profile.updated` payload and needs
    // no atomicity with the rest of this request's changes. Applied only
    // now, after every fallible validation above has already succeeded —
    // never before them. `discriminator_for_rename`/`validate_*` can each
    // still fail and abort this handler with an `AppError`; running this
    // write any earlier would let an otherwise-failed PATCH /me (a bad
    // avatar_url, an invalid genre, ...) leave `discoverable` flipped
    // anyway, which is exactly the kind of partial-write a default-off,
    // no-exceptions preference must never have. Still applied before the
    // `PROFILE_SELECT` read further down, so that read already reflects it.
    if let Some(discoverable) = body.discoverable {
        crate::discovery::set_discoverable(&state, identity_id, discoverable).await?;
    }

    // `display_name`, `avatar_url`, `bio`, `favorite_genres`, and `pronouns`
    // are all promised-durable (ADR #75, the table in
    // docs/architecture/identity.md; #155 widened the set), so a change to
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
            kind: "profile.updated".to_string(),
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
                discriminator.as_deref(),
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

    // `profiles` is a projection (issue #42): the write below happens
    // through the indexer applying `event`, in this same transaction, not
    // through a bespoke `UPDATE` here — matching `register_finish`. A
    // request that changed nothing has no event, so nothing to apply; the
    // `SELECT` after this still returns the (unchanged) current row.
    if let Some(event) = &event {
        state.indexer.apply_in_tx(&mut tx, event).await?;
        outbox::enqueue(&mut tx, event).await?;
    }

    let row = sqlx::query(PROFILE_SELECT)
        .bind(identity_id)
        .fetch_one(&mut *tx)
        .await?;
    let row_main_guild: Option<Uuid> = row.try_get("main_guild")?;
    let effective_main_guild = match row_main_guild {
        Some(guild_id) => Some(guild_id),
        None => earliest_joined_guild(&mut *tx, identity_id).await?,
    };

    tx.commit().await?;

    Ok(Json(profile_row_to_response(
        identity_id,
        &row,
        effective_main_guild,
    )?))
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
            Some("4821"),
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
        assert_eq!(
            payload,
            serde_json::json!({ "display_name": "nova", "discriminator": "4821" })
        );
        assert!(payload.get("avatar_url").is_none());
        assert!(payload.get("bio").is_none());
        assert!(payload.get("favorite_genres").is_none());
        assert!(payload.get("pronouns").is_none());
    }

    #[test]
    fn profile_updated_payload_distinguishes_a_cleared_avatar_from_an_untouched_one() {
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
            None,
        );
        assert_eq!(cleared, serde_json::json!({ "avatar_url": null }));

        let set = profile_updated_payload(
            None,
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
            Some("4821"),
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
            None, None, None, None, None, None, None, None, None, None, None, None, None,
        );
        assert!(untouched.get("bio").is_none());
    }

    #[test]
    fn profile_updated_payload_carries_favorite_genres_as_strings() {
        let payload = profile_updated_payload(
            None,
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
