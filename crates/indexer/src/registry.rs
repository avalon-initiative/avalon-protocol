//! The Game Registry's derived-metrics read model — issue #261, the first
//! concrete slice of the epic-sized #89. See
//! `docs/architecture/game-registry.md`.
//!
//! Composes two projections this crate already maintains
//! (`projections::game_bindings`, `projections::attestations`) into the
//! four metrics #261 scopes in: `players`, `total players ever`,
//! `achievements issued`/`revoked`, and `unique achievement holders`. Every
//! value carries its definition string and class label — the contract the
//! whole registry model depends on
//! (`docs/architecture/game-registry.md`: "every published metric carries
//! its definition and a class label"), never a bare number.
//!
//! All four are `durable-derived`: computed purely from durable protocol
//! events (bindings, achievement issue/revoke), never realtime and never
//! self-reported. `players online now` (realtime — #78 says realtime
//! numbers are never stored as durable metrics) and self-reported fields
//! (genre, website) are explicitly deferred, not stubbed here.
//!
//! No composite score, no ranking — this module returns five independent
//! labeled facts and nothing that combines them, matching #89's own
//! invariant.

use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::projections::{attestations, game_bindings};
use crate::IndexError;

pub const CLASS_DURABLE_DERIVED: &str = "durable-derived";

/// One labeled fact: a value, its precise definition, and the class of
/// evidence it came from. Never serialized as a bare number.
#[derive(Debug, Clone, Serialize)]
pub struct Metric {
    pub value: i64,
    pub definition: &'static str,
    pub class: &'static str,
}

impl Metric {
    fn durable_derived(value: i64, definition: &'static str) -> Self {
        Self {
            value,
            definition,
            class: CLASS_DURABLE_DERIVED,
        }
    }
}

/// The four #261 metrics for one game/issuer, each independently labeled.
#[derive(Debug, Clone, Serialize)]
pub struct GameRegistryMetrics {
    pub players: Metric,
    pub total_players_ever: Metric,
    pub achievements_issued: Metric,
    pub achievements_revoked: Metric,
    pub unique_achievement_holders: Metric,
}

/// Computes all four metrics for `game_id`. `issuer` is the string form
/// achievement events attribute to this game — `format!("game:{slug}")`,
/// matching the convention `projections::attestations`'s own fixtures
/// already established for the `issuer` field
/// (`avalon_protocol::achievements::Issuer::Game` serialized as a plain
/// `game:<slug>` string, ahead of achievement issuing — Epic #30 — actually
/// landing).
///
/// Reads the projection tables directly (SQL aggregates), never replays
/// events in memory — this is the request-time path; the pure
/// `fold`/`count_*` helpers in `projections::game_bindings` and
/// `projections::attestations` exist so the same arithmetic can be proven
/// against fixture events without Postgres (see their test modules).
///
/// Never errors for a game with no activity — every count is `0`, not a
/// missing field or a distinguished error, matching this crate's "absence
/// means nothing happened yet" posture (`docs/architecture/query-and-indexing.md`).
pub async fn compute_for_game(
    pool: &PgPool,
    game_id: Uuid,
    issuer: &str,
) -> Result<GameRegistryMetrics, IndexError> {
    let players = game_bindings::active_player_count(pool, game_id).await?;
    let total_players_ever = game_bindings::total_players_ever(pool, game_id).await?;
    let achievements_issued = attestations::issued_count(pool, issuer).await?;
    let achievements_revoked = attestations::revoked_count(pool, issuer).await?;
    let unique_achievement_holders = attestations::unique_holder_count(pool, issuer).await?;

    Ok(GameRegistryMetrics {
        players: Metric::durable_derived(players, "distinct identities with an active GameBinding"),
        total_players_ever: Metric::durable_derived(
            total_players_ever,
            "distinct identities that ever had a binding",
        ),
        achievements_issued: Metric::durable_derived(
            achievements_issued,
            "count of achievement.issued events by this issuer",
        ),
        achievements_revoked: Metric::durable_derived(
            achievements_revoked,
            "count of achievement.revoked events by this issuer",
        ),
        unique_achievement_holders: Metric::durable_derived(
            unique_achievement_holders,
            "distinct subjects with at least one valid attestation from this issuer",
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
        let metric = Metric::durable_derived(3, "some definition");
        assert_eq!(metric.class, CLASS_DURABLE_DERIVED);
        assert!(!metric.definition.is_empty());
        assert_eq!(metric.value, 3);
    }
}
