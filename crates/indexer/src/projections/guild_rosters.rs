//! Guild roster read model, built from `guild.created` (the owner's implicit
//! membership), `guild.member_added`, `guild.member_removed`, and
//! `guild.role_changed`.
//!
//! Its own `indexer_guild_members` table
//! (`crates/server/db/migrations/0015_indexer_projections`). Issue #506
//! retargeted `crates/server/src/guilds.rs` onto this table: it no longer
//! writes `crates/server`'s prior `guild_members` table
//! (`crates/server/db/migrations/0009_guild_membership`) directly, and its
//! own membership/role reads go through [`role_index_for`], [`roster`], and
//! [`memberships_for`] below (generic over `sqlx::PgExecutor`, same pattern
//! [`super::profiles::fetch`]/[`super::friendships::are_friends`] already
//! established). Unlike the server's old `guild_members` table, this one has
//! no foreign key into `guild_roles` — the indexer decodes payload
//! independently of whether a locally-known `guild_roles` row exists for
//! `role_index` (`docs/projects/backend-server/architecture/query-and-indexing.md`: "the indexer
//! consumes protocol semantics; it never redefines them").
//!
//! `guild.created` gets its own [`decode`] case (a rebuild test
//! surfaced this gap): a guild's owner is never issued a
//! separate `guild.member_added`, since guild creation already carries
//! `owner` in its own payload (see `worked-ledger-example.md`) — decoding it
//! here into the same `role_index = 0` (owner) upsert every other member row
//! gets means the roster is complete without every reader having to special-
//! case `guilds.owner` on top of it. This is a roster-*completeness* fix,
//! not a permissions one: `crate::server::guilds::has_guild_permission`
//! checks `actor == guild_owner` directly against the `guilds.owner` column
//! and never consulted `guild_members`/`indexer_guild_members` for the owner
//! case in the first place.

use avalon_protocol::events::ProtocolEvent;
use sqlx::{PgExecutor, Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::IndexError;

const OWNER_ROLE_INDEX: i32 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuildRosterWrite {
    /// Backs both `guild.member_added` (a fresh row) and `guild.role_changed`
    /// (an existing row's `role_index` changes) — [`apply`]'s `ON CONFLICT`
    /// only touches `role_index`, so a role change never clobbers the
    /// member's original `joined_at`.
    Upsert {
        guild_id: Uuid,
        identity_id: Uuid,
        role_index: i32,
        joined_at: OffsetDateTime,
    },
    Remove {
        guild_id: Uuid,
        identity_id: Uuid,
    },
}

fn role_index(payload: &serde_json::Value) -> Option<i32> {
    i32::try_from(payload.get("role_index")?.as_i64()?).ok()
}

pub fn decode(event: &ProtocolEvent) -> Option<GuildRosterWrite> {
    match event.kind.as_str() {
        "guild.created" => {
            let guild_id = super::uuid_field(&event.payload, "guild_id")?;
            let owner = super::uuid_field(&event.payload, "owner")?;
            Some(GuildRosterWrite::Upsert {
                guild_id,
                identity_id: owner,
                role_index: OWNER_ROLE_INDEX,
                joined_at: event.timestamp,
            })
        }
        "guild.member_added" | "guild.role_changed" => {
            let guild_id = super::uuid_field(&event.payload, "guild_id")?;
            let identity_id = super::uuid_field(&event.payload, "identity_id")?;
            let role_index = role_index(&event.payload)?;
            Some(GuildRosterWrite::Upsert {
                guild_id,
                identity_id,
                role_index,
                joined_at: event.timestamp,
            })
        }
        "guild.member_removed" => {
            let guild_id = super::uuid_field(&event.payload, "guild_id")?;
            let identity_id = super::uuid_field(&event.payload, "identity_id")?;
            Some(GuildRosterWrite::Remove {
                guild_id,
                identity_id,
            })
        }
        "guild.membership_reversed" => {
            let guild_id = super::uuid_field(&event.payload, "guild_id")?;
            let identity_id = super::uuid_field(&event.payload, "identity_id")?;
            match event.payload.get("effect")?.as_str()? {
                "membership_removed" => Some(GuildRosterWrite::Remove {
                    guild_id,
                    identity_id,
                }),
                "membership_restored" => Some(GuildRosterWrite::Upsert {
                    guild_id,
                    identity_id,
                    role_index: role_index(&event.payload)?,
                    joined_at: event.timestamp,
                }),
                _ => None,
            }
        }
        _ => None,
    }
}

pub async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    write: &GuildRosterWrite,
) -> Result<(), IndexError> {
    match write {
        GuildRosterWrite::Upsert {
            guild_id,
            identity_id,
            role_index,
            joined_at,
        } => {
            sqlx::query(
                "INSERT INTO indexer_guild_members (guild_id, identity_id, role_index, joined_at) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (guild_id, identity_id) DO UPDATE SET role_index = EXCLUDED.role_index",
            )
            .bind(guild_id)
            .bind(identity_id)
            .bind(role_index)
            .bind(joined_at)
            .execute(&mut **tx)
            .await?;
        }
        GuildRosterWrite::Remove {
            guild_id,
            identity_id,
        } => {
            sqlx::query(
                "DELETE FROM indexer_guild_members WHERE guild_id = $1 AND identity_id = $2",
            )
            .bind(guild_id)
            .bind(identity_id)
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(())
}

/// `identity_id`'s current `role_index` in `guild_id`, or `None` if they
/// aren't currently a member.
pub async fn role_index_for<'e, E>(
    executor: E,
    guild_id: Uuid,
    identity_id: Uuid,
) -> Result<Option<i32>, IndexError>
where
    E: PgExecutor<'e>,
{
    let role_index: Option<i32> = sqlx::query_scalar(
        "SELECT role_index FROM indexer_guild_members WHERE guild_id = $1 AND identity_id = $2",
    )
    .bind(guild_id)
    .bind(identity_id)
    .fetch_optional(executor)
    .await?;
    Ok(role_index)
}

/// `identity_id`'s current `joined_at` in `guild_id`, or `None` if they
/// aren't currently a member — the `joined_at` half of [`role_index_for`],
/// kept separate rather than always fetching both since most call sites
/// only need one or the other.
pub async fn joined_at_for<'e, E>(
    executor: E,
    guild_id: Uuid,
    identity_id: Uuid,
) -> Result<Option<OffsetDateTime>, IndexError>
where
    E: PgExecutor<'e>,
{
    let joined_at: Option<OffsetDateTime> = sqlx::query_scalar(
        "SELECT joined_at FROM indexer_guild_members WHERE guild_id = $1 AND identity_id = $2",
    )
    .bind(guild_id)
    .bind(identity_id)
    .fetch_optional(executor)
    .await?;
    Ok(joined_at)
}

/// Whether `identity_id` is currently a member of `guild_id`.
pub async fn is_member<'e, E>(
    executor: E,
    guild_id: Uuid,
    identity_id: Uuid,
) -> Result<bool, IndexError>
where
    E: PgExecutor<'e>,
{
    Ok(role_index_for(executor, guild_id, identity_id)
        .await?
        .is_some())
}

/// Current member count of `guild_id`.
pub async fn member_count<'e, E>(executor: E, guild_id: Uuid) -> Result<i64, IndexError>
where
    E: PgExecutor<'e>,
{
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM indexer_guild_members WHERE guild_id = $1")
            .bind(guild_id)
            .fetch_one(executor)
            .await?;
    Ok(count)
}

/// Whether any current member of `guild_id` holds `role_index` — issue
/// #506: the old `guild_members(guild_id, role_index)` foreign key into
/// `guild_roles` used to enforce this at the database level (a role delete
/// simply failed with a foreign-key violation); this table has no such
/// key on purpose (see the module doc comment), so a role-delete caller
/// needs this explicit check instead, run inside the same transaction as
/// the delete to avoid racing a concurrent role change.
pub async fn any_member_with_role<'e, E>(
    executor: E,
    guild_id: Uuid,
    role_index: i32,
) -> Result<bool, IndexError>
where
    E: PgExecutor<'e>,
{
    let row: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM indexer_guild_members WHERE guild_id = $1 AND role_index = $2 LIMIT 1",
    )
    .bind(guild_id)
    .bind(role_index)
    .fetch_optional(executor)
    .await?;
    Ok(row.is_some())
}

/// One roster row, from [`roster`].
#[derive(Debug, Clone)]
pub struct MemberRow {
    pub identity_id: Uuid,
    pub role_index: i32,
    pub joined_at: OffsetDateTime,
}

/// `guild_id`'s full current roster, ordered by `joined_at` — same shape
/// `crates/server/src/guilds.rs::list_members` already returns.
pub async fn roster<'e, E>(executor: E, guild_id: Uuid) -> Result<Vec<MemberRow>, IndexError>
where
    E: PgExecutor<'e>,
{
    let rows = sqlx::query(
        "SELECT identity_id, role_index, joined_at FROM indexer_guild_members \
         WHERE guild_id = $1 ORDER BY joined_at",
    )
    .bind(guild_id)
    .fetch_all(executor)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(MemberRow {
                identity_id: row.try_get("identity_id")?,
                role_index: row.try_get("role_index")?,
                joined_at: row.try_get("joined_at")?,
            })
        })
        .collect()
}

/// One membership row, from [`memberships_for`].
#[derive(Debug, Clone)]
pub struct MembershipRow {
    pub guild_id: Uuid,
    pub role_index: i32,
    pub joined_at: OffsetDateTime,
}

/// Every guild `identity_id` currently belongs to, ordered by `joined_at` —
/// same shape `crates/server/src/guilds.rs::list_my_guilds` already returns.
pub async fn memberships_for<'e, E>(
    executor: E,
    identity_id: Uuid,
) -> Result<Vec<MembershipRow>, IndexError>
where
    E: PgExecutor<'e>,
{
    let rows = sqlx::query(
        "SELECT guild_id, role_index, joined_at FROM indexer_guild_members \
         WHERE identity_id = $1 ORDER BY joined_at",
    )
    .bind(identity_id)
    .fetch_all(executor)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(MembershipRow {
                guild_id: row.try_get("guild_id")?,
                role_index: row.try_get("role_index")?,
                joined_at: row.try_get("joined_at")?,
            })
        })
        .collect()
}

/// Every identity that shares at least one guild membership with
/// `identity_id`, not yet filtered against the caller's own friends/blocks/
/// self — the "mutual guild members" set `crate::server::discovery` and
/// `crate::server::conversations` need.
pub async fn mutual_members<'e, E>(
    executor: E,
    identity_id: Uuid,
) -> Result<std::collections::HashSet<Uuid>, IndexError>
where
    E: PgExecutor<'e>,
{
    let rows = sqlx::query(
        r#"
        SELECT DISTINCT gm2.identity_id AS candidate
        FROM indexer_guild_members gm1
        JOIN indexer_guild_members gm2 ON gm2.guild_id = gm1.guild_id
        WHERE gm1.identity_id = $1 AND gm2.identity_id != $1
        "#,
    )
    .bind(identity_id)
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
            issuer: GlobalId::new("guild", &Uuid::new_v4().to_string(), "self", "x"),
            subject: GlobalId::new("guild", &Uuid::new_v4().to_string(), "self", "x"),
            payload,
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }
    }

    #[test]
    fn decodes_guild_created_into_an_owner_upsert() {
        let guild_id = Uuid::new_v4();
        let owner = Uuid::new_v4();
        let source_event = event(
            "guild.created",
            serde_json::json!({
                "guild_id": guild_id,
                "name": "Wandering Blades",
                "tag": "WB",
                "description": "",
                "owner": owner,
            }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write,
            GuildRosterWrite::Upsert {
                guild_id,
                identity_id: owner,
                role_index: OWNER_ROLE_INDEX,
                joined_at: source_event.timestamp,
            }
        );
    }

    #[test]
    fn decodes_member_added_into_an_upsert() {
        let guild_id = Uuid::new_v4();
        let identity_id = Uuid::new_v4();
        let source_event = event(
            "guild.member_added",
            serde_json::json!({ "guild_id": guild_id, "identity_id": identity_id, "role_index": 2, "via": "join", "actor": identity_id }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write,
            GuildRosterWrite::Upsert {
                guild_id,
                identity_id,
                role_index: 2,
                joined_at: source_event.timestamp,
            }
        );
    }

    #[test]
    fn decodes_role_changed_into_an_upsert_too() {
        let guild_id = Uuid::new_v4();
        let identity_id = Uuid::new_v4();
        let source_event = event(
            "guild.role_changed",
            serde_json::json!({ "guild_id": guild_id, "identity_id": identity_id, "role_index": 1, "actor": Uuid::new_v4() }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write,
            GuildRosterWrite::Upsert {
                guild_id,
                identity_id,
                role_index: 1,
                joined_at: source_event.timestamp,
            }
        );
    }

    #[test]
    fn decodes_member_removed_into_a_remove() {
        let guild_id = Uuid::new_v4();
        let identity_id = Uuid::new_v4();
        let write = decode(&event(
            "guild.member_removed",
            serde_json::json!({ "guild_id": guild_id, "identity_id": identity_id, "reason": "left", "actor": identity_id }),
        ))
        .unwrap();
        assert_eq!(
            write,
            GuildRosterWrite::Remove {
                guild_id,
                identity_id,
            }
        );
    }

    #[test]
    fn unrecognized_kind_decodes_to_none() {
        assert_eq!(
            decode(&event("friend.accepted", serde_json::json!({}))),
            None
        );
    }

    #[test]
    fn missing_role_index_decodes_to_none() {
        assert_eq!(
            decode(&event(
                "guild.member_added",
                serde_json::json!({ "guild_id": Uuid::new_v4(), "identity_id": Uuid::new_v4() }),
            )),
            None
        );
    }

    #[test]
    fn decodes_membership_reversed_by_effect() {
        let guild_id = Uuid::new_v4();
        let identity_id = Uuid::new_v4();
        let reversal = |effect: &str| {
            event(
                "guild.membership_reversed",
                serde_json::json!({
                    "reverses_event_id": Uuid::new_v4(),
                    "recovery_request_id": Uuid::new_v4(),
                    "guild_id": guild_id,
                    "identity_id": identity_id,
                    "effect": effect,
                    "role_index": 1,
                }),
            )
        };
        assert_eq!(
            decode(&reversal("membership_removed")),
            Some(GuildRosterWrite::Remove {
                guild_id,
                identity_id
            })
        );
        let restored = decode(&reversal("membership_restored")).unwrap();
        assert!(matches!(
            restored,
            GuildRosterWrite::Upsert { role_index: 1, .. }
        ));
        assert_eq!(decode(&reversal("something_else")), None);
    }
}
