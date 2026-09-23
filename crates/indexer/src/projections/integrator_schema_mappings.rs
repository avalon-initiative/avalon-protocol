//! The Integrator Registry's schema-mapping discovery projection: an
//! integrator's published `IntegratorSchemaMapping`s, surfaced
//! through the same indexer-projection machinery every other read model in
//! this crate already uses — same posture
//! `crate::projections::integrator_schemas` established for schema
//! versions themselves.
//!
//! Decodes `game_schema_mapping.published`
//! (`crates/server/src/integrator_schema_mappings.rs::publish_mapping`)
//! into a natural-key upsert — idempotent under replay, same as every
//! other projection here. Unlike schema versions, a mapping has no
//! `superseded_by`/lineage to update on a later event; each published
//! mapping is a standalone fact.

use avalon_protocol::events::ProtocolEvent;
use sqlx::{PgPool, Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::IndexError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegratorSchemaMappingPublished {
    pub id: String,
    pub integrator_id: Uuid,
    pub from_schema_id: String,
    pub to_schema_id: String,
    pub description: String,
    pub field_correspondence: serde_json::Value,
    pub published_at: OffsetDateTime,
}

pub fn decode(event: &ProtocolEvent) -> Option<IntegratorSchemaMappingPublished> {
    if event.kind != "game_schema_mapping.published" {
        return None;
    }
    let id = event.payload.get("id")?.as_str()?.to_string();
    let integrator_id = super::uuid_field(&event.payload, "integrator_id")?;
    let from_schema_id = event.payload.get("from_schema_id")?.as_str()?.to_string();
    let to_schema_id = event.payload.get("to_schema_id")?.as_str()?.to_string();
    let description = event
        .payload
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let field_correspondence = event
        .payload
        .get("field_correspondence")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    Some(IntegratorSchemaMappingPublished {
        id,
        integrator_id,
        from_schema_id,
        to_schema_id,
        description,
        field_correspondence,
        published_at: event.timestamp,
    })
}

pub async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    write: &IntegratorSchemaMappingPublished,
) -> Result<(), IndexError> {
    sqlx::query(
        "INSERT INTO indexer_integrator_schema_mappings \
         (id, integrator_id, from_schema_id, to_schema_id, description, \
          field_correspondence, published_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (id) DO UPDATE SET \
             integrator_id = EXCLUDED.integrator_id, \
             from_schema_id = EXCLUDED.from_schema_id, \
             to_schema_id = EXCLUDED.to_schema_id, \
             description = EXCLUDED.description, \
             field_correspondence = EXCLUDED.field_correspondence, \
             published_at = EXCLUDED.published_at",
    )
    .bind(&write.id)
    .bind(write.integrator_id)
    .bind(&write.from_schema_id)
    .bind(&write.to_schema_id)
    .bind(&write.description)
    .bind(&write.field_correspondence)
    .bind(write.published_at)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// One row of the registry's discovery surface — an integrator's published
/// mappings, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegratorSchemaMappingRow {
    pub id: String,
    pub from_schema_id: String,
    pub to_schema_id: String,
    pub description: String,
    pub field_correspondence: serde_json::Value,
    pub published_at: OffsetDateTime,
}

/// Every published mapping for `integrator_id`, oldest first — empty for an
/// integrator with no publications, same "absence means nothing published"
/// posture the rest of this crate's read models use.
pub async fn list_for_integrator(
    pool: &PgPool,
    integrator_id: Uuid,
) -> Result<Vec<IntegratorSchemaMappingRow>, IndexError> {
    let rows = sqlx::query(
        "SELECT id, from_schema_id, to_schema_id, description, field_correspondence, published_at \
         FROM indexer_integrator_schema_mappings WHERE integrator_id = $1 ORDER BY published_at",
    )
    .bind(integrator_id)
    .fetch_all(pool)
    .await?;

    let mut mappings = Vec::with_capacity(rows.len());
    for row in rows {
        mappings.push(IntegratorSchemaMappingRow {
            id: row.try_get("id")?,
            from_schema_id: row.try_get("from_schema_id")?,
            to_schema_id: row.try_get("to_schema_id")?,
            description: row.try_get("description")?,
            field_correspondence: row.try_get("field_correspondence")?,
            published_at: row.try_get("published_at")?,
        });
    }
    Ok(mappings)
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
            subject: GlobalId::new("game", "ashen-realms", "schema_mapping", "1"),
            payload,
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }
    }

    #[test]
    fn decodes_a_published_mapping() {
        let integrator_id = Uuid::new_v4();
        let source_event = event(
            "game_schema_mapping.published",
            serde_json::json!({
                "id": "game:ashen-realms:schema_mapping:1",
                "integrator_id": integrator_id,
                "from_schema_id": "game:ashen-realms:schema:1",
                "to_schema_id": "game:ashen-realms:schema:2",
                "description": "progression fields were nested",
                "field_correspondence": { "level": "progression.rank" },
            }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write,
            IntegratorSchemaMappingPublished {
                id: "game:ashen-realms:schema_mapping:1".to_string(),
                integrator_id,
                from_schema_id: "game:ashen-realms:schema:1".to_string(),
                to_schema_id: "game:ashen-realms:schema:2".to_string(),
                description: "progression fields were nested".to_string(),
                field_correspondence: serde_json::json!({ "level": "progression.rank" }),
                published_at: source_event.timestamp,
            }
        );
    }

    #[test]
    fn decodes_missing_description_and_field_correspondence_to_defaults() {
        let integrator_id = Uuid::new_v4();
        let source_event = event(
            "game_schema_mapping.published",
            serde_json::json!({
                "id": "game:ashen-realms:schema_mapping:1",
                "integrator_id": integrator_id,
                "from_schema_id": "game:ashen-realms:schema:1",
                "to_schema_id": "game:ashen-realms:schema:2",
            }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(write.description, "");
        assert_eq!(write.field_correspondence, serde_json::json!({}));
    }

    #[test]
    fn unrecognized_kind_decodes_to_none() {
        assert_eq!(decode(&event("guild.created", serde_json::json!({}))), None);
    }

    #[test]
    fn malformed_payload_decodes_to_none() {
        assert_eq!(
            decode(&event(
                "game_schema_mapping.published",
                serde_json::json!({ "id": "game:ashen-realms:schema_mapping:1" })
            )),
            None
        );
    }
}
