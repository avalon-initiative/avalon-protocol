//! `AchievementDefinition` CRUD per game (issue #31) — a game defines its
//! achievements before it can issue them (#32).
//!
//! **Namespacing.** A definition's `GlobalId` is `game:<slug>:achievement:<key>`
//! (`crates/protocol/src/ids.rs`), minted by [`definition_ref`] the same way
//! `crates/server/src/games.rs`'s `game_ref` and `guilds.rs`'s `guild_ref`
//! namespace their own events — `key` matches `[a-z0-9_]+`
//! ([`validate_key`]), and the slug is always the caller's own, taken from
//! its registration (#26), never the caller's choice. `id` is immutable once
//! created; nothing in this module ever changes it.
//!
//! **Auth.** All three endpoints are game-credential-authenticated
//! (`crate::games::authenticate_game`, the challenge-response scheme #26
//! established), not a player session — defining an achievement is
//! something a game does about its own catalogue, not something a player
//! consents to. Unlike issuing (#32, gated behind the `achievements.issue`
//! capability grant), *defining* needs nothing beyond the game proving its
//! own identity: a game can always describe its own achievements, it just
//! can't issue one to a player without that player's consent. The write
//! endpoints additionally check that the authenticated game is the one
//! named by the `{slug}` path segment — a game authenticated as itself can
//! never create or change a definition under another game's slug
//! (`AppError::AchievementDefinitionForbidden`, 403). `GET
//! /games/{slug}/achievements` is public and unauthenticated, same
//! visibility level `games::get_game` and `guilds::get_guild` already use.
//!
//! **Durability.** `achievement_definitions` is a projection; `achievement.defined`,
//! `achievement.definition_updated`, and `achievement.definition_retired`
//! are the durable history, written into the outbox in the same transaction
//! as the row insert/update, same pattern `friends.rs`/`guilds.rs`/`games.rs`
//! already established for #71. `issuer` is `game:<slug>:self:<verb>`
//! (mirroring `game_ref`); `subject` is the definition's own `GlobalId` —
//! matching the event-kind catalogue's "game → achievement id" shape
//! (`docs/architecture/protocol-events.md`).
//!
//! **Update and retirement.** `PATCH /games/{slug}/achievements/{key}`
//! updates `name`/`description`/`schema` and bumps `version`, emitting
//! `achievement.definition_updated`; the id never changes. The same
//! endpoint also supports retiring a definition (`retired: true`) — no new
//! issuances against it (#32 enforces that), but existing attestations are
//! never touched and the row is never deleted, matching the ticket's "no
//! delete endpoint" design. Retiring emits `achievement.definition_retired`
//! instead of `.definition_updated` (a status change, not a definition
//! change) and does not bump `version`; retiring an already-retired
//! definition is a no-op that emits nothing, and a retired definition can
//! still have its name/description/schema edited in the same call.

use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::ids::GlobalId;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::AppError;
use crate::games::{authenticate_game, fetch_game_id_by_slug, game_ref};
use crate::outbox;
use crate::state::AppState;

/// `game:<slug>:achievement:<key>` — the definition's immutable, globally
/// unique id. `pub(crate)` so a future issuing endpoint (#32) can build the
/// same id to look up the definition an attestation points at, rather than
/// reimplementing this format.
pub(crate) fn definition_ref(slug: &str, key: &str) -> GlobalId {
    GlobalId::new("game", slug, "achievement", key)
}

/// Lowercase `[a-z0-9_]`, 2-128 characters — deliberately rejects rather
/// than normalizes an out-of-charset key, same posture
/// `games::validate_slug` documents for slugs.
fn validate_key(key: &str) -> Result<(), AppError> {
    let len = key.chars().count();
    if !(2..=128).contains(&len) {
        return Err(AppError::InvalidAchievementKey);
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(AppError::InvalidAchievementKey);
    }
    Ok(())
}

/// Authenticates the calling game and checks it is the one named by
/// `slug` — the shared guard both write endpoints use. Returns the path
/// slug's own `game_id` (already resolved, so callers don't do it twice).
async fn authenticate_owning_game(
    state: &AppState,
    headers: &HeaderMap,
    slug: &str,
) -> Result<Uuid, AppError> {
    let path_game_id = fetch_game_id_by_slug(state, slug).await?;
    let caller_game_id = authenticate_game(state, headers).await?;
    if caller_game_id != path_game_id {
        return Err(AppError::AchievementDefinitionForbidden);
    }
    Ok(path_game_id)
}

struct DefinitionRow {
    id: String,
    key: String,
    name: String,
    description: String,
    schema: Option<String>,
    version: i32,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    retired_at: Option<OffsetDateTime>,
}

async fn fetch_definition(
    state: &AppState,
    game_id: Uuid,
    key: &str,
) -> Result<DefinitionRow, AppError> {
    let row = sqlx::query(
        "SELECT id, key, name, description, schema, version, created_at, updated_at, retired_at \
         FROM achievement_definitions WHERE game_id = $1 AND key = $2",
    )
    .bind(game_id)
    .bind(key)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::AchievementDefinitionNotFound)?;
    Ok(DefinitionRow {
        id: row.try_get("id")?,
        key: row.try_get("key")?,
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        schema: row.try_get("schema")?,
        version: row.try_get("version")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        retired_at: row.try_get("retired_at")?,
    })
}

#[derive(Serialize)]
pub struct AchievementDefinitionResponse {
    pub id: String,
    pub game_id: Uuid,
    pub key: String,
    pub name: String,
    pub description: String,
    pub schema: Option<String>,
    pub version: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub retired: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub retired_at: Option<OffsetDateTime>,
}

fn definition_response(game_id: Uuid, row: DefinitionRow) -> AchievementDefinitionResponse {
    AchievementDefinitionResponse {
        id: row.id,
        game_id,
        key: row.key,
        name: row.name,
        description: row.description,
        schema: row.schema,
        version: row.version,
        created_at: row.created_at,
        updated_at: row.updated_at,
        retired: row.retired_at.is_some(),
        retired_at: row.retired_at,
    }
}

#[derive(Deserialize)]
pub struct CreateAchievementDefinitionRequest {
    pub key: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub schema: Option<GlobalId>,
}

/// `POST /games/{slug}/achievements` — create. 409 on a duplicate key for
/// this game; a different game defining the same key is a distinct id and
/// always succeeds (namespacing's whole point).
pub async fn create_achievement_definition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(body): Json<CreateAchievementDefinitionRequest>,
) -> Result<Json<AchievementDefinitionResponse>, AppError> {
    let game_id = authenticate_owning_game(&state, &headers, &slug).await?;
    validate_key(&body.key)?;

    let id = definition_ref(&slug, &body.key);
    let schema_str = body.schema.as_ref().map(GlobalId::as_str);
    let now = OffsetDateTime::now_utc();
    const INITIAL_VERSION: i32 = 1;

    let mut tx = state.pool.begin().await?;

    let inserted = sqlx::query(
        "INSERT INTO achievement_definitions \
         (id, game_id, key, name, description, schema, version, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)",
    )
    .bind(id.as_str())
    .bind(game_id)
    .bind(&body.key)
    .bind(&body.name)
    .bind(&body.description)
    .bind(schema_str)
    .bind(INITIAL_VERSION)
    .bind(now)
    .execute(&mut *tx)
    .await;
    if let Err(sqlx::Error::Database(db_err)) = &inserted {
        if db_err.is_unique_violation() {
            return Err(AppError::AchievementKeyTaken);
        }
    }
    inserted?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "achievement.defined".to_string(),
        issuer: game_ref(&slug, "achievement_defined"),
        subject: id.clone(),
        payload: serde_json::json!({
            "id": id.as_str(),
            "game_id": game_id,
            "slug": slug,
            "key": body.key,
            "name": body.name,
            "description": body.description,
            "schema": schema_str,
            "version": INITIAL_VERSION,
        }),
        timestamp: now,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(definition_response(
        game_id,
        DefinitionRow {
            id: id.as_str().to_string(),
            key: body.key,
            name: body.name,
            description: body.description,
            schema: schema_str.map(str::to_string),
            version: INITIAL_VERSION,
            created_at: now,
            updated_at: now,
            retired_at: None,
        },
    )))
}

#[derive(Deserialize)]
pub struct UpdateAchievementDefinitionRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub schema: Option<GlobalId>,
    /// Set to `true` to retire the definition (see module doc comment).
    /// Never used to un-retire — retirement is one-way.
    #[serde(default)]
    pub retired: Option<bool>,
}

/// `PATCH /games/{slug}/achievements/{key}` — updates name/description/schema
/// (bumping `version`) and/or retires the definition. The id never changes.
pub async fn update_achievement_definition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((slug, key)): Path<(String, String)>,
    Json(body): Json<UpdateAchievementDefinitionRequest>,
) -> Result<Json<AchievementDefinitionResponse>, AppError> {
    let game_id = authenticate_owning_game(&state, &headers, &slug).await?;
    let existing = fetch_definition(&state, game_id, &key).await?;

    let new_name = body.name.clone().unwrap_or_else(|| existing.name.clone());
    let new_description = body
        .description
        .clone()
        .unwrap_or_else(|| existing.description.clone());
    let new_schema = body
        .schema
        .as_ref()
        .map(|s| s.as_str().to_string())
        .or_else(|| existing.schema.clone());
    let definition_changed =
        body.name.is_some() || body.description.is_some() || body.schema.is_some();
    let now_retiring = body.retired == Some(true) && existing.retired_at.is_none();

    let now = OffsetDateTime::now_utc();
    let new_version = if definition_changed {
        existing.version + 1
    } else {
        existing.version
    };
    let new_retired_at = if now_retiring {
        Some(now)
    } else {
        existing.retired_at
    };

    let mut tx = state.pool.begin().await?;

    sqlx::query(
        "UPDATE achievement_definitions \
         SET name = $3, description = $4, schema = $5, version = $6, updated_at = $7, retired_at = $8 \
         WHERE game_id = $1 AND key = $2",
    )
    .bind(game_id)
    .bind(&key)
    .bind(&new_name)
    .bind(&new_description)
    .bind(&new_schema)
    .bind(new_version)
    .bind(now)
    .bind(new_retired_at)
    .execute(&mut *tx)
    .await?;

    if definition_changed {
        let event = ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "achievement.definition_updated".to_string(),
            issuer: game_ref(&slug, "achievement_definition_updated"),
            subject: GlobalId::new("game", &slug, "achievement", &key),
            payload: serde_json::json!({
                "id": existing.id,
                "game_id": game_id,
                "slug": slug,
                "key": key,
                "name": new_name,
                "description": new_description,
                "schema": new_schema,
                "version": new_version,
            }),
            timestamp: now,
            version: 1,
        };
        outbox::enqueue(&mut tx, &event).await?;
    }

    if now_retiring {
        let event = ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "achievement.definition_retired".to_string(),
            issuer: game_ref(&slug, "achievement_definition_retired"),
            subject: GlobalId::new("game", &slug, "achievement", &key),
            payload: serde_json::json!({
                "id": existing.id,
                "game_id": game_id,
                "slug": slug,
                "key": key,
            }),
            timestamp: now,
            version: 1,
        };
        outbox::enqueue(&mut tx, &event).await?;
    }

    tx.commit().await?;

    Ok(Json(definition_response(
        game_id,
        DefinitionRow {
            id: existing.id,
            key,
            name: new_name,
            description: new_description,
            schema: new_schema,
            version: new_version,
            created_at: existing.created_at,
            updated_at: now,
            retired_at: new_retired_at,
        },
    )))
}

/// `GET /games/{slug}/achievements` — public listing (feeds the registry,
/// #89). No auth required, same visibility level `games::get_game` and
/// `guilds::get_guild` already use. Includes retired definitions (marked
/// `retired: true`) rather than hiding them — a retired definition's past
/// attestations are still real and still need somewhere to point.
pub async fn list_achievement_definitions(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<Vec<AchievementDefinitionResponse>>, AppError> {
    let game_id = fetch_game_id_by_slug(&state, &slug).await?;

    let rows = sqlx::query(
        "SELECT id, key, name, description, schema, version, created_at, updated_at, retired_at \
         FROM achievement_definitions WHERE game_id = $1 ORDER BY created_at",
    )
    .bind(game_id)
    .fetch_all(&state.pool)
    .await?;

    let mut definitions = Vec::with_capacity(rows.len());
    for row in rows {
        definitions.push(definition_response(
            game_id,
            DefinitionRow {
                id: row.try_get("id")?,
                key: row.try_get("key")?,
                name: row.try_get("name")?,
                description: row.try_get("description")?,
                schema: row.try_get("schema")?,
                version: row.try_get("version")?,
                created_at: row.try_get("created_at")?,
                updated_at: row.try_get("updated_at")?,
                retired_at: row.try_get("retired_at")?,
            },
        ));
    }
    Ok(Json(definitions))
}

/// A pure, DB-free projection of a definition's current state from its own
/// event history — proves `achievement_definitions` is genuinely derived
/// from `achievement.defined`/`.definition_updated`/`.definition_retired`
/// rather than a second source of truth (issue #31's "rebuild" test,
/// mirroring `avalon_indexer::Indexer::rebuild`'s fold-over-events shape,
/// since no concrete per-domain projection exists yet in the still-stub
/// `indexer` crate for this or any other table). `pub(crate)` for this
/// module's own tests.
// Only exercised by this module's own tests today (there is no live caller
// yet — no concrete indexer projection exists in this repo for anything,
// see the doc comment above), same `#![allow(dead_code)]` posture
// `crate::authz` documents for its own not-yet-wired-up infrastructure.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RebuiltDefinition {
    pub name: String,
    pub description: String,
    pub schema: Option<String>,
    pub version: u32,
    pub retired: bool,
}

#[allow(dead_code)]
pub(crate) fn rebuild_definition(events: &[ProtocolEvent]) -> Option<RebuiltDefinition> {
    let mut state: Option<RebuiltDefinition> = None;
    for event in events {
        match event.kind.as_str() {
            "achievement.defined" => {
                state = Some(RebuiltDefinition {
                    name: event.payload["name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    description: event.payload["description"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    schema: event.payload["schema"].as_str().map(str::to_string),
                    version: event.payload["version"].as_u64().unwrap_or(1) as u32,
                    retired: false,
                });
            }
            "achievement.definition_updated" => {
                if let Some(def) = state.as_mut() {
                    def.name = event.payload["name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    def.description = event.payload["description"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    def.schema = event.payload["schema"].as_str().map(str::to_string);
                    def.version = event.payload["version"].as_u64().unwrap_or_default() as u32;
                }
            }
            "achievement.definition_retired" => {
                if let Some(def) = state.as_mut() {
                    def.retired = true;
                }
            }
            _ => {}
        }
    }
    state
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — pure-logic checks only. The
    //! endpoint-level flows (create, duplicate-key 409, cross-slug 403,
    //! update bumps version, list) are covered by
    //! `crates/server/tests/achievements.rs`, gated `--ignored`.

    use super::*;

    #[test]
    fn validate_key_accepts_lowercase_alphanumeric_and_underscore() {
        assert!(validate_key("dragon_slayer").is_ok());
        assert!(validate_key("ab").is_ok());
        assert!(validate_key("a1_2").is_ok());
    }

    #[test]
    fn validate_key_rejects_uppercase_and_hyphens() {
        assert!(validate_key("Dragon_Slayer").is_err());
        assert!(validate_key("dragon-slayer").is_err());
        assert!(validate_key("dragon slayer").is_err());
    }

    #[test]
    fn validate_key_rejects_too_short_or_too_long() {
        assert!(validate_key("a").is_err());
        assert!(validate_key(&"a".repeat(129)).is_err());
        assert!(validate_key(&"a".repeat(128)).is_ok());
    }

    #[test]
    fn definition_ref_namespaces_by_slug_and_key() {
        let id = definition_ref("ashen-realms", "dragon_slayer");
        assert_eq!(id.as_str(), "game:ashen-realms:achievement:dragon_slayer");
    }

    #[test]
    fn two_games_defining_the_same_key_produce_distinct_ids() {
        let a = definition_ref("ashen-realms", "dragon_slayer");
        let b = definition_ref("worldzero", "dragon_slayer");
        assert_ne!(a, b);
    }

    fn defined_event(
        name: &str,
        description: &str,
        schema: Option<&str>,
        version: u32,
    ) -> ProtocolEvent {
        ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "achievement.defined".to_string(),
            issuer: game_ref("ashen-realms", "achievement_defined"),
            subject: definition_ref("ashen-realms", "dragon_slayer"),
            payload: serde_json::json!({
                "name": name,
                "description": description,
                "schema": schema,
                "version": version,
            }),
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }
    }

    fn updated_event(
        name: &str,
        description: &str,
        schema: Option<&str>,
        version: u32,
    ) -> ProtocolEvent {
        let mut event = defined_event(name, description, schema, version);
        event.kind = "achievement.definition_updated".to_string();
        event
    }

    fn retired_event() -> ProtocolEvent {
        ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "achievement.definition_retired".to_string(),
            issuer: game_ref("ashen-realms", "achievement_definition_retired"),
            subject: definition_ref("ashen-realms", "dragon_slayer"),
            payload: serde_json::json!({}),
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }
    }

    #[test]
    fn rebuild_from_only_a_defined_event_matches_creation() {
        let events = vec![defined_event("Dragon Slayer", "Slew the dragon", None, 1)];
        let rebuilt = rebuild_definition(&events).unwrap();
        assert_eq!(rebuilt.name, "Dragon Slayer");
        assert_eq!(rebuilt.description, "Slew the dragon");
        assert_eq!(rebuilt.schema, None);
        assert_eq!(rebuilt.version, 1);
        assert!(!rebuilt.retired);
    }

    #[test]
    fn rebuild_applies_updates_in_order_and_bumps_version() {
        let events = vec![
            defined_event("Dragon Slayer", "Slew the dragon", None, 1),
            updated_event(
                "Dragon Slayer",
                "Slew the Dragon Lord",
                Some("game:ashen-realms:achievement:schema:v1"),
                2,
            ),
        ];
        let rebuilt = rebuild_definition(&events).unwrap();
        assert_eq!(rebuilt.description, "Slew the Dragon Lord");
        assert_eq!(
            rebuilt.schema.as_deref(),
            Some("game:ashen-realms:achievement:schema:v1")
        );
        assert_eq!(rebuilt.version, 2);
    }

    #[test]
    fn rebuild_reflects_retirement_without_touching_other_fields() {
        let events = vec![
            defined_event("Dragon Slayer", "Slew the dragon", None, 1),
            retired_event(),
        ];
        let rebuilt = rebuild_definition(&events).unwrap();
        assert!(rebuilt.retired);
        assert_eq!(rebuilt.name, "Dragon Slayer");
        assert_eq!(rebuilt.version, 1);
    }

    #[test]
    fn rebuild_with_no_events_yields_nothing() {
        assert!(rebuild_definition(&[]).is_none());
    }
}
