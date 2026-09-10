//! The binding-status cache, built from `game.binding_established` /
//! `game.binding_ended` — see `docs/architecture/game-bindings.md` and
//! `avalon_protocol::games::GameBinding`, whose shape this projection's
//! payload expectations mirror.
//!
//! Kept as its own table (`indexer_game_bindings`) rather than reusing
//! `crates/server`'s existing `bindings` (0012_game_bindings), same reason
//! `friendships`/`guild_rosters`/`attestations` already get their own
//! tables per `docs/architecture/query-and-indexing.md`: `bindings` is
//! still written directly by `crates/server/src/connections.rs` at request
//! time, and retargeting that write path is #44's job, not this one's —
//! writing both paths into the same table would create two writers of one
//! projection.
//!
//! This is the Game Registry's (#89, first slice #261) source for the
//! `players` and `total players ever` metrics: `crate::registry` reads
//! this table's aggregate counts, never per-player rows, matching the
//! registry's "aggregates only" invariant.

use std::collections::HashSet;

use avalon_protocol::events::ProtocolEvent;
use sqlx::{PgPool, Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::IndexError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameBindingWrite {
    Establish {
        id: Uuid,
        identity_id: Uuid,
        game_id: Uuid,
        established_at: OffsetDateTime,
    },
    End {
        id: Uuid,
        ended_at: OffsetDateTime,
    },
}

pub fn decode(event: &ProtocolEvent) -> Option<GameBindingWrite> {
    match event.kind.as_str() {
        "game.binding_established" => {
            let id = super::uuid_field(&event.payload, "binding_id")?;
            let identity_id = super::uuid_field(&event.payload, "identity_id")?;
            let game_id = super::uuid_field(&event.payload, "game_id")?;
            Some(GameBindingWrite::Establish {
                id,
                identity_id,
                game_id,
                established_at: event.timestamp,
            })
        }
        "game.binding_ended" => {
            let id = super::uuid_field(&event.payload, "binding_id")?;
            Some(GameBindingWrite::End {
                id,
                ended_at: event.timestamp,
            })
        }
        _ => None,
    }
}

pub async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    write: &GameBindingWrite,
) -> Result<(), IndexError> {
    match write {
        GameBindingWrite::Establish {
            id,
            identity_id,
            game_id,
            established_at,
        } => {
            sqlx::query(
                "INSERT INTO indexer_game_bindings (id, identity_id, game_id, established_at) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (id) DO UPDATE SET \
                     identity_id = EXCLUDED.identity_id, \
                     game_id = EXCLUDED.game_id, \
                     established_at = EXCLUDED.established_at",
            )
            .bind(id)
            .bind(identity_id)
            .bind(game_id)
            .bind(established_at)
            .execute(&mut **tx)
            .await?;
        }
        GameBindingWrite::End { id, ended_at } => {
            sqlx::query("UPDATE indexer_game_bindings SET ended_at = $2 WHERE id = $1")
                .bind(id)
                .bind(ended_at)
                .execute(&mut **tx)
                .await?;
        }
    }
    Ok(())
}

/// Current binding state, derived by folding decoded writes in order —
/// pure and unit-testable without Postgres, mirroring what `apply`'s SQL
/// upserts converge to. Not itself a projection table; exists so the two
/// #261 binding metrics can be proven against hand-built fixture events
/// (see this module's tests) with no live Postgres, matching the ticket's
/// "fixture-based, no live Postgres" test requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingState {
    pub game_id: Uuid,
    pub identity_id: Uuid,
    pub active: bool,
}

pub fn fold(writes: &[GameBindingWrite]) -> Vec<BindingState> {
    use std::collections::HashMap;
    let mut by_id: HashMap<Uuid, BindingState> = HashMap::new();
    for write in writes {
        match write {
            GameBindingWrite::Establish {
                id,
                identity_id,
                game_id,
                ..
            } => {
                by_id.insert(
                    *id,
                    BindingState {
                        game_id: *game_id,
                        identity_id: *identity_id,
                        active: true,
                    },
                );
            }
            GameBindingWrite::End { id, .. } => {
                if let Some(state) = by_id.get_mut(id) {
                    state.active = false;
                }
            }
        }
    }
    by_id.into_values().collect()
}

/// "players" — distinct identities with an active `GameBinding` to
/// `game_id` (`docs/architecture/game-registry.md`).
pub fn count_active_players(states: &[BindingState], game_id: Uuid) -> usize {
    states
        .iter()
        .filter(|s| s.game_id == game_id && s.active)
        .map(|s| s.identity_id)
        .collect::<HashSet<_>>()
        .len()
}

/// "total players ever" — distinct identities that ever had a binding to
/// `game_id`, active or ended.
pub fn count_total_players_ever(states: &[BindingState], game_id: Uuid) -> usize {
    states
        .iter()
        .filter(|s| s.game_id == game_id)
        .map(|s| s.identity_id)
        .collect::<HashSet<_>>()
        .len()
}

/// The SQL-backed equivalent of [`count_active_players`], read at request
/// time from the projection table itself rather than replayed in memory —
/// what `crate::registry::compute_for_game` actually calls.
pub async fn active_player_count(pool: &PgPool, game_id: Uuid) -> Result<i64, IndexError> {
    let row = sqlx::query(
        "SELECT COUNT(DISTINCT identity_id) AS c FROM indexer_game_bindings \
         WHERE game_id = $1 AND ended_at IS NULL",
    )
    .bind(game_id)
    .fetch_one(pool)
    .await?;
    Ok(row.try_get("c")?)
}

/// The SQL-backed equivalent of [`count_total_players_ever`].
pub async fn total_players_ever(pool: &PgPool, game_id: Uuid) -> Result<i64, IndexError> {
    let row = sqlx::query(
        "SELECT COUNT(DISTINCT identity_id) AS c FROM indexer_game_bindings WHERE game_id = $1",
    )
    .bind(game_id)
    .fetch_one(pool)
    .await?;
    Ok(row.try_get("c")?)
}

#[cfg(test)]
mod tests {
    use avalon_protocol::ids::GlobalId;

    use super::*;

    fn event(kind: &str, payload: serde_json::Value) -> ProtocolEvent {
        ProtocolEvent {
            id: Uuid::new_v4(),
            kind: kind.to_string(),
            issuer: GlobalId::new("identity", &Uuid::new_v4().to_string(), "self", "x"),
            subject: GlobalId::new("game", "ashen-realms", "self", "x"),
            payload,
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }
    }

    fn established(binding_id: Uuid, identity_id: Uuid, game_id: Uuid) -> ProtocolEvent {
        event(
            "game.binding_established",
            serde_json::json!({
                "binding_id": binding_id,
                "identity_id": identity_id,
                "game_id": game_id,
                "slug": "ashen-realms",
            }),
        )
    }

    fn ended(binding_id: Uuid) -> ProtocolEvent {
        event(
            "game.binding_ended",
            serde_json::json!({ "binding_id": binding_id }),
        )
    }

    #[test]
    fn decodes_binding_established() {
        let binding_id = Uuid::new_v4();
        let identity_id = Uuid::new_v4();
        let game_id = Uuid::new_v4();
        let source_event = established(binding_id, identity_id, game_id);
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write,
            GameBindingWrite::Establish {
                id: binding_id,
                identity_id,
                game_id,
                established_at: source_event.timestamp,
            }
        );
    }

    #[test]
    fn decodes_binding_ended() {
        let binding_id = Uuid::new_v4();
        let source_event = ended(binding_id);
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write,
            GameBindingWrite::End {
                id: binding_id,
                ended_at: source_event.timestamp,
            }
        );
    }

    #[test]
    fn unrecognized_kind_decodes_to_none() {
        assert_eq!(decode(&event("guild.created", serde_json::json!({}))), None);
    }

    /// Fixture: three identities ever bind to the game, one of them ends
    /// its binding — "players" (active only) is 2, "total players ever"
    /// (active + ended) is 3. Known event stream, known expected value, no
    /// Postgres needed — the ticket's fixture-test requirement for these
    /// two metrics.
    #[test]
    fn players_and_total_players_ever_metrics_from_a_fixture_event_stream() {
        let game_id = Uuid::new_v4();
        let other_game_id = Uuid::new_v4();
        let player_a = Uuid::new_v4();
        let player_b = Uuid::new_v4();
        let player_c = Uuid::new_v4();
        let binding_a = Uuid::new_v4();
        let binding_b = Uuid::new_v4();
        let binding_c = Uuid::new_v4();
        let unrelated_binding = Uuid::new_v4();

        let events = [
            established(binding_a, player_a, game_id),
            established(binding_b, player_b, game_id),
            established(binding_c, player_c, game_id),
            ended(binding_c),
            // Noise: a binding to a different game must not count here.
            established(unrelated_binding, player_a, other_game_id),
        ];

        let writes: Vec<GameBindingWrite> = events.iter().filter_map(decode).collect();
        let states = fold(&writes);

        assert_eq!(count_active_players(&states, game_id), 2);
        assert_eq!(count_total_players_ever(&states, game_id), 3);
    }

    /// Replaying the same event stream twice (the rebuild scenario) must
    /// reproduce the same values — `fold` converges on natural key
    /// (`binding_id`), same idempotency property `apply`'s SQL upsert has.
    #[test]
    fn rebuilding_from_the_same_events_twice_reproduces_the_same_values() {
        let game_id = Uuid::new_v4();
        let player_a = Uuid::new_v4();
        let binding_a = Uuid::new_v4();
        let events = [established(binding_a, player_a, game_id)];

        let writes: Vec<GameBindingWrite> = events.iter().filter_map(decode).collect();
        let mut doubled = writes.clone();
        doubled.extend(writes.clone());

        let states_once = fold(&writes);
        let states_twice = fold(&doubled);

        assert_eq!(
            count_active_players(&states_once, game_id),
            count_active_players(&states_twice, game_id)
        );
        assert_eq!(
            count_total_players_ever(&states_once, game_id),
            count_total_players_ever(&states_twice, game_id)
        );
    }
}
