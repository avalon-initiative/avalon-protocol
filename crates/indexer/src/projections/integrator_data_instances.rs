//! Integrator Space instance-data projection: decodes `game_data.published`
//! (`crates/server/src/integrator_data.rs::publish_instance`) into an upsert of
//! the new instance row, plus — when the event names a `supersedes`
//! id — an update of that earlier row's `superseded_by`. Same shape as
//! `integrator_schemas`'s own projection in this same directory, matching the
//! `integrator_schemas`/`indexer_integrator_schemas` pairing exactly as the ticket
//! requires.
//!
//! **Read-side visibility filtering happens in `crates/server/src/integrator_data.rs`,
//! not here.** This module only stores/serves full instances — every
//! stored field, unfiltered — the same posture `indexer_integrator_schemas`
//! already has relative to `integrator_schemas::get_schema_version`'s public,
//! unfiltered `proto_source` read. The visibility rule is applied once,
//! at the one HTTP read endpoint, against the schema's visibility
//! metadata resolved via `integrator_schemas::get_visibility`.

use avalon_protocol::events::ProtocolEvent;
use sqlx::{PgPool, Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::IndexError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegratorDataPublished {
    pub id: String,
    pub schema_id: String,
    pub integrator_id: Uuid,
    pub subject: Uuid,
    pub instance: serde_json::Value,
    pub published_at: OffsetDateTime,
    /// The immediately-prior instance's id for this `(schema, subject)`
    /// pair, if this publication supersedes one.
    pub supersedes: Option<String>,
}

pub fn decode(event: &ProtocolEvent) -> Option<IntegratorDataPublished> {
    if event.kind != "game_data.published" {
        return None;
    }
    let id = event.payload.get("id")?.as_str()?.to_string();
    let schema_id = event.payload.get("schema")?.as_str()?.to_string();
    let integrator_id = super::uuid_field(&event.payload, "game_id")?;
    let subject = super::uuid_field(&event.payload, "subject")?;
    let instance = event.payload.get("instance")?.clone();
    let supersedes = event
        .payload
        .get("supersedes")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Some(IntegratorDataPublished {
        id,
        schema_id,
        integrator_id,
        subject,
        instance,
        published_at: event.timestamp,
        supersedes,
    })
}

/// Issue #533: `game_data.deleted`'s decoded shape — a tombstone
/// referencing an existing instance, never new content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegratorDataDeleted {
    pub instance_id: String,
    pub deleted_at: OffsetDateTime,
    pub reason_code: String,
    pub reason: Option<String>,
}

pub fn decode_deletion(event: &ProtocolEvent) -> Option<IntegratorDataDeleted> {
    if event.kind != "game_data.deleted" {
        return None;
    }
    let instance_id = event.payload.get("instance_id")?.as_str()?.to_string();
    let reason_code = event.payload.get("reason_code")?.as_str()?.to_string();
    let reason = event
        .payload
        .get("reason")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Some(IntegratorDataDeleted {
        instance_id,
        deleted_at: event.timestamp,
        reason_code,
        reason,
    })
}

/// Sets the tombstone columns on the referenced instance row — never
/// touches `instance`/`published_at`, same "lifecycle marker, not content
/// mutation" shape `apply`'s own `superseded_by` update already uses.
pub async fn apply_deletion(
    tx: &mut Transaction<'_, Postgres>,
    write: &IntegratorDataDeleted,
) -> Result<(), IndexError> {
    sqlx::query(
        "UPDATE indexer_integrator_data_instances \
         SET deleted_at = $2, delete_reason_code = $3, delete_reason = $4 \
         WHERE id = $1",
    )
    .bind(&write.instance_id)
    .bind(write.deleted_at)
    .bind(&write.reason_code)
    .bind(&write.reason)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    write: &IntegratorDataPublished,
) -> Result<(), IndexError> {
    sqlx::query(
        "INSERT INTO indexer_integrator_data_instances \
         (id, schema_id, integrator_id, subject, instance, published_at, superseded_by) \
         VALUES ($1, $2, $3, $4, $5, $6, NULL) \
         ON CONFLICT (id) DO UPDATE SET \
             schema_id = EXCLUDED.schema_id, \
             integrator_id = EXCLUDED.integrator_id, \
             subject = EXCLUDED.subject, \
             instance = EXCLUDED.instance, \
             published_at = EXCLUDED.published_at",
    )
    .bind(&write.id)
    .bind(&write.schema_id)
    .bind(write.integrator_id)
    .bind(write.subject)
    .bind(&write.instance)
    .bind(write.published_at)
    .execute(&mut **tx)
    .await?;

    if let Some(supersedes) = &write.supersedes {
        sqlx::query(
            "UPDATE indexer_integrator_data_instances SET superseded_by = $2 WHERE id = $1",
        )
        .bind(supersedes)
        .bind(&write.id)
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}

/// One stored instance, as read back for `GET /identities/{id}/integrator-data`
/// — visibility filtering is applied by the caller
/// (`crates/server/src/integrator_data.rs`), not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegratorDataInstanceRow {
    pub id: String,
    pub schema_id: String,
    pub integrator_id: Uuid,
    pub instance: serde_json::Value,
    pub published_at: OffsetDateTime,
}

/// Every *current* (non-superseded) instance belonging to `subject`,
/// across every schema/integrator that has ever published one — empty if none,
/// same "absence means nothing published" posture the rest of this
/// crate's read models use.
pub async fn list_current_for_subject(
    pool: &PgPool,
    subject: Uuid,
) -> Result<Vec<IntegratorDataInstanceRow>, IndexError> {
    let rows = sqlx::query(
        "SELECT id, schema_id, integrator_id, instance, published_at \
         FROM indexer_integrator_data_instances \
         WHERE subject = $1 AND superseded_by IS NULL AND deleted_at IS NULL \
         ORDER BY published_at",
    )
    .bind(subject)
    .fetch_all(pool)
    .await?;

    let mut instances = Vec::with_capacity(rows.len());
    for row in rows {
        instances.push(IntegratorDataInstanceRow {
            id: row.try_get("id")?,
            schema_id: row.try_get("schema_id")?,
            integrator_id: row.try_get("integrator_id")?,
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
            subject: GlobalId::new("identity", "abc", "integrator_data", "1"),
            payload,
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }
    }

    #[test]
    fn decodes_a_first_publication_with_no_supersedes() {
        let integrator_id = Uuid::new_v4();
        let subject = Uuid::new_v4();
        let source_event = event(
            "game_data.published",
            serde_json::json!({
                "id": "game:ashen-realms:schema:1:data:1",
                "schema": "game:ashen-realms:schema:1",
                "game_id": integrator_id,
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

    #[test]
    fn decodes_a_deletion() {
        let source_event = event(
            "game_data.deleted",
            serde_json::json!({
                "instance_id": "game:ashen-realms:schema:1:data:1",
                "schema": "game:ashen-realms:schema:1",
                "game_id": Uuid::new_v4(),
                "subject": Uuid::new_v4(),
                "reason_code": "deleted",
                "reason": "character deleted by player",
            }),
        );
        let write = decode_deletion(&source_event).unwrap();
        assert_eq!(write.instance_id, "game:ashen-realms:schema:1:data:1");
        assert_eq!(write.reason_code, "deleted");
        assert_eq!(
            write.reason,
            Some("character deleted by player".to_string())
        );
    }

    #[test]
    fn deletion_decode_ignores_non_deletion_kinds() {
        assert_eq!(
            decode_deletion(&event("game_data.published", serde_json::json!({}))),
            None
        );
    }

    #[test]
    fn deletion_decode_of_malformed_payload_is_none() {
        assert_eq!(
            decode_deletion(&event(
                "game_data.deleted",
                serde_json::json!({ "instance_id": "x" })
            )),
            None
        );
    }
}
