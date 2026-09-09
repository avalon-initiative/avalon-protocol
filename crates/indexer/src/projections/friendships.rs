//! The friendships read model, built from `friend.accepted`/`friend.removed`.
//!
//! Deliberately its own `indexer_friendships` table
//! (`crates/server/db/migrations/0014_indexer_projections`), not the
//! `friendships` table `crates/server/src/friends.rs` already writes
//! directly at request time
//! (`crates/server/db/migrations/0004_social_graph`). Retargeting that
//! write path — so `friends.rs` stops writing it and reads go through the
//! indexer instead — is issue #44's job, not this ticket's (#42): writing
//! both paths into the same table here would immediately violate this
//! ticket's own invariant ("no projection table is written by a request
//! handler; only by `Indexer::apply`") the moment `friends.rs` also wrote a
//! row for the same event. A separate table lets this projection exist,
//! and be tested, without destabilizing the social-graph feature that
//! already ships against the original table.
//!
//! `friend.requested` is a recognized kind with no effect here — a pending
//! request has no current-state row in a friendship roster (see
//! `docs/architecture/social-graph.md`); [`decode`] returns `None` for it,
//! same as for a kind this projection has never heard of.

use avalon_protocol::events::ProtocolEvent;
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::IndexError;

/// `a < b` always — mirrors `crates/server/src/friends.rs::ordered_pair` so
/// a pair is stored once regardless of who requested whom.
fn ordered_pair(x: Uuid, y: Uuid) -> (Uuid, Uuid) {
    if x < y {
        (x, y)
    } else {
        (y, x)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FriendshipWrite {
    Upsert {
        a: Uuid,
        b: Uuid,
        since: OffsetDateTime,
    },
    Remove {
        a: Uuid,
        b: Uuid,
    },
}

pub fn decode(event: &ProtocolEvent) -> Option<FriendshipWrite> {
    match event.kind.as_str() {
        "friend.accepted" => {
            let from = super::uuid_field(&event.payload, "from")?;
            let to = super::uuid_field(&event.payload, "to")?;
            let (a, b) = ordered_pair(from, to);
            Some(FriendshipWrite::Upsert {
                a,
                b,
                since: event.timestamp,
            })
        }
        "friend.removed" => {
            let a = super::uuid_field(&event.payload, "a")?;
            let b = super::uuid_field(&event.payload, "b")?;
            Some(FriendshipWrite::Remove { a, b })
        }
        _ => None,
    }
}

pub async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    write: &FriendshipWrite,
) -> Result<(), IndexError> {
    match write {
        FriendshipWrite::Upsert { a, b, since } => {
            sqlx::query(
                "INSERT INTO indexer_friendships (a, b, since) VALUES ($1, $2, $3) \
                 ON CONFLICT (a, b) DO NOTHING",
            )
            .bind(a)
            .bind(b)
            .bind(since)
            .execute(&mut **tx)
            .await?;
        }
        FriendshipWrite::Remove { a, b } => {
            sqlx::query("DELETE FROM indexer_friendships WHERE a = $1 AND b = $2")
                .bind(a)
                .bind(b)
                .execute(&mut **tx)
                .await?;
        }
    }
    Ok(())
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
            subject: GlobalId::new("identity", &Uuid::new_v4().to_string(), "self", "x"),
            payload,
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }
    }

    #[test]
    fn decodes_friend_accepted_into_an_ordered_upsert() {
        let x = Uuid::new_v4();
        let y = Uuid::new_v4();
        let source_event = event(
            "friend.accepted",
            serde_json::json!({ "from": x, "to": y, "actor": y }),
        );
        let write = decode(&source_event).unwrap();
        let (expected_a, expected_b) = ordered_pair(x, y);
        assert_eq!(
            write,
            FriendshipWrite::Upsert {
                a: expected_a,
                b: expected_b,
                since: source_event.timestamp,
            }
        );
    }

    #[test]
    fn decodes_friend_removed_into_a_remove() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let write = decode(&event(
            "friend.removed",
            serde_json::json!({ "a": a, "b": b, "actor": a }),
        ))
        .unwrap();
        assert_eq!(write, FriendshipWrite::Remove { a, b });
    }

    #[test]
    fn friend_requested_has_no_current_state_effect() {
        let write = decode(&event(
            "friend.requested",
            serde_json::json!({ "from": Uuid::new_v4(), "to": Uuid::new_v4() }),
        ));
        assert_eq!(write, None);
    }

    #[test]
    fn unrecognized_kind_decodes_to_none() {
        assert_eq!(decode(&event("guild.created", serde_json::json!({}))), None);
    }

    #[test]
    fn malformed_payload_decodes_to_none_not_a_panic() {
        assert_eq!(
            decode(&event(
                "friend.accepted",
                serde_json::json!({ "from": "not-a-uuid" })
            )),
            None
        );
    }
}
