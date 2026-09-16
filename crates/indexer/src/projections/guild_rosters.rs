//! Guild roster read model, built from `guild.member_added`,
//! `guild.member_removed`, and `guild.role_changed`.
//!
//! Its own `indexer_guild_members` table
//! (`crates/server/db/migrations/0015_indexer_projections`), for the same
//! reason [`super::friendships`] isn't `crates/server`'s existing
//! `guild_members` table (`crates/server/db/migrations/0009_guild_membership`,
//! written directly by `crates/server/src/guilds.rs`): retargeting that
//! write path is issue #506's job, and reusing the same table now would mean
//! two writers, violating this ticket's own invariant. Unlike the server's
//! `guild_members` table, this one has no foreign key into `guild_roles` —
//! the indexer decodes payload independently of whether a locally-known
//! `guild_roles` row exists for `role_index` (`docs/architecture/query-and-indexing.md`:
//! "the indexer consumes protocol semantics; it never redefines them").

use avalon_protocol::events::ProtocolEvent;
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::IndexError;

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
}
