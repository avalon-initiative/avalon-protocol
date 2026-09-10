//! Game registration and server-to-server game authentication (issue #26).
//!
//! Backs `crates/protocol/src/games.rs`'s `Game` / `GameRegistration` /
//! `GameCredential` types. `POST /games` is how a game first becomes known
//! to Avalon — nothing before this ticket lets one register at all.
//! Registering never grants access by itself (see this module's own
//! invariant tests below and #27, which owns the actual grant/consent
//! logic); `requested_capabilities` is only ever a declaration presented to
//! players.
//!
//! **Slugs are forever.** A slug is the `owner` segment of every `GlobalId`
//! the game later mints (`game:<slug>:achievement:<key>`), so it must be
//! lowercase `[a-z0-9-]` and unique — enforced with a unique index plus an
//! `is_unique_violation()` catch (409 on collision), the same pattern
//! `crates/server/src/guilds.rs::create_guild` already established for
//! name/tag uniqueness, not a pre-check-then-insert race. Renaming a slug is
//! not supported by this or any endpoint.
//!
//! **`game.registered` is network-attributed, not game-signed**, even
//! though the event-kind catalogue (`docs/architecture/protocol-events.md`)
//! lists its long-run signer as "game key": at the moment this event is
//! built, nothing has verified the registrant actually controls the
//! submitted key yet (that's exactly why the challenge-response endpoint
//! below exists), so claiming a real signature here would be dishonest. This
//! is the same "network as signer" milestone-1 stand-in
//! `crates/server/src/friends.rs` and `handlers::update_profile` already use
//! for their own events — `issuer`/`subject` both name the game itself
//! (`game_ref`, mirroring `guilds.rs`'s `guild_ref` helper), attributed to
//! the registering request rather than to a player identity or a proven
//! device key. The row insert and the event enqueue happen in one
//! transaction via the outbox pattern (`crates/server/src/outbox.rs`),
//! closing #71's gap for this path too.
//!
//! **Server-to-server auth for a game is a signature, not a shared
//! secret.** Issue #80 (which signing scheme/key-management approach to use
//! generally) is still an open decision in this repo, so this implements a
//! simple challenge-response as a milestone-1 stand-in pending that
//! decision, plainly documented as such rather than silently downgraded to
//! something permanent-looking: `POST /games/{slug}/challenge` issues a
//! short-lived random nonce (same ephemeral-ceremony shape
//! `handlers.rs`'s `webauthn_ceremonies` table/TTL uses, minus any WebAuthn
//! involvement), and [`authenticate_game`] verifies a detached Ed25519
//! signature over that nonce against the key recorded for the claimed
//! `key_id`, reusing `crate::auth::verify_event_signature` rather than
//! reimplementing signature verification. `GET /games/whoami` exists only to
//! prove this extractor works end to end; `crate::connections` (#27) owns
//! the real capability-bearing endpoints that use it.
//!
//! `GET /games/{slug}` ([`get_game`]) is a public, unauthenticated read of
//! a game's registration — no credential fields, unlike the one-time
//! [`GameResponse`] `register_game` itself returns. `crate::connections`
//! reads it to validate a player's approved capabilities against what the
//! game actually declared, and the Hub's consent view reads it to render
//! the game's name/developer/requested capabilities.
//!
//! Deferred to #84: key rotation, multiple keys, revocation, issuer status
//! transitions — this only ever records the first key and sets
//! `status = active`.

use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::games::GameStatus;
use avalon_protocol::ids::GlobalId;
use avalon_protocol::permissions::Capability;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::verify_event_signature;
use crate::error::AppError;
use crate::outbox;
use crate::state::AppState;

const GAME_CHALLENGE_TTL_MINUTES: i64 = 5;
const GAME_CHALLENGE_NONCE_BYTES: usize = 32;

const GAME_KEY_ID_HEADER: &str = "x-avalon-game-key-id";
const GAME_CHALLENGE_ID_HEADER: &str = "x-avalon-game-challenge-id";
const GAME_SIGNATURE_HEADER: &str = "x-avalon-game-signature";

/// Only algorithm `crate::auth::verify_event_signature` (and thus the
/// challenge-response scheme below) can verify. Registration itself rejects
/// anything else up front rather than silently accepting a key it can never
/// later authenticate.
const SUPPORTED_KEY_ALGORITHM: &str = "ed25519";

/// `pub(crate)` so `connections.rs` (#27/#83) can build the same
/// `game:<slug>:self:<verb>` `GlobalId` shape for `game.binding_established`/
/// `game.binding_ended` subjects, rather than reimplementing this format.
pub(crate) fn game_ref(slug: &str, verb: &str) -> GlobalId {
    GlobalId::new("game", slug, "self", verb)
}

/// Lowercase `[a-z0-9-]`, 2-64 characters. Deliberately rejects rather than
/// normalizes an out-of-charset slug (e.g. uppercase) — silently rewriting
/// it would surprise a caller who submitted something else and would still
/// have to be documented as a distinct behavior from what they asked for.
fn validate_slug(slug: &str) -> Result<(), AppError> {
    let len = slug.chars().count();
    if !(2..=64).contains(&len) {
        return Err(AppError::InvalidGameSlug);
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(AppError::InvalidGameSlug);
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct InitialKeyRequest {
    pub algorithm: String,
    /// Standard-base64-encoded raw public key bytes.
    pub public_key: String,
}

#[derive(Deserialize)]
pub struct CreateGameRequest {
    pub slug: String,
    pub name: String,
    pub developer: String,
    #[serde(default)]
    pub requested_capabilities: Vec<String>,
    pub initial_key: InitialKeyRequest,
}

#[derive(Serialize)]
pub struct GameCredentialResponse {
    pub game_id: Uuid,
    pub key_id: String,
}

#[derive(Serialize)]
pub struct GameResponse {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub developer: String,
    #[serde(with = "time::serde::rfc3339")]
    pub registered_at: OffsetDateTime,
    pub status: String,
    pub requested_capabilities: Vec<String>,
    pub credential: GameCredentialResponse,
}

pub async fn register_game(
    State(state): State<AppState>,
    Json(body): Json<CreateGameRequest>,
) -> Result<Json<GameResponse>, AppError> {
    validate_slug(&body.slug)?;

    if body.initial_key.algorithm != SUPPORTED_KEY_ALGORITHM {
        return Err(AppError::InvalidGameKey);
    }
    let public_key_bytes = BASE64
        .decode(&body.initial_key.public_key)
        .map_err(|_| AppError::InvalidGameKey)?;

    // Normalized through `Capability` so an unrecognized string round-trips
    // rather than erroring (see `crates/protocol/src/permissions.rs`'s own
    // doc comment) — a declaration presented to players, never validated
    // against a closed vocabulary here.
    let requested_capabilities: Vec<String> = body
        .requested_capabilities
        .iter()
        .map(|c| Capability::from(c.as_str()).as_str().to_string())
        .collect();

    let game_id = Uuid::new_v4();
    let key_id = Uuid::new_v4();
    let registered_at = OffsetDateTime::now_utc();

    let mut tx = state.pool.begin().await?;

    let inserted = sqlx::query(
        "INSERT INTO games (id, slug, name, developer, registered_at, status) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(game_id)
    .bind(&body.slug)
    .bind(&body.name)
    .bind(&body.developer)
    .bind(registered_at)
    .bind(GameStatus::Active.as_str())
    .execute(&mut *tx)
    .await;
    if let Err(sqlx::Error::Database(db_err)) = &inserted {
        if db_err.is_unique_violation() {
            return Err(AppError::GameSlugTaken);
        }
    }
    inserted?;

    for capability in &requested_capabilities {
        sqlx::query(
            "INSERT INTO game_requested_capabilities (game_id, capability) VALUES ($1, $2)",
        )
        .bind(game_id)
        .bind(capability)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query(
        "INSERT INTO issuer_keys (key_id, game_id, algorithm, public_key, created_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(key_id)
    .bind(game_id)
    .bind(&body.initial_key.algorithm)
    .bind(&public_key_bytes)
    .bind(registered_at)
    .execute(&mut *tx)
    .await?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "game.registered".to_string(),
        issuer: game_ref(&body.slug, "registered"),
        subject: game_ref(&body.slug, "registered"),
        payload: serde_json::json!({
            "game_id": game_id,
            "slug": body.slug,
            "name": body.name,
            "developer": body.developer,
            "requested_capabilities": requested_capabilities,
            "initial_key": {
                "key_id": key_id,
                "algorithm": body.initial_key.algorithm,
                "public_key": BASE64.encode(&public_key_bytes),
            },
        }),
        timestamp: registered_at,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(GameResponse {
        id: game_id,
        slug: body.slug,
        name: body.name,
        developer: body.developer,
        registered_at,
        status: GameStatus::Active.as_str().to_string(),
        requested_capabilities,
        credential: GameCredentialResponse {
            game_id,
            key_id: key_id.to_string(),
        },
    }))
}

/// `pub(crate)` so `connections.rs` can resolve a slug to a game id without
/// duplicating this lookup.
pub(crate) async fn fetch_game_id_by_slug(state: &AppState, slug: &str) -> Result<Uuid, AppError> {
    let row = sqlx::query("SELECT id FROM games WHERE slug = $1")
        .bind(slug)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::GameNotFound)?;
    Ok(row.try_get("id")?)
}

#[derive(Serialize)]
pub struct GamePublicResponse {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub developer: String,
    #[serde(with = "time::serde::rfc3339")]
    pub registered_at: OffsetDateTime,
    pub status: String,
    pub requested_capabilities: Vec<String>,
}

/// A public read of a game's registration — no credential fields, unlike
/// [`GameResponse`] (which only `register_game` itself ever returns, to the
/// registrant, once). This is what the Hub's consent view (#27) and
/// `connections.rs`'s `POST /games/{slug}/connect` (to validate approved
/// capabilities against what the game actually declared) both read; same
/// visibility level `crates/server/src/guilds.rs`'s `get_guild` uses — no
/// auth required, nothing here is sensitive.
pub async fn get_game(
    State(state): State<AppState>,
    axum::extract::Path(slug): axum::extract::Path<String>,
) -> Result<Json<GamePublicResponse>, AppError> {
    let row = sqlx::query(
        "SELECT id, slug, name, developer, registered_at, status FROM games WHERE slug = $1",
    )
    .bind(&slug)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::GameNotFound)?;

    let game_id: Uuid = row.try_get("id")?;
    let capability_rows =
        sqlx::query("SELECT capability FROM game_requested_capabilities WHERE game_id = $1")
            .bind(game_id)
            .fetch_all(&state.pool)
            .await?;
    let requested_capabilities = capability_rows
        .into_iter()
        .map(|r| r.try_get::<String, _>("capability"))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Json(GamePublicResponse {
        id: game_id,
        slug: row.try_get("slug")?,
        name: row.try_get("name")?,
        developer: row.try_get("developer")?,
        registered_at: row.try_get("registered_at")?,
        status: row.try_get("status")?,
        requested_capabilities,
    }))
}

#[derive(Serialize)]
pub struct GameChallengeResponse {
    pub challenge_id: Uuid,
    /// Standard-base64-encoded random nonce the game must sign with its
    /// registered key and echo back (see [`authenticate_game`]).
    pub nonce: String,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

pub async fn create_game_challenge(
    State(state): State<AppState>,
    axum::extract::Path(slug): axum::extract::Path<String>,
) -> Result<Json<GameChallengeResponse>, AppError> {
    let game_id = fetch_game_id_by_slug(&state, &slug).await?;

    let mut nonce = [0u8; GAME_CHALLENGE_NONCE_BYTES];
    rand::rng().fill_bytes(&mut nonce);
    let challenge_id = Uuid::new_v4();
    let expires_at =
        OffsetDateTime::now_utc() + time::Duration::minutes(GAME_CHALLENGE_TTL_MINUTES);

    sqlx::query(
        "INSERT INTO game_challenges (id, game_id, nonce, expires_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(challenge_id)
    .bind(game_id)
    .bind(nonce.as_slice())
    .bind(expires_at)
    .execute(&state.pool)
    .await?;

    Ok(Json(GameChallengeResponse {
        challenge_id,
        nonce: BASE64.encode(nonce),
        expires_at,
    }))
}

fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, AppError> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::InvalidGameSignature)
}

/// Verifies a game's server-to-server request via the challenge-response
/// scheme this module's doc comment describes (the milestone-1 stand-in
/// pending #80). Reads `key_id` / `challenge_id` / a base64 detached Ed25519
/// signature from fixed headers, consumes the matching `game_challenges` row
/// with a single `DELETE ... RETURNING` — the same single-use pattern
/// `handlers::register_finish` uses for `webauthn_ceremonies`, so a captured
/// signature can't be replayed against a second request — checks its TTL,
/// then verifies the signature against the stored public key for that
/// `key_id` via [`verify_event_signature`]. Returns the authenticated
/// game's id. `pub(crate)` so future game-authenticated endpoints (#27's
/// capability grants, etc.) can reuse it.
pub(crate) async fn authenticate_game(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Uuid, AppError> {
    let key_id: Uuid = header_value(headers, GAME_KEY_ID_HEADER)?
        .parse()
        .map_err(|_| AppError::InvalidGameSignature)?;
    let challenge_id: Uuid = header_value(headers, GAME_CHALLENGE_ID_HEADER)?
        .parse()
        .map_err(|_| AppError::InvalidGameSignature)?;
    let signature_bytes = BASE64
        .decode(header_value(headers, GAME_SIGNATURE_HEADER)?)
        .map_err(|_| AppError::InvalidGameSignature)?;

    let challenge_row = sqlx::query(
        "DELETE FROM game_challenges WHERE id = $1 RETURNING game_id, nonce, expires_at",
    )
    .bind(challenge_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::GameChallengeNotFound)?;
    let expires_at: OffsetDateTime = challenge_row.try_get("expires_at")?;
    if expires_at < OffsetDateTime::now_utc() {
        return Err(AppError::GameChallengeExpired);
    }
    let game_id: Uuid = challenge_row.try_get("game_id")?;
    let nonce: Vec<u8> = challenge_row.try_get("nonce")?;

    let key_row =
        sqlx::query("SELECT public_key FROM issuer_keys WHERE key_id = $1 AND game_id = $2")
            .bind(key_id)
            .bind(game_id)
            .fetch_optional(&state.pool)
            .await?
            .ok_or(AppError::GameKeyNotFound)?;
    let public_key: Vec<u8> = key_row.try_get("public_key")?;

    if !verify_event_signature(&public_key, &nonce, &signature_bytes) {
        return Err(AppError::InvalidGameSignature);
    }

    Ok(game_id)
}

#[derive(Serialize)]
pub struct GameWhoamiResponse {
    pub game_id: Uuid,
}

/// Exists only to prove [`authenticate_game`] works end to end over real
/// HTTP (this ticket's own suggestion) — not a real capability-bearing
/// endpoint; #27 owns those.
pub async fn game_whoami(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<GameWhoamiResponse>, AppError> {
    let game_id = authenticate_game(&state, &headers).await?;
    Ok(Json(GameWhoamiResponse { game_id }))
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — pure-logic checks only. The
    //! endpoint-level flows (register, slug collision, challenge-response
    //! round-trip) are covered by `crates/server/tests/games.rs`, gated
    //! `--ignored`.

    use ed25519_dalek::{Signer, SigningKey};

    use super::*;

    #[test]
    fn validate_slug_accepts_lowercase_alphanumeric_and_hyphen() {
        assert!(validate_slug("ashen-realms").is_ok());
        assert!(validate_slug("a1-2").is_ok());
    }

    #[test]
    fn validate_slug_rejects_uppercase() {
        assert!(validate_slug("Ashen-Realms").is_err());
    }

    #[test]
    fn validate_slug_rejects_disallowed_characters() {
        assert!(validate_slug("ashen_realms").is_err());
        assert!(validate_slug("ashen realms").is_err());
        assert!(validate_slug("ashen.realms").is_err());
    }

    #[test]
    fn validate_slug_rejects_too_short_or_too_long() {
        assert!(validate_slug("a").is_err());
        assert!(validate_slug(&"a".repeat(65)).is_err());
        assert!(validate_slug(&"a".repeat(64)).is_ok());
    }

    #[test]
    fn game_ref_namespaces_by_slug_and_verb() {
        let global_id = game_ref("ashen-realms", "registered");
        assert_eq!(global_id.as_str(), "game:ashen-realms:self:registered");
    }

    /// Exercises the exact bytes/flow `authenticate_game` verifies over,
    /// without needing a database: a game signs a nonce with its own key,
    /// and `verify_event_signature` (the helper `authenticate_game` reuses
    /// rather than reimplementing) accepts it — and rejects a signature
    /// from any other key or over any other nonce.
    #[test]
    fn a_game_signed_nonce_verifies_against_its_own_key_only() {
        let mut csprng = rand::rng();
        let signing_key = SigningKey::generate(&mut csprng);
        let nonce = b"a-challenge-nonce";
        let signature = signing_key.sign(nonce);

        assert!(verify_event_signature(
            signing_key.verifying_key().as_bytes(),
            nonce,
            &signature.to_bytes(),
        ));

        let other_key = SigningKey::generate(&mut csprng);
        assert!(!verify_event_signature(
            other_key.verifying_key().as_bytes(),
            nonce,
            &signature.to_bytes(),
        ));
        assert!(!verify_event_signature(
            signing_key.verifying_key().as_bytes(),
            b"a-different-nonce",
            &signature.to_bytes(),
        ));
    }
}
