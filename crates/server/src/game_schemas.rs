//! Game Space schema publication (issue #255, closing the decision made in
//! #181) — a game publishing how its own data is structured, versioned and
//! immutable once published. See `docs/architecture/game-space.md`.
//!
//! **Namespacing.** A version's `GlobalId` is `game:<slug>:schema:<version>`
//! (`crates/protocol/src/ids.rs`), minted by [`schema_ref`] — the same
//! per-module `game_ref`/`definition_ref` precedent `games.rs` and
//! `achievements.rs` already established, applied here to schema versions.
//!
//! **Auth — the exact `achievements.rs` pattern, reused rather than
//! reinvented.** Publishing a schema is something a game does about its own
//! catalogue, not something that touches player data — the same reasoning
//! `achievements.rs`'s module doc comment gives for why *defining* an
//! achievement needs nothing beyond the game proving its own identity
//! (`games::authenticate_game`'s challenge-response scheme), never a
//! player-granted capability. That's why this module does **not** use
//! `crate::authz::require_capability` — that guard exists specifically for
//! a game acting *on behalf of a player* (e.g. `presence::update_game_presence`,
//! gated on a capability the player granted); nothing here reads or writes
//! anything belonging to a player at all. [`authenticate_owning_game`]
//! mirrors `achievements.rs`'s function of the same name: resolve the
//! `{slug}` path segment's own game id, authenticate the caller via
//! `games::authenticate_game`, and 403
//! (`AppError::GameSchemaForbidden`) unless they match — so a game
//! authenticated as itself can never publish a schema attributed to
//! another game's id, structurally (the id used for every insert below is
//! the *authenticated* game id, never anything read from the request body).
//!
//! **Immutability + lineage.** `POST /games/{slug}/schemas` always inserts
//! a new row; there is no update/PATCH endpoint for `proto_source`, full
//! stop. `version` is one more than the game's current maximum (1 for a
//! game's first publication). When there is a prior version, its
//! `superseded_by` is set to the new version's id in the same transaction
//! — the one, documented exception to "never edit a published row" (see
//! `crates/protocol/src/game_schemas.rs`'s module doc comment): lineage
//! metadata, not the published text itself. Both facts (the new row, and
//! the prior row's new `superseded_by`) are captured by one
//! `game_schema.published` event, so the indexer's projection
//! (`crates/indexer/src/projections/game_schemas.rs`) can derive both
//! writes by replaying that single event.
//!
//! **Durability.** Same outbox pattern (#71) every other write endpoint in
//! this crate uses: the row insert(s) and the event enqueue happen in one
//! transaction via `crate::outbox`.
//!
//! **Reads.** `GET /games/{slug}/schemas` (list, oldest first) and `GET
//! /games/{slug}/schemas/{version}` (one version) are public and
//! unauthenticated, same visibility level `games::get_game` and
//! `achievements::list_achievement_definitions` already use — nothing
//! about a published schema is sensitive.

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

/// `game:<slug>:schema:<version>` — a published version's immutable,
/// globally unique id. `pub(crate)` so this module's own tests (and any
/// future caller) can build the same id rather than reimplementing the
/// format.
pub(crate) fn schema_ref(slug: &str, version: u32) -> GlobalId {
    GlobalId::new("game", slug, "schema", &version.to_string())
}

/// Authenticates the calling game and checks it is the one named by
/// `slug` — see this module's doc comment for why this exists instead of
/// `crate::authz::require_capability`. Returns the path slug's own
/// `game_id` (already resolved, so callers don't do it twice).
async fn authenticate_owning_game(
    state: &AppState,
    headers: &HeaderMap,
    slug: &str,
) -> Result<Uuid, AppError> {
    let path_game_id = fetch_game_id_by_slug(state, slug).await?;
    let caller_game_id = authenticate_game(state, headers).await?;
    if caller_game_id != path_game_id {
        return Err(AppError::GameSchemaForbidden);
    }
    Ok(path_game_id)
}

struct SchemaVersionRow {
    id: String,
    version: i32,
    proto_source: String,
    published_at: OffsetDateTime,
    superseded_by: Option<String>,
}

#[derive(Serialize)]
pub struct GameSchemaVersionResponse {
    pub id: String,
    pub game_id: Uuid,
    pub version: u32,
    pub proto_source: String,
    #[serde(with = "time::serde::rfc3339")]
    pub published_at: OffsetDateTime,
    pub superseded_by: Option<String>,
}

fn version_response(game_id: Uuid, row: SchemaVersionRow) -> GameSchemaVersionResponse {
    GameSchemaVersionResponse {
        id: row.id,
        game_id,
        version: row.version as u32,
        proto_source: row.proto_source,
        published_at: row.published_at,
        superseded_by: row.superseded_by,
    }
}

#[derive(Deserialize)]
pub struct PublishGameSchemaVersionRequest {
    /// Raw `.proto` source text, stored opaque — never parsed or compiled
    /// here (#181's decision; out of scope for #255).
    pub proto_source: String,
}

/// `POST /games/{slug}/schemas` — publish the next version. Always an
/// insert, never an update to an existing row (see module doc comment).
pub async fn publish_schema_version(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(body): Json<PublishGameSchemaVersionRequest>,
) -> Result<Json<GameSchemaVersionResponse>, AppError> {
    let game_id = authenticate_owning_game(&state, &headers, &slug).await?;
    if body.proto_source.trim().is_empty() {
        return Err(AppError::InvalidGameSchema);
    }

    let mut tx = state.pool.begin().await?;

    // Lock the game's own row for the duration of this transaction so two
    // concurrent publishes for the same game serialize instead of racing:
    // without this, both transactions can read the same `MAX(version)`
    // under READ COMMITTED, compute the same `new_version`, and have one
    // lose to the `UNIQUE (game_id, version)` constraint with a raw,
    // unhandled `AppError::Database` 500. `FOR UPDATE` makes the second
    // transaction block here until the first commits (or rolls back), at
    // which point it re-reads `MAX(version)` and correctly computes the
    // next one.
    sqlx::query("SELECT id FROM games WHERE id = $1 FOR UPDATE")
        .bind(game_id)
        .fetch_one(&mut *tx)
        .await?;

    let current_max: Option<i32> =
        sqlx::query("SELECT MAX(version) AS max_version FROM game_schemas WHERE game_id = $1")
            .bind(game_id)
            .fetch_one(&mut *tx)
            .await?
            .try_get("max_version")?;
    let previous_version = current_max.map(|v| v as u32);
    let new_version = previous_version.map(|v| v + 1).unwrap_or(1);

    let id = schema_ref(&slug, new_version);
    let now = OffsetDateTime::now_utc();
    let previous_id = previous_version.map(|v| schema_ref(&slug, v));

    sqlx::query(
        "INSERT INTO game_schemas (id, game_id, version, proto_source, published_at, superseded_by) \
         VALUES ($1, $2, $3, $4, $5, NULL)",
    )
    .bind(id.as_str())
    .bind(game_id)
    .bind(new_version as i32)
    .bind(&body.proto_source)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    if let Some(previous_id) = &previous_id {
        sqlx::query("UPDATE game_schemas SET superseded_by = $2 WHERE id = $1")
            .bind(previous_id.as_str())
            .bind(id.as_str())
            .execute(&mut *tx)
            .await?;
    }

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "game_schema.published".to_string(),
        issuer: game_ref(&slug, "schema_published"),
        subject: id.clone(),
        payload: serde_json::json!({
            "id": id.as_str(),
            "game_id": game_id,
            "slug": slug,
            "version": new_version,
            "proto_source": body.proto_source,
            "supersedes": previous_id.as_ref().map(GlobalId::as_str),
        }),
        timestamp: now,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(version_response(
        game_id,
        SchemaVersionRow {
            id: id.as_str().to_string(),
            version: new_version as i32,
            proto_source: body.proto_source,
            published_at: now,
            superseded_by: None,
        },
    )))
}

/// `GET /games/{slug}/schemas` — every published version for this game,
/// oldest first. Public, unauthenticated (see module doc comment). Empty
/// for a game that has never published.
pub async fn list_schema_versions(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<Vec<GameSchemaVersionResponse>>, AppError> {
    let game_id = fetch_game_id_by_slug(&state, &slug).await?;

    let rows = sqlx::query(
        "SELECT id, version, proto_source, published_at, superseded_by \
         FROM game_schemas WHERE game_id = $1 ORDER BY version",
    )
    .bind(game_id)
    .fetch_all(&state.pool)
    .await?;

    let mut versions = Vec::with_capacity(rows.len());
    for row in rows {
        versions.push(version_response(
            game_id,
            SchemaVersionRow {
                id: row.try_get("id")?,
                version: row.try_get("version")?,
                proto_source: row.try_get("proto_source")?,
                published_at: row.try_get("published_at")?,
                superseded_by: row.try_get("superseded_by")?,
            },
        ));
    }
    Ok(Json(versions))
}

/// `GET /games/{slug}/schemas/{version}` — one published version, verbatim.
/// Public, unauthenticated. This is the endpoint a round-trip fetch of a
/// just-published version calls to prove the stored `proto_source` matches
/// what was submitted exactly.
pub async fn get_schema_version(
    State(state): State<AppState>,
    Path((slug, version)): Path<(String, u32)>,
) -> Result<Json<GameSchemaVersionResponse>, AppError> {
    let game_id = fetch_game_id_by_slug(&state, &slug).await?;

    let row = sqlx::query(
        "SELECT id, version, proto_source, published_at, superseded_by \
         FROM game_schemas WHERE game_id = $1 AND version = $2",
    )
    .bind(game_id)
    .bind(version as i32)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::GameSchemaNotFound)?;

    Ok(Json(version_response(
        game_id,
        SchemaVersionRow {
            id: row.try_get("id")?,
            version: row.try_get("version")?,
            proto_source: row.try_get("proto_source")?,
            published_at: row.try_get("published_at")?,
            superseded_by: row.try_get("superseded_by")?,
        },
    )))
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — pure-logic checks only. The
    //! endpoint-level flows (publish, round-trip fetch, immutability
    //! across a second version, cross-slug 403) are covered by
    //! `crates/server/tests/game_schemas.rs`, gated `--ignored`.

    use super::*;

    #[test]
    fn schema_ref_namespaces_by_slug_and_version() {
        let id = schema_ref("ashen-realms", 1);
        assert_eq!(id.as_str(), "game:ashen-realms:schema:1");
    }

    #[test]
    fn two_games_publishing_produce_distinct_ids_for_the_same_version_number() {
        let a = schema_ref("ashen-realms", 1);
        let b = schema_ref("worldzero", 1);
        assert_ne!(a, b);
    }

    #[test]
    fn successive_versions_for_the_same_game_produce_distinct_ids() {
        let v1 = schema_ref("ashen-realms", 1);
        let v2 = schema_ref("ashen-realms", 2);
        assert_ne!(v1, v2);
    }
}
