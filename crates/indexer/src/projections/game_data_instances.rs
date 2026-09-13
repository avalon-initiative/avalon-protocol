//! Game Space instance-data projection (issue #384, implementing #381's
//! decided policy): decodes `game_data.published`
//! (`crates/server/src/game_data.rs::publish_instance`) into an upsert of
//! the new instance row, plus — when the event names a `supersedes`
//! id — an update of that earlier row's `superseded_by`. Same shape as
//! `game_schemas`'s own projection in this same directory, matching the
//! `game_schemas`/`indexer_game_schemas` pairing exactly as the ticket
//! requires.
//!
//! **Read-side visibility filtering happens in `crates/server/src/game_data.rs`,
//! not here.** This module only stores/serves full instances — every
//! stored field, unfiltered — the same posture `indexer_game_schemas`
//! already has relative to `game_schemas::get_schema_version`'s public,
//! unfiltered `proto_source` read. The visibility rule is applied once,
//! at the one HTTP read endpoint, against the schema's visibility
//! metadata resolved via `game_schemas::get_visibility`.

use avalon_protocol::events::ProtocolEvent;
use sqlx::{PgPool, Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::IndexError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameDataPublished {
    pub id: String,
    pub schema_id: String,
    pub game_id: Uuid,
    pub subject: Uuid,
    pub instance: serde_json::Value,
    pub published_at: OffsetDateTime,
    /// The immediately-prior instance's id for this `(schema, subject)`
    /// pair, if this publication supersedes one.
    pub supersedes: Option<String>,
}

pub fn decode(event: &ProtocolEvent) -> Option<GameDataPublished> {
    if event.kind != "game_data.published" {
        return None;
    }
    let id = event.payload.get("id")?.as_str()?.to_string();
    let schema_id = event.payload.get("schema")?.as_str()?.to_string();
    let game_id = super::uuid_field(&event.payload, "game_id")?;
    let subject = super::uuid_field(&event.payload, "subject")?;
    let instance = event.payload.get("instance")?.clone();
    let supersedes = event
        .payload
        .get("supersedes")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Some(GameDataPublished {
        id,
        schema_id,
        game_id,
        subject,
        instance,
        published_at: event.timestamp,
        supersedes,
    })
}

pub async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    write: &GameDataPublished,
) -> Result<(), IndexError> {
    sqlx::query(
        "INSERT INTO indexer_game_data_instances \
         (id, schema_id, game_id, subject, instance, published_at, superseded_by) \
         VALUES ($1, $2, $3, $4, $5, $6, NULL) \
         ON CONFLICT (id) DO UPDATE SET \
             schema_id = EXCLUDED.schema_id, \
             game_id = EXCLUDED.game_id, \
             subject = EXCLUDED.subject, \
             instance = EXCLUDED.instance, \
             published_at = EXCLUDED.published_at",
    )
    .bind(&write.id)
    .bind(&write.schema_id)
    .bind(write.game_id)
    .bind(write.subject)
    .bind(&write.instance)
    .bind(write.published_at)
    .execute(&mut **tx)
    .await?;

    if let Some(supersedes) = &write.supersedes {
        sqlx::query("UPDATE indexer_game_data_instances SET superseded_by = $2 WHERE id = $1")
            .bind(supersedes)
            .bind(&write.id)
            .execute(&mut **tx)
            .await?;
    }

    Ok(())
}

/// One stored instance, as read back for `GET /identities/{id}/game-data`
/// — visibility filtering is applied by the caller
/// (`crates/server/src/game_data.rs`), not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameDataInstanceRow {
    pub id: String,
    pub schema_id: String,
    pub game_id: Uuid,
    pub instance: serde_json::Value,
    pub published_at: OffsetDateTime,
}

/// Every *current* (non-superseded) instance belonging to `subject`,
/// across every schema/game that has ever published one — empty if none,
/// same "absence means nothing published" posture the rest of this
/// crate's read models use.
pub async fn list_current_for_subject(
    pool: &PgPool,
    subject: Uuid,
) -> Result<Vec<GameDataInstanceRow>, IndexError> {
    let rows = sqlx::query(
        "SELECT id, schema_id, game_id, instance, published_at \
         FROM indexer_game_data_instances \
         WHERE subject = $1 AND superseded_by IS NULL \
         ORDER BY published_at",
    )
    .bind(subject)
    .fetch_all(pool)
    .await?;

    let mut instances = Vec::with_capacity(rows.len());
    for row in rows {
        instances.push(GameDataInstanceRow {
            id: row.try_get("id")?,
            schema_id: row.try_get("schema_id")?,
            game_id: row.try_get("game_id")?,
            instance: row.try_get("instance")?,
            published_at: row.try_get("published_at")?,
        });
    }
    Ok(instances)
}

#[cfg(test)]
mod tests {
    use avalon_protocol::ids::GlobalId;

    use super::*;

    fn event(kind: &str, payload: serde_json::Value) -> ProtocolEvent {
        ProtocolEvent {
            id: Uuid::new_v4(),
            kind: kind.to_string(),
            issuer: GlobalId::new("game", "ashen-realms", "self", "x"),
            subject: GlobalId::new("identity", "abc", "game_data", "1"),
            payload,
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }
    }

    #[test]
    fn decodes_a_first_publication_with_no_supersedes() {
        let game_id = Uuid::new_v4();
        let subject = Uuid::new_v4();
        let source_event = event(
            "game_data.published",
            serde_json::json!({
                "id": "game:ashen-realms:schema:1:data:1",
                "schema": "game:ashen-realms:schema:1",
                "game_id": game_id,
                "subject": subject,
                "instance": { "level": 5 },
                "supersedes": null,
            }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(write.schema_id, "game:ashen-realms:schema:1");
        assert_eq!(write.subject, subject);
        assert_eq!(write.instance, serde_json::json!({ "level": 5 }));
        assert_eq!(write.supersedes, None);
    }

    #[test]
    fn unrecognized_kind_decodes_to_none() {
        assert_eq!(decode(&event("guild.created", serde_json::json!({}))), None);
    }

    #[test]
    fn malformed_payload_decodes_to_none() {
        assert_eq!(
            decode(&event(
                "game_data.published",
                serde_json::json!({ "id": "x" })
            )),
            None
        );
    }
}
