//! The Integrator Registry's derived-metrics read model. See
//! `docs/projects/backend-server/architecture/registry.md`'s "Today in the repo" for the metric
//! definitions, why nothing is ranked/combined, and the `coarsen`/
//! `min_cohort` privacy floor enforced centrally here.

use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::projections::{attestations, integrator_bindings};
use crate::IndexError;

pub const CLASS_DURABLE_DERIVED: &str = "durable-derived";

/// `AVALON_REGISTRY_MIN_COHORT`, otherwise [`DEFAULT_MIN_COHORT`]. Any
/// non-positive or unparseable value falls back to the default rather than
/// disabling the floor — same fallback posture
/// `guild_messages::message_cap`/`archive_retention_days` already
/// establish for this repo's other "env-configurable, never silently
/// zero/off" settings.
pub fn min_cohort() -> i64 {
    std::env::var("AVALON_REGISTRY_MIN_COHORT")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MIN_COHORT)
}

/// Chosen as a round, documented number, not a silently-picked magic
/// constant — small enough that a legitimate small/indie integrator's registry
/// entry isn't withheld outright, large enough that "fewer than 5" doesn't
/// narrow a cohort down to one identifiable person. Revisit with real data
/// once the registry has real external traffic.
pub const DEFAULT_MIN_COHORT: i64 = 5;

/// One labeled fact: a value, its precise definition, and the class of
/// evidence it came from. Never serialized as a bare number.
///
/// `exact`: `true` means `value` is the real count; `false`
/// means `value` is the configured floor and the real count is only known
/// to be somewhere in `1..floor` — a consumer should render this as
/// "fewer than `value`", never as if it were precise.
#[derive(Debug, Clone, Serialize)]
pub struct Metric {
    pub value: i64,
    pub definition: &'static str,
    pub class: &'static str,
    pub exact: bool,
}

impl Metric {
    fn durable_derived(raw_value: i64, definition: &'static str, floor: i64) -> Self {
        let (value, exact) = coarsen(raw_value, floor);
        Self {
            value,
            definition,
            class: CLASS_DURABLE_DERIVED,
            exact,
        }
    }
}

/// The cohort-size floor itself, pulled out of [`Metric`] so it
/// can be unit tested directly against the boundary cases
/// (zero, just under the floor, exactly at the floor, well above it)
/// without touching Postgres. `0` always stays exact — see this module's
/// own doc comment for why "nobody" isn't a privacy concern.
fn coarsen(raw_value: i64, floor: i64) -> (i64, bool) {
    if raw_value == 0 || raw_value >= floor {
        (raw_value, true)
    } else {
        (floor, false)
    }
}

/// The four #261 metrics for one integrator/issuer, each independently labeled.
#[derive(Debug, Clone, Serialize)]
pub struct IntegratorRegistryMetrics {
    pub players: Metric,
    pub total_players_ever: Metric,
    pub achievements_issued: Metric,
    pub achievements_revoked: Metric,
    pub unique_achievement_holders: Metric,
}

/// Computes all four metrics for `integrator_id`. `issuer` is the string form
/// achievement events attribute to this integrator — `format!("game:{slug}")`,
/// matching the convention `projections::attestations`'s own fixtures
/// already established for the `issuer` field
/// (`avalon_protocol::achievements::Issuer::Game` serialized as a plain
/// `game:<slug>` string, ahead of achievement issuing — Epic #30 — actually
/// landing).
///
/// Reads the projection tables directly (SQL aggregates), never replays
/// events in memory — this is the request-time path; the pure
/// `fold`/`count_*` helpers in `projections::integrator_bindings` and
/// `projections::attestations` exist so the same arithmetic can be proven
/// against fixture events without Postgres (see their test modules).
///
/// Never errors for an integrator with no activity — every count is `0`, not a
/// missing field or a distinguished error, matching this crate's "absence
/// means nothing happened yet" posture (`docs/projects/backend-server/architecture/query-and-indexing.md`).
pub async fn compute_for_integrator(
    pool: &PgPool,
    integrator_id: Uuid,
    issuer: &str,
) -> Result<IntegratorRegistryMetrics, IndexError> {
    let players = integrator_bindings::active_player_count(pool, integrator_id).await?;
    let total_players_ever = integrator_bindings::total_players_ever(pool, integrator_id).await?;
    let achievements_issued = attestations::issued_count(pool, issuer).await?;
    let achievements_revoked = attestations::revoked_count(pool, issuer).await?;
    let unique_achievement_holders = attestations::unique_holder_count(pool, issuer).await?;

    // One floor read per call, applied uniformly to every metric below —
    // issue #96's invariant that the enforcement point is single and
    // consumer-agnostic, not one check per metric with its own threshold.
    let floor = min_cohort();

    Ok(IntegratorRegistryMetrics {
        players: Metric::durable_derived(
            players,
            "distinct identities with an active IntegratorBinding",
            floor,
        ),
        total_players_ever: Metric::durable_derived(
            total_players_ever,
            "distinct identities that ever had a binding",
            floor,
        ),
        achievements_issued: Metric::durable_derived(
            achievements_issued,
            "count of achievement.issued events by this issuer",
            floor,
        ),
        achievements_revoked: Metric::durable_derived(
            achievements_revoked,
            "count of achievement.revoked events by this issuer",
            floor,
        ),
        unique_achievement_holders: Metric::durable_derived(
            unique_achievement_holders,
            "distinct subjects with at least one valid attestation from this issuer",
            floor,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every metric carries a non-empty definition and the
    /// `durable-derived` class label — the ticket's hard requirement that
    /// no value ships bare.
    #[test]
    fn every_metric_carries_a_definition_and_the_durable_derived_class() {
        let metric = Metric::durable_derived(30, "some definition", DEFAULT_MIN_COHORT);
        assert_eq!(metric.class, CLASS_DURABLE_DERIVED);
        assert!(!metric.definition.is_empty());
        assert_eq!(metric.value, 30);
        assert!(metric.exact);
    }

    #[test]
    fn coarsen_reports_zero_exactly_never_treating_absence_as_sensitive() {
        assert_eq!(coarsen(0, 5), (0, true));
    }

    #[test]
    fn coarsen_reports_a_cohort_below_the_floor_as_the_floor_itself_marked_inexact() {
        assert_eq!(coarsen(1, 5), (5, false));
        assert_eq!(coarsen(4, 5), (5, false));
    }

    #[test]
    fn coarsen_reports_a_cohort_at_or_above_the_floor_exactly() {
        assert_eq!(coarsen(5, 5), (5, true));
        assert_eq!(coarsen(1000, 5), (1000, true));
    }

    #[test]
    fn min_cohort_falls_back_to_the_default_when_unset_or_invalid() {
        // SAFETY-of-intent note: `std::env::set_var` is process-global;
        // no other test in this crate touches `AVALON_REGISTRY_MIN_COHORT`,
        // so it's safe here despite being `unsafe` in edition-2024 terms —
        // same posture `crates/server/src/recovery.rs`'s own env-var
        // fallback tests take.
        unsafe {
            std::env::remove_var("AVALON_REGISTRY_MIN_COHORT");
        }
        assert_eq!(min_cohort(), DEFAULT_MIN_COHORT);

        unsafe {
            std::env::set_var("AVALON_REGISTRY_MIN_COHORT", "0");
        }
        assert_eq!(min_cohort(), DEFAULT_MIN_COHORT);

        unsafe {
            std::env::set_var("AVALON_REGISTRY_MIN_COHORT", "not-a-number");
        }
        assert_eq!(min_cohort(), DEFAULT_MIN_COHORT);

        unsafe {
            std::env::set_var("AVALON_REGISTRY_MIN_COHORT", "10");
        }
        assert_eq!(min_cohort(), 10);

        unsafe {
            std::env::remove_var("AVALON_REGISTRY_MIN_COHORT");
        }
    }
}
