//! The friendships read model, built from `friend.accepted`/`friend.removed`.
//!
//! Deliberately its own `indexer_friendships` table
//! (`crates/server/db/migrations/0015_indexer_projections`), not the old
//! `friendships` table (`crates/server/db/migrations/0004_social_graph`,
//! now dead — `crates/server/src/friends.rs` stopped writing it).
//!
//! `friend.requested` is a recognized kind with no effect here — a pending
//! request has no current-state row in a friendship roster (see
//! `docs/projects/backend-server/architecture/social-graph.md`); [`decode`] returns `None` for it,
//! same as for a kind this projection has never heard of.
//!
//! Issue #506: [`are_friends`], [`list_for`], and [`partners_of`] are the
//! read half, generic over `sqlx::PgExecutor` so a caller can pass either
//! the shared pool or an open transaction — same pattern
//! `projections::profiles::fetch`/`fetch_many` already established for
//! #44. `friends.rs`'s handlers now read `indexer_friendships` through
//! these instead of querying the old `friendships` table directly; writes
//! go through `PostgresIndexer::apply_in_tx`. `friend_requests` (pending,
//! not-yet-accepted requests) is untouched — it's workflow state, not
//! current-relationship state, and was never in this projection's scope.

use avalon_protocol::events::ProtocolEvent;
use sqlx::{PgExecutor, Postgres, Row, Transaction};
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

/// Whether `x`/`y` (in either order) are currently friends.
pub async fn are_friends<'e, E>(executor: E, x: Uuid, y: Uuid) -> Result<bool, IndexError>
where
    E: PgExecutor<'e>,
{
    let (a, b) = ordered_pair(x, y);
    let row: Option<i32> =
        sqlx::query_scalar("SELECT 1 FROM indexer_friendships WHERE a = $1 AND b = $2")
            .bind(a)
            .bind(b)
            .fetch_optional(executor)
            .await?;
    Ok(row.is_some())
}

/// One current friendship, from [`list_for`].
#[derive(Debug, Clone)]
pub struct FriendshipRow {
    pub a: Uuid,
    pub b: Uuid,
    pub since: OffsetDateTime,
}

/// Every current friendship involving `identity_id`.
pub async fn list_for<'e, E>(
    executor: E,
    identity_id: Uuid,
) -> Result<Vec<FriendshipRow>, IndexError>
where
    E: PgExecutor<'e>,
{
    let rows = sqlx::query("SELECT a, b, since FROM indexer_friendships WHERE a = $1 OR b = $1")
        .bind(identity_id)
        .fetch_all(executor)
        .await?;

    rows.into_iter()
        .map(|row| {
            Ok(FriendshipRow {
                a: row.try_get("a")?,
                b: row.try_get("b")?,
                since: row.try_get("since")?,
            })
        })
        .collect()
}

/// Every identity `identity_id` is currently friends with, as a flat set —
/// the shape `crate::server::friends::friend_partners`/presence's
/// friends-only visibility scope actually need, rather than the raw
/// `(a, b)` pairs [`list_for`] returns.
pub async fn partners_of<'e, E>(
    executor: E,
    identity_id: Uuid,
) -> Result<std::collections::HashSet<Uuid>, IndexError>
where
    E: PgExecutor<'e>,
{
    let rows = sqlx::query(
        r#"
        SELECT b AS other FROM indexer_friendships WHERE a = $1
        UNION
        SELECT a AS other FROM indexer_friendships WHERE b = $1
        "#,
    )
    .bind(identity_id)
    .fetch_all(executor)
    .await?;

    let mut set = std::collections::HashSet::with_capacity(rows.len());
    for row in rows {
        set.insert(row.try_get("other")?);
    }
    Ok(set)
}

/// Every identity with a current friendship to at least one id in
/// `identity_ids` — the "friends of friends" set `crate::server::discovery`
/// needs, before it's filtered against the caller's own
/// friends/blocks/self. Empty input short-circuits to an empty result
/// rather than issuing an `ANY($1)` query with an empty array.
pub async fn friends_of_any<'e, E>(
    executor: E,
    identity_ids: &[Uuid],
) -> Result<std::collections::HashSet<Uuid>, IndexError>
where
    E: PgExecutor<'e>,
{
    if identity_ids.is_empty() {
        return Ok(std::collections::HashSet::new());
    }
    let rows = sqlx::query(
        r#"
        SELECT b AS candidate FROM indexer_friendships WHERE a = ANY($1)
        UNION
        SELECT a AS candidate FROM indexer_friendships WHERE b = ANY($1)
        "#,
    )
    .bind(identity_ids)
    .fetch_all(executor)
    .await?;
    let mut set = std::collections::HashSet::with_capacity(rows.len());
    for row in rows {
        set.insert(row.try_get("candidate")?);
    }
    Ok(set)
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
