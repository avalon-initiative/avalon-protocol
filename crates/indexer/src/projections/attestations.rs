//! The attestation-status cache, built from `achievement.issued` /
//! `achievement.revoked` — see `docs/architecture/achievements-and-attestations.md`
//! and `avalon_protocol::achievements::AchievementAttestation`, whose shape
//! this projection's payload expectations mirror.
//!
//! Nothing in this repo emits either event kind yet: Epic #30
//! (Achievements & Attestations) is still scaffolding, same as this crate
//! was before this ticket. This projection exists so #30's future issuing
//! flow has a read model ready to consume it on day one, matching the
//! ticket's design (issue #42), and so its own idempotency/decode behavior
//! is provable now via fixture events rather than only once a real issuer
//! exists. `revoked_at` is a cache of the latest relevant event, per
//! `docs/architecture/query-and-indexing.md`'s "current status is a cache"
//! rule — the event log remains the record of *when* and *why*.

use std::collections::HashSet;

use avalon_protocol::events::ProtocolEvent;
use sqlx::{PgPool, Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::IndexError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttestationWrite {
    Issue {
        id: Uuid,
        issuer: String,
        subject: Uuid,
        achievement: String,
        issued_at: OffsetDateTime,
    },
    Revoke {
        id: Uuid,
        revoked_at: OffsetDateTime,
    },
}

pub fn decode(event: &ProtocolEvent) -> Option<AttestationWrite> {
    match event.kind.as_str() {
        "achievement.issued" => {
            let id = super::uuid_field(&event.payload, "id")?;
            let issuer = event.payload.get("issuer")?.as_str()?.to_string();
            let subject = super::uuid_field(&event.payload, "subject")?;
            let achievement = event.payload.get("achievement")?.as_str()?.to_string();
            Some(AttestationWrite::Issue {
                id,
                issuer,
                subject,
                achievement,
                issued_at: event.timestamp,
            })
        }
        "achievement.revoked" => {
            let id = super::uuid_field(&event.payload, "id")?;
            Some(AttestationWrite::Revoke {
                id,
                revoked_at: event.timestamp,
            })
        }
        _ => None,
    }
}

pub async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    write: &AttestationWrite,
) -> Result<(), IndexError> {
    match write {
        AttestationWrite::Issue {
            id,
            issuer,
            subject,
            achievement,
            issued_at,
        } => {
            sqlx::query(
                "INSERT INTO indexer_attestations (id, issuer, subject, achievement, issued_at) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (id) DO UPDATE SET \
                     issuer = EXCLUDED.issuer, \
                     subject = EXCLUDED.subject, \
                     achievement = EXCLUDED.achievement",
            )
            .bind(id)
            .bind(issuer)
            .bind(subject)
            .bind(achievement)
            .bind(issued_at)
            .execute(&mut **tx)
            .await?;
        }
        AttestationWrite::Revoke { id, revoked_at } => {
            sqlx::query("UPDATE indexer_attestations SET revoked_at = $2 WHERE id = $1")
                .bind(id)
                .bind(revoked_at)
                .execute(&mut **tx)
                .await?;
        }
    }
    Ok(())
}

/// Current attestation state, derived by folding decoded writes in
/// order — pure and unit-testable without Postgres, same purpose
/// `projections::game_bindings::fold`/`BindingState` serve for the
/// binding metrics: it lets the Game Registry's (#89, first slice #261)
/// achievement metrics be proven against hand-built fixture events, no
/// live Postgres needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestationState {
    pub issuer: String,
    pub subject: Uuid,
    pub revoked_at: Option<OffsetDateTime>,
}

pub fn fold(writes: &[AttestationWrite]) -> Vec<AttestationState> {
    use std::collections::HashMap;
    let mut by_id: HashMap<Uuid, AttestationState> = HashMap::new();
    for write in writes {
        match write {
            AttestationWrite::Issue {
                id,
                issuer,
                subject,
                ..
            } => {
                by_id.insert(
                    *id,
                    AttestationState {
                        issuer: issuer.clone(),
                        subject: *subject,
                        revoked_at: None,
                    },
                );
            }
            AttestationWrite::Revoke { id, revoked_at } => {
                if let Some(state) = by_id.get_mut(id) {
                    state.revoked_at = Some(*revoked_at);
                }
            }
        }
    }
    by_id.into_values().collect()
}

/// "achievements issued" — count of `achievement.issued` events by
/// `issuer` (one per distinct attestation id, since each id is issued
/// exactly once).
pub fn count_issued(states: &[AttestationState], issuer: &str) -> usize {
    states.iter().filter(|s| s.issuer == issuer).count()
}

/// "achievements revoked" — count of attestations by `issuer` that carry
/// a `revoked_at` at all, regardless of whether that timestamp is past or
/// future. This intentionally does NOT mirror `is_valid`'s "future
/// `revoked_at` still counts as valid" rule -- it mirrors the SQL
/// equivalent, [`revoked_count`], which counts `revoked_at IS NOT NULL`
/// unconditionally (i.e. "has a revocation been recorded", not "is it
/// revoked as of now").
pub fn count_revoked(states: &[AttestationState], issuer: &str) -> usize {
    states
        .iter()
        .filter(|s| s.issuer == issuer && s.revoked_at.is_some())
        .count()
}

/// "unique achievement holders" — distinct subjects with at least one
/// currently-valid attestation from `issuer` as of `now`, matching
/// `avalon_protocol::achievements::AchievementAttestation::is_valid`
/// exactly: an attestation is valid unless it has a `revoked_at` that is
/// in the past. A future `revoked_at` (scheduled but not yet in effect)
/// still counts as a valid, currently-held attestation.
pub fn count_unique_holders(
    states: &[AttestationState],
    issuer: &str,
    now: OffsetDateTime,
) -> usize {
    states
        .iter()
        .filter(|s| {
            s.issuer == issuer
                && match s.revoked_at {
                    Some(revoked_at) => revoked_at > now,
                    None => true,
                }
        })
        .map(|s| s.subject)
        .collect::<HashSet<_>>()
        .len()
}

/// The SQL-backed equivalent of [`count_issued`], read at request time
/// from `indexer_attestations` rather than replayed in memory — what
/// `crate::registry::compute_for_game` actually calls.
pub async fn issued_count(pool: &PgPool, issuer: &str) -> Result<i64, IndexError> {
    let row = sqlx::query("SELECT COUNT(*) AS c FROM indexer_attestations WHERE issuer = $1")
        .bind(issuer)
        .fetch_one(pool)
        .await?;
    Ok(row.try_get("c")?)
}

/// The SQL-backed equivalent of [`count_revoked`].
pub async fn revoked_count(pool: &PgPool, issuer: &str) -> Result<i64, IndexError> {
    let row = sqlx::query(
        "SELECT COUNT(*) AS c FROM indexer_attestations \
         WHERE issuer = $1 AND revoked_at IS NOT NULL",
    )
    .bind(issuer)
    .fetch_one(pool)
    .await?;
    Ok(row.try_get("c")?)
}

/// The SQL-backed equivalent of [`count_unique_holders`]. `revoked_at IS
/// NULL OR revoked_at > now()` mirrors
/// `avalon_protocol::achievements::AchievementAttestation::is_valid`'s
/// definition of validity exactly, rather than approximating it as a bare
/// `revoked_at IS NULL` check.
pub async fn unique_holder_count(pool: &PgPool, issuer: &str) -> Result<i64, IndexError> {
    let row = sqlx::query(
        "SELECT COUNT(DISTINCT subject) AS c FROM indexer_attestations \
         WHERE issuer = $1 AND (revoked_at IS NULL OR revoked_at > now())",
    )
    .bind(issuer)
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
            issuer: GlobalId::new("game", &Uuid::new_v4().to_string(), "self", "x"),
            subject: GlobalId::new("identity", &Uuid::new_v4().to_string(), "self", "x"),
            payload,
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }
    }

    #[test]
    fn decodes_achievement_issued() {
        let id = Uuid::new_v4();
        let subject = Uuid::new_v4();
        let source_event = event(
            "achievement.issued",
            serde_json::json!({
                "id": id,
                "issuer": "game:ashen-realms",
                "subject": subject,
                "achievement": "game:ashen-realms:achievement:dragon_slayer",
            }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write,
            AttestationWrite::Issue {
                id,
                issuer: "game:ashen-realms".to_string(),
                subject,
                achievement: "game:ashen-realms:achievement:dragon_slayer".to_string(),
                issued_at: source_event.timestamp,
            }
        );
    }

    #[test]
    fn decodes_achievement_revoked() {
        let id = Uuid::new_v4();
        let source_event = event("achievement.revoked", serde_json::json!({ "id": id }));
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write,
            AttestationWrite::Revoke {
                id,
                revoked_at: source_event.timestamp,
            }
        );
    }

    #[test]
    fn unrecognized_kind_decodes_to_none() {
        assert_eq!(decode(&event("guild.created", serde_json::json!({}))), None);
    }

    #[test]
    fn malformed_payload_decodes_to_none() {
        assert_eq!(
            decode(&event(
                "achievement.issued",
                serde_json::json!({ "id": "nope" })
            )),
            None
        );
    }

    /// Fixture: this issuer issues three attestations to two distinct
    /// subjects and revokes one of them; a different issuer's attestation
    /// is noise that must not be counted. Known event stream, known
    /// expected values — the ticket's fixture-test requirement for the
    /// three achievement metrics.
    #[test]
    fn achievement_metrics_from_a_fixture_event_stream() {
        let issuer = "game:ashen-realms";
        let other_issuer = "game:other-game";
        let subject_x = Uuid::new_v4();
        let subject_y = Uuid::new_v4();

        let issue = |id: Uuid, issuer: &str, subject: Uuid| {
            event(
                "achievement.issued",
                serde_json::json!({
                    "id": id,
                    "issuer": issuer,
                    "subject": subject,
                    "achievement": "game:ashen-realms:achievement:dragon_slayer",
                }),
            )
        };
        let revoke = |id: Uuid| event("achievement.revoked", serde_json::json!({ "id": id }));

        let attestation_1 = Uuid::new_v4();
        let attestation_2 = Uuid::new_v4();
        let attestation_3 = Uuid::new_v4();
        let noise_attestation = Uuid::new_v4();

        let events = [
            issue(attestation_1, issuer, subject_x),
            issue(attestation_2, issuer, subject_x),
            issue(attestation_3, issuer, subject_y),
            revoke(attestation_2),
            issue(noise_attestation, other_issuer, subject_x),
        ];

        let writes: Vec<AttestationWrite> = events.iter().filter_map(decode).collect();
        let states = fold(&writes);
        let now = OffsetDateTime::now_utc();

        assert_eq!(count_issued(&states, issuer), 3);
        assert_eq!(count_revoked(&states, issuer), 1);
        // subject_x still holds attestation_1 (valid); subject_y holds
        // attestation_3 — 2 unique holders despite 3 issued attestations.
        assert_eq!(count_unique_holders(&states, issuer, now), 2);
    }

    /// The exact discrepancy this suite previously didn't cover: an
    /// attestation revoked at a timestamp still in the future (scheduled,
    /// not yet in effect) must still count as a valid, currently-held
    /// attestation — mirroring `AchievementAttestation::is_valid` and the
    /// SQL's `revoked_at IS NULL OR revoked_at > now()`. A plain
    /// `revoked: bool` flag flipped permanently true the instant any
    /// `Revoke` write is folded (the old shape of `AttestationState`)
    /// would undercount this holder by one; comparing the stored
    /// `revoked_at` against `now` gets it right.
    #[test]
    fn future_revoked_at_still_counts_as_a_valid_holder() {
        let issuer = "game:ashen-realms";
        let subject = Uuid::new_v4();
        let attestation_id = Uuid::new_v4();
        let now = OffsetDateTime::now_utc();
        let future = now + time::Duration::days(7);

        let issue_event = event(
            "achievement.issued",
            serde_json::json!({
                "id": attestation_id,
                "issuer": issuer,
                "subject": subject,
                "achievement": "game:ashen-realms:achievement:dragon_slayer",
            }),
        );
        let mut revoke_event = event(
            "achievement.revoked",
            serde_json::json!({ "id": attestation_id }),
        );
        revoke_event.timestamp = future;

        let writes: Vec<AttestationWrite> = [issue_event, revoke_event]
            .iter()
            .filter_map(decode)
            .collect();
        let states = fold(&writes);

        // Still valid as of `now` — the revocation hasn't taken effect yet.
        assert_eq!(count_unique_holders(&states, issuer, now), 1);
        // But is no longer valid once `now` passes the scheduled revocation.
        assert_eq!(
            count_unique_holders(&states, issuer, future + time::Duration::seconds(1)),
            0
        );
    }

    /// Replaying the same event stream twice reproduces the same values —
    /// `fold` converges on natural key (attestation id).
    #[test]
    fn rebuilding_from_the_same_events_twice_reproduces_the_same_values() {
        let issuer = "game:ashen-realms";
        let subject = Uuid::new_v4();
        let attestation_id = Uuid::new_v4();
        let events = [event(
            "achievement.issued",
            serde_json::json!({
                "id": attestation_id,
                "issuer": issuer,
                "subject": subject,
                "achievement": "game:ashen-realms:achievement:dragon_slayer",
            }),
        )];

        let writes: Vec<AttestationWrite> = events.iter().filter_map(decode).collect();
        let mut doubled = writes.clone();
        doubled.extend(writes.clone());

        let states_once = fold(&writes);
        let states_twice = fold(&doubled);
        let now = OffsetDateTime::now_utc();

        assert_eq!(
            count_issued(&states_once, issuer),
            count_issued(&states_twice, issuer)
        );
        assert_eq!(
            count_unique_holders(&states_once, issuer, now),
            count_unique_holders(&states_twice, issuer, now)
        );
    }
}
