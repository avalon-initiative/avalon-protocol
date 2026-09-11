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
//! **`/integrations` is the canonical public API path (#293)**, generalizing
//! #282's Hub-internal `/games` → `/integrations` route rename onto the
//! server's own public API, ahead of the POC test environment putting these
//! routes in front of real outside testers. `GET /games` and
//! `GET /games/{slug}` keep working as redirects to `GET /integrations`/
//! `GET /integrations/{slug}` ([`redirect_list_games`]/[`redirect_get_game`]);
//! `POST /games` keeps working identically to `POST /integrations` (both
//! routed to the same [`register_game`] handler, never a redirect — a 30x
//! silently turns a POST into a GET in many clients). The
//! `x-avalon-game-*`/`x-avalon-integrator-*` auth headers below follow the
//! same pattern: either name is accepted from a caller, and this repo's own
//! outbound code sends only the `integrator` name going forward. None of
//! this renames `Issuer::Game`, `GameId`, the `games` table, or any ledger
//! event kind — those stay permanent (#275).
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
//! **`GET /games` ([`list_games`], issue #270).** Same public,
//! unauthenticated visibility level as [`get_game`], cursor-paginated the
//! same way `crates/server/src/guilds.rs::discover_guilds` already is for
//! guilds (issue #154) — [`build_games_list_query`] mirrors
//! `build_discover_query`'s split-out-for-unit-testing shape and keyset
//! `(sort key, id) < / > (subquery for cursor id)` pagination exactly, just
//! over `games` instead of `guilds`. Milestone-1 stand-in: a direct query,
//! not yet a real indexer read model, same pragmatic call `discover_guilds`
//! already made. Only `name`/`newest` sorts exist — no ranking, no score,
//! matching #89's hard invariant that this ticket explicitly carries
//! forward into the Hub's game directory. Returns each game's public
//! fields only ([`GameSummary`]: id/slug/name/developer/registered_at/
//! status) — no `requested_capabilities`, since a directory listing has no
//! reason to fetch a field the card doesn't show (same reasoning
//! `DiscoverGuildSummary` already documents).
//!
//! **Key rotation and revocation (#84, implementing #80's decided two-tier
//! root/operational key role)** — [`add_issuer_key`]/[`revoke_issuer_key`]
//! below. Only a **root** key may authorize a key-set change; any
//! non-revoked, non-expired key (root or operational) may still
//! authenticate ordinary game-credentialed calls via [`authenticate_game`],
//! since role only gates who may change the key *set*, never
//! attestation-signing authority. Registration's own initial key is always
//! recorded as `role: root` — see [`register_game`] — so it doubles as
//! both the issuer's root key and its first operational key by default,
//! matching #80's "zero extra friction at signup" requirement.
//!
//! Still deferred: `GameStatus`'s `Suspended`/`Revoked`/`Deprecated`
//! variants exist (#84) but nothing in this repo can yet transition a game
//! into them — the network-level authorization model for that is
//! explicitly out of scope for #84, a separate follow-up.

use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::games::{GameStatus, IntegratorCategory, IssuerKey, KeyRole};
use avalon_protocol::ids::GlobalId;
use avalon_protocol::permissions::Capability;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, QueryBuilder, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::verify_event_signature;
use crate::error::AppError;
use crate::outbox;
use crate::state::AppState;

const GAME_CHALLENGE_TTL_MINUTES: i64 = 5;
const GAME_CHALLENGE_NONCE_BYTES: usize = 32;

/// Default/maximum page size for `GET /games` (issue #270) — same
/// "small default, capped maximum" shape
/// `guilds::DEFAULT_DISCOVER_PAGE_SIZE`/`MAX_DISCOVER_PAGE_SIZE` already use.
const DEFAULT_GAMES_LIST_PAGE_SIZE: i64 = 20;
const MAX_GAMES_LIST_PAGE_SIZE: i64 = 100;

/// Header names accepted for game/integrator server-to-server auth.
///
/// `x-avalon-game-*` is the original name; `x-avalon-integrator-*` is the
/// generic replacement added by #293 (mirroring #282's `IntegratorCategory`
/// generalization on the server's public API). Both are accepted from any
/// caller indefinitely — see [`header_value`] — but this repo's own
/// outbound code (SDK, CLI) sends only the `integrator` name from now on.
const GAME_KEY_ID_HEADER: &str = "x-avalon-game-key-id";
const INTEGRATOR_KEY_ID_HEADER: &str = "x-avalon-integrator-key-id";
const GAME_CHALLENGE_ID_HEADER: &str = "x-avalon-game-challenge-id";
const INTEGRATOR_CHALLENGE_ID_HEADER: &str = "x-avalon-integrator-challenge-id";
const GAME_SIGNATURE_HEADER: &str = "x-avalon-game-signature";
const INTEGRATOR_SIGNATURE_HEADER: &str = "x-avalon-integrator-signature";

/// Only algorithm `crate::auth::verify_event_signature` (and thus the
/// challenge-response scheme below) can verify. Registration itself rejects
/// anything else up front rather than silently accepting a key it can never
/// later authenticate.
const SUPPORTED_KEY_ALGORITHM: &str = "ed25519";

/// `pub(crate)` so `connections.rs` (#27/#83) can build the same
/// `game:<slug>:self:<verb>` `GlobalId` shape for `game.binding_established`/
/// `game.binding_ended` subjects, rather than reimplementing this format.
pub(crate) fn game_ref(slug: &str, verb: &str) -> GlobalId {
    issuer_ref("game", slug, verb)
}

/// Generic form of [`game_ref`] — `<namespace>:<slug>:self:<verb>` for any
/// issuer category's namespace (`"game"`/`"app"`/`"service"`, matching
/// `IntegratorCategory::as_str()`). `pub(crate)` so `achievements.rs` (#324)
/// can mint the same shape for `App`/`Service` issuers, not just `Game`.
pub(crate) fn issuer_ref(namespace: &str, slug: &str, verb: &str) -> GlobalId {
    GlobalId::new(namespace, slug, "self", verb)
}

/// The category (`game`/`app`/`service`) a registered game/app/service was
/// recorded under — what determines its claim vocabulary (#324:
/// `IntegratorCategory::claim_kind`). `pub(crate)` so `achievements.rs` can
/// reject a caller acting under the wrong claim-vocabulary route (an
/// `Issuer::Game` hitting `/integrations/{slug}/milestones`, or vice versa)
/// rather than silently accepting a mismatched label.
pub(crate) async fn fetch_game_category(
    state: &AppState,
    game_id: Uuid,
) -> Result<IntegratorCategory, AppError> {
    let row = sqlx::query("SELECT category FROM games WHERE id = $1")
        .bind(game_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::GameNotFound)?;
    let category_raw: String = row.try_get("category")?;
    // The column only ever gets written via `IntegratorCategory::as_str()`
    // (see `register_game` above) — an unparseable value here would mean
    // the write side and this read side have drifted, not a real runtime
    // condition to design an error path around. Defaulting to `Game`
    // (rather than panicking a live request) is safe precisely because
    // this can't actually happen in practice.
    Ok(IntegratorCategory::parse(&category_raw).unwrap_or(IntegratorCategory::Game))
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
    /// `game` / `app` / `service` (#282); omitted means `game`.
    #[serde(default)]
    pub category: Option<String>,
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
    pub category: String,
    pub requested_capabilities: Vec<String>,
    pub credential: GameCredentialResponse,
}

pub async fn register_game(
    State(state): State<AppState>,
    Json(body): Json<CreateGameRequest>,
) -> Result<Json<GameResponse>, AppError> {
    validate_slug(&body.slug)?;

    let category = match body.category.as_deref() {
        None => IntegratorCategory::Game,
        Some(raw) => IntegratorCategory::parse(raw).ok_or(AppError::InvalidGameCategory)?,
    };

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
        "INSERT INTO games (id, slug, name, developer, registered_at, status, category) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(game_id)
    .bind(&body.slug)
    .bind(&body.name)
    .bind(&body.developer)
    .bind(registered_at)
    .bind(GameStatus::Active.as_str())
    .bind(category.as_str())
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

    // #80/#84: the key registered here is always `root` — it doubles as
    // the issuer's root key and its first operational key by default (any
    // non-revoked, non-expired key may sign attestations regardless of
    // role; only key-*set* changes require root specifically).
    sqlx::query(
        "INSERT INTO issuer_keys (key_id, game_id, algorithm, public_key, role, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(key_id)
    .bind(game_id)
    .bind(&body.initial_key.algorithm)
    .bind(&public_key_bytes)
    .bind(KeyRole::Root.as_str())
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
            "category": category.as_str(),
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
        category: category.as_str().to_string(),
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
    pub category: String,
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
        "SELECT id, slug, name, developer, registered_at, status, category FROM games WHERE slug = $1",
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
        category: row.try_get("category")?,
        requested_capabilities,
    }))
}

/// `sort=` values `GET /games` (issue #270) accepts — deliberately just
/// these two, matching #89's "no ranking, no score" invariant: `newest`
/// (default) and `name`, mirroring `guilds::DiscoverSort` minus the
/// membership-derived `most_members` option games have no equivalent of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GamesListSort {
    Newest,
    Name,
}

impl GamesListSort {
    fn parse(raw: Option<&str>) -> Result<GamesListSort, AppError> {
        Ok(match raw {
            None | Some("newest") => GamesListSort::Newest,
            Some("name") => GamesListSort::Name,
            Some(_) => return Err(AppError::InvalidGamesListQuery),
        })
    }
}

#[derive(Deserialize)]
pub struct ListGamesQuery {
    /// Free-text search over `name`/`slug`/`developer` (case-insensitive
    /// substring) — same shape `guilds::DiscoverGuildsQuery::q` uses.
    pub q: Option<String>,
    /// `newest` (default) | `name`.
    pub sort: Option<String>,
    pub limit: Option<i64>,
    /// The last game id from the previous page's results — same bare-id
    /// cursor shape `guilds::DiscoverGuildsQuery::cursor` uses.
    pub cursor: Option<Uuid>,
}

#[derive(Serialize)]
pub struct GameSummary {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub developer: String,
    #[serde(with = "time::serde::rfc3339")]
    pub registered_at: OffsetDateTime,
    pub status: String,
    pub category: String,
}

#[derive(Serialize)]
pub struct ListGamesResponse {
    pub games: Vec<GameSummary>,
    /// `Some(id)` when another page exists — pass it back as `cursor=` to
    /// fetch it. `None` means this was the last page.
    pub next_cursor: Option<Uuid>,
}

/// Builds the `GET /games` query — split out from [`list_games`] so the
/// filter/sort/pagination logic can be unit-tested (via
/// [`sqlx::QueryBuilder::sql`]) without a live Postgres connection, same
/// pattern `guilds::build_discover_query` already established for #154.
fn build_games_list_query(
    query: &ListGamesQuery,
    sort: GamesListSort,
    limit: i64,
) -> QueryBuilder<Postgres> {
    let mut builder: QueryBuilder<Postgres> = QueryBuilder::new(
        "SELECT id, slug, name, developer, registered_at, status, category FROM games WHERE 1 = 1",
    );

    if let Some(q) = query.q.as_ref().filter(|s| !s.trim().is_empty()) {
        let like = format!("%{}%", crate::guilds::escape_like(q));
        builder.push(" AND (name ILIKE ");
        builder.push_bind(like.clone());
        builder.push(" OR slug ILIKE ");
        builder.push_bind(like.clone());
        builder.push(" OR developer ILIKE ");
        builder.push_bind(like);
        builder.push(")");
    }

    if let Some(cursor_id) = query.cursor {
        match sort {
            GamesListSort::Newest => {
                builder.push(
                    " AND (registered_at, id) < (SELECT registered_at, id FROM games WHERE id = ",
                );
                builder.push_bind(cursor_id);
                builder.push(")");
            }
            GamesListSort::Name => {
                builder.push(" AND (name, id) > (SELECT name, id FROM games WHERE id = ");
                builder.push_bind(cursor_id);
                builder.push(")");
            }
        }
    }

    match sort {
        GamesListSort::Newest => builder.push(" ORDER BY registered_at DESC, id DESC"),
        GamesListSort::Name => builder.push(" ORDER BY name ASC, id ASC"),
    };

    // Fetch one extra row past the page size, purely to know whether a next
    // page exists — trimmed back off before building the response, same
    // convention `build_discover_query` uses.
    builder.push(" LIMIT ");
    builder.push_bind(limit + 1);

    builder
}

/// `GET /games?q=&sort=&limit=&cursor=` (issue #270). Public, unauthenticated
/// — same visibility level [`get_game`] already uses. See the module doc
/// comment for the pagination/sort design.
pub async fn list_games(
    State(state): State<AppState>,
    Query(query): Query<ListGamesQuery>,
) -> Result<Json<ListGamesResponse>, AppError> {
    let sort = GamesListSort::parse(query.sort.as_deref())?;
    let limit = query
        .limit
        .unwrap_or(DEFAULT_GAMES_LIST_PAGE_SIZE)
        .clamp(1, MAX_GAMES_LIST_PAGE_SIZE);

    let mut builder = build_games_list_query(&query, sort, limit);
    let rows = builder.build().fetch_all(&state.pool).await?;
    let has_more = rows.len() as i64 > limit;

    let mut games = Vec::with_capacity(rows.len().min(limit as usize));
    for row in rows.iter().take(limit as usize) {
        games.push(GameSummary {
            id: row.try_get("id")?,
            slug: row.try_get("slug")?,
            name: row.try_get("name")?,
            developer: row.try_get("developer")?,
            registered_at: row.try_get("registered_at")?,
            status: row.try_get("status")?,
            category: row.try_get("category")?,
        });
    }
    let next_cursor = if has_more {
        games.last().map(|g| g.id)
    } else {
        None
    };

    Ok(Json(ListGamesResponse { games, next_cursor }))
}

/// `GET /games` compatibility redirect (#293) → `GET /integrations`,
/// preserving the query string as-is (`?q=&sort=&limit=&cursor=`). A real
/// HTTP redirect is safe here since this is a `GET`, unlike registration
/// below (see the module doc comment / issue #293's own invariant about not
/// redirecting a `POST`).
pub async fn redirect_list_games(
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> axum::response::Redirect {
    match query {
        Some(q) if !q.is_empty() => {
            axum::response::Redirect::temporary(&format!("/integrations?{q}"))
        }
        _ => axum::response::Redirect::temporary("/integrations"),
    }
}

/// `GET /games/{slug}` compatibility redirect (#293) → `GET
/// /integrations/{slug}`. `slug` is already validated to `[a-z0-9-]` at
/// registration time, so no further escaping is needed to embed it in the
/// redirect target.
pub async fn redirect_get_game(
    axum::extract::Path(slug): axum::extract::Path<String>,
) -> axum::response::Redirect {
    axum::response::Redirect::temporary(&format!("/integrations/{slug}"))
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

/// Reads a header by trying `new_name` first, then `old_name` — either
/// name is accepted from a caller (#293), preferring the generic
/// `integrator` name when both happen to be present.
fn header_value<'a>(
    headers: &'a HeaderMap,
    new_name: &str,
    old_name: &str,
) -> Result<&'a str, AppError> {
    headers
        .get(new_name)
        .or_else(|| headers.get(old_name))
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::InvalidGameSignature)
}

fn issuer_key_from_row(row: &sqlx::postgres::PgRow) -> Result<IssuerKey, AppError> {
    let role_raw: String = row.try_get("role")?;
    Ok(IssuerKey {
        key_id: row.try_get("key_id")?,
        algorithm: row.try_get("algorithm")?,
        public_key: row.try_get("public_key")?,
        role: KeyRole::parse(&role_raw).unwrap_or(KeyRole::Operational),
        valid_from: row.try_get("created_at")?,
        valid_until: row.try_get("valid_until")?,
        revoked_at: row.try_get("revoked_at")?,
    })
}

/// Shared core of [`authenticate_game`]/[`authenticate_game_root`]: verifies
/// a game's server-to-server request via the challenge-response scheme this
/// module's doc comment describes (the milestone-1 stand-in pending #80 —
/// now decided, this scheme's own future is a separate matter #84 doesn't
/// touch). Reads `key_id` / `challenge_id` / a base64 detached Ed25519
/// signature from fixed headers, consumes the matching `game_challenges` row
/// with a single `DELETE ... RETURNING` — the same single-use pattern
/// `handlers::register_finish` uses for `webauthn_ceremonies`, so a captured
/// signature can't be replayed against a second request — checks its TTL,
/// then verifies the signature against the stored public key for that
/// `key_id` via [`verify_event_signature`]. Returns the authenticated game's
/// id and the full [`IssuerKey`] record that authenticated it, so callers
/// can apply their own role/point-in-time check (#80/#84) — this function
/// itself only proves "this request was signed by whichever key `key_id`
/// names", nothing about whether that key is currently valid or what role
/// it holds.
async fn authenticate_game_key(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(Uuid, IssuerKey), AppError> {
    let key_id: Uuid = header_value(headers, INTEGRATOR_KEY_ID_HEADER, GAME_KEY_ID_HEADER)?
        .parse()
        .map_err(|_| AppError::InvalidGameSignature)?;
    let challenge_id: Uuid = header_value(
        headers,
        INTEGRATOR_CHALLENGE_ID_HEADER,
        GAME_CHALLENGE_ID_HEADER,
    )?
    .parse()
    .map_err(|_| AppError::InvalidGameSignature)?;
    let signature_bytes = BASE64
        .decode(header_value(
            headers,
            INTEGRATOR_SIGNATURE_HEADER,
            GAME_SIGNATURE_HEADER,
        )?)
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

    let key_row = sqlx::query(
        "SELECT key_id, algorithm, public_key, role, created_at, valid_until, revoked_at \
         FROM issuer_keys WHERE key_id = $1 AND game_id = $2",
    )
    .bind(key_id)
    .bind(game_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::GameKeyNotFound)?;
    let issuer_key = issuer_key_from_row(&key_row)?;

    if !verify_event_signature(&issuer_key.public_key, &nonce, &signature_bytes) {
        return Err(AppError::InvalidGameSignature);
    }

    Ok((game_id, issuer_key))
}

/// Authenticates a game via any currently-valid key, root or operational
/// (#80/#84 — role only gates key-*set* changes, never ordinary
/// game-credentialed calls). Returns the authenticated game's id.
/// `pub(crate)` so game-authenticated endpoints (#27's capability grants,
/// achievement definitions, etc.) can reuse it.
pub(crate) async fn authenticate_game(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Uuid, AppError> {
    let (game_id, issuer_key) = authenticate_game_key(state, headers).await?;
    if !issuer_key.is_valid_at(OffsetDateTime::now_utc()) {
        return Err(AppError::GameKeyNotFound);
    }
    Ok(game_id)
}

/// Authenticates a game via a currently-valid **root** key specifically
/// (#80/#84) — what [`add_issuer_key`]/[`revoke_issuer_key`] require, since
/// only a root key may authorize a key-set change. An otherwise-valid
/// operational key fails this with the same [`AppError::IssuerKeyNotRoot`]
/// a caller would see for a role it doesn't hold, not a generic auth
/// failure, so a legitimate integration can tell the two apart while
/// debugging.
pub(crate) async fn authenticate_game_root(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Uuid, AppError> {
    let (game_id, issuer_key) = authenticate_game_key(state, headers).await?;
    let now = OffsetDateTime::now_utc();
    if !issuer_key.is_valid_at(now) {
        return Err(AppError::GameKeyNotFound);
    }
    if !issuer_key.authorizes_key_changes() {
        return Err(AppError::IssuerKeyNotRoot);
    }
    Ok(game_id)
}

#[derive(Deserialize)]
pub struct AddIssuerKeyRequest {
    pub algorithm: String,
    /// Standard-base64-encoded raw public key bytes, same shape
    /// [`InitialKeyRequest`] uses at registration.
    pub public_key: String,
    /// `"root"` or `"operational"` — see `avalon_protocol::games::KeyRole`.
    pub role: String,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub valid_until: Option<OffsetDateTime>,
}

#[derive(Serialize)]
pub struct IssuerKeyResponse {
    pub key_id: Uuid,
    pub algorithm: String,
    pub role: String,
    #[serde(with = "time::serde::rfc3339")]
    pub valid_from: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub valid_until: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
}

/// `POST /games/{slug}/keys` (#84, implementing #80's decided two-tier key
/// model) — adds a new key to the issuer's key set. Requires the caller to
/// authenticate as the named `slug` with a currently-valid **root** key
/// ([`authenticate_game_root`]); an operational key, or a root key
/// belonging to a different game, is rejected. Emits `issuer.key_added`.
pub async fn add_issuer_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(body): Json<AddIssuerKeyRequest>,
) -> Result<Json<IssuerKeyResponse>, AppError> {
    let path_game_id = fetch_game_id_by_slug(&state, &slug).await?;
    let caller_game_id = authenticate_game_root(&state, &headers).await?;
    if caller_game_id != path_game_id {
        return Err(AppError::IssuerKeyForbidden);
    }

    if body.algorithm != SUPPORTED_KEY_ALGORITHM {
        return Err(AppError::InvalidGameKey);
    }
    let role = KeyRole::parse(&body.role).ok_or(AppError::InvalidIssuerKeyRole)?;
    let public_key_bytes = BASE64
        .decode(&body.public_key)
        .map_err(|_| AppError::InvalidGameKey)?;

    let key_id = Uuid::new_v4();
    let valid_from = OffsetDateTime::now_utc();

    let mut tx = state.pool.begin().await?;

    sqlx::query(
        "INSERT INTO issuer_keys (key_id, game_id, algorithm, public_key, role, valid_until, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(key_id)
    .bind(path_game_id)
    .bind(&body.algorithm)
    .bind(&public_key_bytes)
    .bind(role.as_str())
    .bind(body.valid_until)
    .bind(valid_from)
    .execute(&mut *tx)
    .await?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "issuer.key_added".to_string(),
        issuer: game_ref(&slug, "key_added"),
        subject: game_ref(&slug, "key_added"),
        payload: serde_json::json!({
            "game_id": path_game_id,
            "slug": slug,
            "key_id": key_id,
            "algorithm": body.algorithm,
            "public_key": BASE64.encode(&public_key_bytes),
            "role": role.as_str(),
            "valid_until": body.valid_until,
        }),
        timestamp: valid_from,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(IssuerKeyResponse {
        key_id,
        algorithm: body.algorithm,
        role: role.as_str().to_string(),
        valid_from,
        valid_until: body.valid_until,
        revoked_at: None,
    }))
}

#[derive(Deserialize)]
pub struct RevokeIssuerKeyRequest {
    pub reason: Option<String>,
}

/// `POST /games/{slug}/keys/{key_id}/revoke` (#84) — revokes a key in the
/// issuer's key set (root or operational; a root key can revoke itself, the
/// same "any key genuinely under your control" trust already implied by
/// authenticating as root at all). Same root-key-of-the-named-issuer
/// requirement as [`add_issuer_key`]. Revoking an already-revoked or
/// nonexistent key returns [`AppError::IssuerKeyForbidden`] rather than
/// silently succeeding — same posture `remove_friend`-style "no-op success"
/// endpoints elsewhere in this repo deliberately don't take, since a caller
/// retrying a revoke against a key it no longer controls is exactly the
/// kind of thing worth surfacing, not swallowing. Emits `issuer.key_revoked`.
pub async fn revoke_issuer_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((slug, key_id)): Path<(String, Uuid)>,
    Json(body): Json<RevokeIssuerKeyRequest>,
) -> Result<Json<IssuerKeyResponse>, AppError> {
    let path_game_id = fetch_game_id_by_slug(&state, &slug).await?;
    let caller_game_id = authenticate_game_root(&state, &headers).await?;
    if caller_game_id != path_game_id {
        return Err(AppError::IssuerKeyForbidden);
    }

    let revoked_at = OffsetDateTime::now_utc();
    let mut tx = state.pool.begin().await?;

    let row = sqlx::query(
        "UPDATE issuer_keys SET revoked_at = $1, revoked_reason = $2 \
         WHERE key_id = $3 AND game_id = $4 AND revoked_at IS NULL \
         RETURNING algorithm, role, created_at, valid_until",
    )
    .bind(revoked_at)
    .bind(&body.reason)
    .bind(key_id)
    .bind(path_game_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::IssuerKeyForbidden)?;

    let algorithm: String = row.try_get("algorithm")?;
    let role: String = row.try_get("role")?;
    let valid_from: OffsetDateTime = row.try_get("created_at")?;
    let valid_until: Option<OffsetDateTime> = row.try_get("valid_until")?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "issuer.key_revoked".to_string(),
        issuer: game_ref(&slug, "key_revoked"),
        subject: game_ref(&slug, "key_revoked"),
        payload: serde_json::json!({
            "game_id": path_game_id,
            "slug": slug,
            "key_id": key_id,
            "revoked_at": revoked_at,
            "reason": body.reason,
        }),
        timestamp: revoked_at,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(IssuerKeyResponse {
        key_id,
        algorithm,
        role,
        valid_from,
        valid_until,
        revoked_at: Some(revoked_at),
    }))
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

    // --- Issue #270: GET /games cursor pagination ---------------------
    //
    // Same SQL-string-based assertions `guilds::build_discover_query`'s own
    // tests use — no live Postgres needed, just checking the query shape
    // `QueryBuilder` produces.

    fn empty_list_games_query() -> ListGamesQuery {
        ListGamesQuery {
            q: None,
            sort: None,
            limit: None,
            cursor: None,
        }
    }

    #[test]
    fn sort_parse_defaults_to_newest_and_rejects_unknown_values() {
        assert!(matches!(
            GamesListSort::parse(None),
            Ok(GamesListSort::Newest)
        ));
        assert!(matches!(
            GamesListSort::parse(Some("newest")),
            Ok(GamesListSort::Newest)
        ));
        assert!(matches!(
            GamesListSort::parse(Some("name")),
            Ok(GamesListSort::Name)
        ));
        assert!(matches!(
            GamesListSort::parse(Some("most_players")),
            Err(AppError::InvalidGamesListQuery)
        ));
    }

    #[test]
    fn select_list_includes_only_public_fields() {
        let builder = build_games_list_query(&empty_list_games_query(), GamesListSort::Newest, 20);
        let sql = builder.sql();
        let sql = sql.as_str();
        assert!(sql.contains("id, slug, name, developer, registered_at, status, category"));
        assert!(sql.contains("FROM games"));
    }

    #[test]
    fn text_search_matches_name_slug_and_developer() {
        let mut query = empty_list_games_query();
        query.q = Some("ashen".to_string());
        let builder = build_games_list_query(&query, GamesListSort::Newest, 20);
        let sql = builder.sql();
        let sql = sql.as_str();
        assert!(sql.contains("name ILIKE"));
        assert!(sql.contains("slug ILIKE"));
        assert!(sql.contains("developer ILIKE"));
    }

    #[test]
    fn blank_search_term_is_dropped_rather_than_matching_everything() {
        let mut query = empty_list_games_query();
        query.q = Some("   ".to_string());
        let builder = build_games_list_query(&query, GamesListSort::Newest, 20);
        assert!(!builder.sql().as_str().contains("ILIKE"));
    }

    #[test]
    fn sort_selects_expected_order_by_clause() {
        let query = empty_list_games_query();

        let newest = build_games_list_query(&query, GamesListSort::Newest, 20);
        assert!(newest
            .sql()
            .as_str()
            .contains("ORDER BY registered_at DESC, id DESC"));

        let name = build_games_list_query(&query, GamesListSort::Name, 20);
        assert!(name.sql().as_str().contains("ORDER BY name ASC, id ASC"));
    }

    #[test]
    fn cursor_adds_keyset_pagination_clause_matching_the_active_sort() {
        let mut query = empty_list_games_query();
        query.cursor = Some(Uuid::new_v4());

        let newest = build_games_list_query(&query, GamesListSort::Newest, 20);
        assert!(newest
            .sql()
            .as_str()
            .contains("(registered_at, id) < (SELECT registered_at, id FROM games WHERE id ="));

        let name = build_games_list_query(&query, GamesListSort::Name, 20);
        assert!(name
            .sql()
            .as_str()
            .contains("(name, id) > (SELECT name, id FROM games WHERE id ="));
    }

    #[test]
    fn no_cursor_means_no_keyset_pagination_clause() {
        let query = empty_list_games_query();
        let builder = build_games_list_query(&query, GamesListSort::Newest, 20);
        assert!(!builder.sql().as_str().contains("WHERE id ="));
    }

    #[test]
    fn limit_fetches_one_extra_row_to_detect_a_next_page() {
        let builder = build_games_list_query(&empty_list_games_query(), GamesListSort::Newest, 20);
        // `push_bind` renders as a placeholder, not the literal value, so
        // this only confirms a LIMIT clause is present — the "+1" behavior
        // itself is exercised by the `#[ignore]`d integration test.
        assert!(builder.sql().as_str().contains(" LIMIT "));
    }
}
