//! `GET /integrations/{slug}/registry` (and, per issue #95, the identical
//! `GET /registry/{slug}` under a dedicated top-level namespace for
//! anything that isn't the Hub) — the Integrator Registry's derived-metrics
//! read surface (issue #261, the first concrete slice of the epic-sized
//! #89): four durable-derived facts about an integrator/issuer, each
//! carrying its own definition and class label, per
//! `docs/architecture/registry.md`. No composite score, no ranking — see
//! that doc's "statistics inform trust; they do not determine it."
//!
//! A separate endpoint rather than folding these fields into `GET
//! /integrations/{slug}` (`integrators::get_integrator`): the two read models change for
//! different reasons (registration facts are edited rarely and directly;
//! derived metrics move with every binding/attestation event elsewhere in
//! the system) and want different response shapes — a metrics response is
//! `{ value, definition, class }` per field, which would sit awkwardly
//! flattened alongside `IntegratorPublicResponse`'s plain fields and risks a
//! bare, unlabeled number sneaking in later. Namespacing them under their
//! own path keeps the class-label requirement structurally impossible to
//! skip.
//!
//! Public and unauthenticated, same visibility level `integrators::get_integrator`
//! already uses: aggregates only, never per-player data
//! (`docs/architecture/registry.md`'s privacy invariant), so nothing
//! returned here is sensitive. An integrator with no binding/achievement activity
//! at all returns zeros for every metric, not an error — same "absence
//! means nothing happened yet" posture the rest of this crate's read
//! endpoints use (`AppError::IntegratorNotFound` is still returned for a slug
//! that doesn't exist at all, same as `integrators::get_integrator`).
//!
//! **Minimum cohort size (issue #96).** A raw count under
//! `avalon_indexer::registry`'s configured floor is coarsened to the floor
//! itself with `exact: false` before it ever reaches this handler — see
//! that module's own doc comment for the enforcement point. This handler
//! only ever forwards `Metric`'s already-coarsened fields; it never sees
//! or could accidentally leak the real sub-floor count.

use avalon_indexer::registry::{compute_for_integrator, Metric};
use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;

use crate::error::AppError;
use crate::integrators::fetch_integrator_id_by_slug;
use crate::state::AppState;

#[derive(Serialize)]
pub struct MetricResponse {
    pub value: i64,
    pub definition: &'static str,
    pub class: &'static str,
    /// Issue #96's minimum-cohort-size floor: `false` means `value` is the
    /// configured floor, not the real count — the real count is only known
    /// to be somewhere in `1..value`. A caller must render this as "fewer
    /// than `value`", never as an exact number, whenever `exact` is
    /// `false`.
    pub exact: bool,
}

impl From<Metric> for MetricResponse {
    fn from(metric: Metric) -> Self {
        Self {
            value: metric.value,
            definition: metric.definition,
            class: metric.class,
            exact: metric.exact,
        }
    }
}

#[derive(Serialize)]
pub struct IntegratorRegistryResponse {
    pub players: MetricResponse,
    pub total_players_ever: MetricResponse,
    pub achievements_issued: MetricResponse,
    pub achievements_revoked: MetricResponse,
    pub unique_achievement_holders: MetricResponse,
}

pub async fn get_integrator_registry(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<IntegratorRegistryResponse>, AppError> {
    let integrator_id = fetch_integrator_id_by_slug(&state, &slug).await?;
    // Matches `projections::attestations`'s own fixture convention for the
    // `issuer` field ahead of achievement issuing (Epic #30) actually
    // landing — see `avalon_indexer::registry::compute_for_integrator`'s doc
    // comment.
    let issuer = format!("game:{slug}");

    let metrics = compute_for_integrator(&state.pool, integrator_id, &issuer).await?;

    Ok(Json(IntegratorRegistryResponse {
        players: metrics.players.into(),
        total_players_ever: metrics.total_players_ever.into(),
        achievements_issued: metrics.achievements_issued.into(),
        achievements_revoked: metrics.achievements_revoked.into(),
        unique_achievement_holders: metrics.unique_achievement_holders.into(),
    }))
}
