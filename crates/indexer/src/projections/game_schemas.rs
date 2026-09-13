//! The Game Registry's schema-discovery projection (issue #255, decided by
//! #181): a game's published `GameSchemaVersion`s, surfaced through the
//! same indexer-projection machinery every other read model in this crate
//! already uses, rather than a separate discovery path
//! (`docs/architecture/game-registry.md`).
//!
//! Decodes `game_schema.published`
//! (`crates/server/src/game_schemas.rs::publish_schema_version`) into an
//! upsert of the new version, plus — when the event names a `supersedes`
//! id — an update of that earlier row's `superseded_by`. Both writes are
//! natural-key upserts/updates, so replaying the same event twice
//! converges to the same state, matching every other projection's
//! idempotency requirement.

use avalon_protocol::events::ProtocolEvent;
use sqlx::{PgPool, Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::IndexError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameSchemaPublished {
    pub id: String,
    pub game_id: Uuid,
    pub version: u32,
    pub proto_source: String,
    pub published_at: OffsetDateTime,
    /// The immediately-prior version's id, if this publication supersedes
    /// one — `None` for a game's first published version.
    pub supersedes: Option<String>,
    /// `"public"`/`"private"` (#384/#381) — absent on an event emitted
    /// before #384 landed, in which case this defaults to `"public"`,
    /// preserving that publication's original fully-open behavior.
    pub default_visibility: String,
    /// Field name -> `"public"`/`"private"`; empty for a pre-#384 event.
    pub field_visibility: serde_json::Value,
}

pub fn decode(event: &ProtocolEvent) -> Option<GameSchemaPublished> {
    if event.kind != "game_schema.published" {
        return None;
    }
    let id = event.payload.get("id")?.as_str()?.to_string();
    let game_id = super::uuid_field(&event.payload, "game_id")?;
    let version = event.payload.get("version")?.as_u64()? as u32;
    let proto_source = event.payload.get("proto_source")?.as_str()?.to_string();
    let supersedes = event
        .payload
        .get("supersedes")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let default_visibility = event
        .payload
        .get("default_visibility")
        .and_then(|v| v.as_str())
        .unwrap_or("public")
        .to_string();
    let field_visibility = event
        .payload
        .get("field_visibility")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    Some(GameSchemaPublished {
        id,
        game_id,
        version,
        proto_source,
        published_at: event.timestamp,
        supersedes,
        default_visibility,
        field_visibility,
    })
}

pub async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    write: &GameSchemaPublished,
) -> Result<(), IndexError> {
    sqlx::query(
        "INSERT INTO indexer_game_schemas \
         (id, game_id, version, proto_source, published_at, superseded_by, \
          default_visibility, field_visibility) \
         VALUES ($1, $2, $3, $4, $5, NULL, $6, $7) \
         ON CONFLICT (id) DO UPDATE SET \
             game_id = EXCLUDED.game_id, \
             version = EXCLUDED.version, \
             proto_source = EXCLUDED.proto_source, \
             published_at = EXCLUDED.published_at, \
             default_visibility = EXCLUDED.default_visibility, \
             field_visibility = EXCLUDED.field_visibility",
    )
    .bind(&write.id)
    .bind(write.game_id)
    .bind(write.version as i32)
    .bind(&write.proto_source)
    .bind(write.published_at)
    .bind(&write.default_visibility)
    .bind(&write.field_visibility)
    .execute(&mut **tx)
    .await?;

    if let Some(supersedes) = &write.supersedes {
        sqlx::query("UPDATE indexer_game_schemas SET superseded_by = $2 WHERE id = $1")
            .bind(supersedes)
            .bind(&write.id)
            .execute(&mut **tx)
            .await?;
    }

    Ok(())
}

/// Visibility metadata for one schema, as recorded by the indexer's own
/// projection — what `game_data`'s read endpoint (#384) resolves per
/// instance to apply the bidirectional visibility rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaVisibility {
    pub game_id: Uuid,
    pub default_visibility: String,
    pub field_visibility: serde_json::Value,
}

/// One schema's visibility metadata by id, `None` if never published (or
/// not yet reflected in this projection).
pub async fn get_visibility(
    pool: &PgPool,
    schema_id: &str,
) -> Result<Option<SchemaVisibility>, IndexError> {
    let row = sqlx::query(
        "SELECT game_id, default_visibility, field_visibility \
         FROM indexer_game_schemas WHERE id = $1",
    )
    .bind(schema_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(SchemaVisibility {
        game_id: row.try_get("game_id")?,
        default_visibility: row.try_get("default_visibility")?,
        field_visibility: row.try_get("field_visibility")?,
    }))
}

/// One row of the registry's discovery surface — a game's published schema
/// versions, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameSchemaVersionRow {
    pub id: String,
    pub version: u32,
    pub proto_source: String,
    pub published_at: OffsetDateTime,
    pub superseded_by: Option<String>,
}

/// Every published schema version for `game_id`, oldest first — empty for
/// a game with no publications, same "absence means nothing published"
/// posture the rest of this crate's read models use rather than a
/// distinguished "not found" error.
pub async fn list_for_game(
    pool: &PgPool,
    game_id: Uuid,
) -> Result<Vec<GameSchemaVersionRow>, IndexError> {
    let rows = sqlx::query(
        "SELECT id, version, proto_source, published_at, superseded_by \
         FROM indexer_game_schemas WHERE game_id = $1 ORDER BY version",
    )
    .bind(game_id)
    .fetch_all(pool)
    .await?;

    let mut versions = Vec::with_capacity(rows.len());
    for row in rows {
        let version: i32 = row.try_get("version")?;
        versions.push(GameSchemaVersionRow {
            id: row.try_get("id")?,
            version: version as u32,
            proto_source: row.try_get("proto_source")?,
            published_at: row.try_get("published_at")?,
            superseded_by: row.try_get("superseded_by")?,
        });
    }
    Ok(versions)
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
            subject: GlobalId::new("game", "ashen-realms", "schema", "1"),
            payload,
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }
    }

    #[test]
    fn decodes_a_first_publication_with_no_supersedes() {
        let game_id = Uuid::new_v4();
        let source_event = event(
            "game_schema.published",
            serde_json::json!({
                "id": "game:ashen-realms:schema:1",
                "game_id": game_id,
                "version": 1,
                "proto_source": "message Character { uint32 level = 1; }",
                "supersedes": null,
            }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write,
            GameSchemaPublished {
                id: "game:ashen-realms:schema:1".to_string(),
                game_id,
                version: 1,
                proto_source: "message Character { uint32 level = 1; }".to_string(),
                published_at: source_event.timestamp,
                supersedes: None,
                default_visibility: "public".to_string(),
                field_visibility: serde_json::json!({}),
            }
        );
    }

    #[test]
    fn decodes_visibility_metadata_when_present() {
        let game_id = Uuid::new_v4();
        let source_event = event(
            "game_schema.published",
            serde_json::json!({
                "id": "game:ashen-realms:schema:1",
                "game_id": game_id,
                "version": 1,
                "proto_source": "message Character { uint32 level = 1; }",
                "supersedes": null,
                "default_visibility": "private",
                "field_visibility": { "level": "public" },
            }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(write.default_visibility, "private");
        assert_eq!(
            write.field_visibility,
            serde_json::json!({ "level": "public" })
        );
    }

    #[test]
    fn decoding_a_pre_384_event_defaults_to_fully_open_visibility() {
        let game_id = Uuid::new_v4();
        let source_event = event(
            "game_schema.published",
            serde_json::json!({
                "id": "game:ashen-realms:schema:1",
                "game_id": game_id,
                "version": 1,
                "proto_source": "message Character { uint32 level = 1; }",
                "supersedes": null,
            }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(write.default_visibility, "public");
        assert_eq!(write.field_visibility, serde_json::json!({}));
    }

    #[test]
    fn decodes_a_second_publication_with_supersedes_set() {
        let game_id = Uuid::new_v4();
        let source_event = event(
            "game_schema.published",
            serde_json::json!({
                "id": "game:ashen-realms:schema:2",
                "game_id": game_id,
                "version": 2,
                "proto_source": "message Character { uint32 level = 1; uint32 xp = 2; }",
                "supersedes": "game:ashen-realms:schema:1",
            }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write.supersedes.as_deref(),
            Some("game:ashen-realms:schema:1")
        );
        assert_eq!(write.version, 2);
    }

    #[test]
    fn unrecognized_kind_decodes_to_none() {
        assert_eq!(decode(&event("guild.created", serde_json::json!({}))), None);
    }

    #[test]
    fn malformed_payload_decodes_to_none() {
        assert_eq!(
            decode(&event(
                "game_schema.published",
                serde_json::json!({ "id": "game:ashen-realms:schema:1" })
            )),
            None
        );
    }
}
