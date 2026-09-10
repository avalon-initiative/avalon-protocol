//! `GET /games/{slug}/registry` — the Game Registry's derived-metrics read
//! surface (issue #261, the first concrete slice of the epic-sized #89):
//! four durable-derived facts about a game/issuer, each carrying its own
//! definition and class label, per `docs/architecture/game-registry.md`.
//! No composite score, no ranking — see that doc's "statistics inform
//! trust; they do not determine it."
//!
//! A separate endpoint rather than folding these fields into `GET
//! /games/{slug}` (`games::get_game`): the two read models change for
//! different reasons (registration facts are edited rarely and directly;
//! derived metrics move with every binding/attestation event elsewhere in
//! the system) and want different response shapes — a metrics response is
//! `{ value, definition, class }` per field, which would sit awkwardly
//! flattened alongside `GamePublicResponse`'s plain fields and risks a
//! bare, unlabeled number sneaking in later. Namespacing them under their
//! own path keeps the class-label requirement structurally impossible to
//! skip.
//!
//! Public and unauthenticated, same visibility level `games::get_game`
//! already uses: aggregates only, never per-player data
//! (`docs/architecture/game-registry.md`'s privacy invariant), so nothing
//! returned here is sensitive. A game with no binding/achievement activity
//! at all returns zeros for every metric, not an error — same "absence
//! means nothing happened yet" posture the rest of this crate's read
//! endpoints use (`AppError::GameNotFound` is still returned for a slug
//! that doesn't exist at all, same as `games::get_game`).

use avalon_indexer::registry::{compute_for_game, Metric};
use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;

use crate::error::AppError;
use crate::games::fetch_game_id_by_slug;
use crate::state::AppState;

#[derive(Serialize)]
pub struct MetricResponse {
    pub value: i64,
    pub definition: &'static str,
    pub class: &'static str,
}

impl From<Metric> for MetricResponse {
    fn from(metric: Metric) -> Self {
        Self {
            value: metric.value,
            definition: metric.definition,
            class: metric.class,
        }
    }
}

#[derive(Serialize)]
pub struct GameRegistryResponse {
    pub players: MetricResponse,
    pub total_players_ever: MetricResponse,
    pub achievements_issued: MetricResponse,
    pub achievements_revoked: MetricResponse,
    pub unique_achievement_holders: MetricResponse,
}

pub async fn get_game_registry(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<GameRegistryResponse>, AppError> {
    let game_id = fetch_game_id_by_slug(&state, &slug).await?;
    // Matches `projections::attestations`'s own fixture convention for the
    // `issuer` field ahead of achievement issuing (Epic #30) actually
    // landing — see `avalon_indexer::registry::compute_for_game`'s doc
    // comment.
    let issuer = format!("game:{slug}");

    let metrics = compute_for_game(&state.pool, game_id, &issuer).await?;

    Ok(Json(GameRegistryResponse {
        players: metrics.players.into(),
        total_players_ever: metrics.total_players_ever.into(),
        achievements_issued: metrics.achievements_issued.into(),
        achievements_revoked: metrics.achievements_revoked.into(),
        unique_achievement_holders: metrics.unique_achievement_holders.into(),
    }))
}
