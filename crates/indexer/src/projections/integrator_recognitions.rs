//! Public recognition relationships — the graph
//! `crates/server/src/recognitions.rs` writes to and reads from.
//! `integrator.recognition_published`/`.recognition_revoked` decode into
//! an upsert of the `(recognizer_id, recognized_id)` row; replaying either
//! event twice converges to the same state (natural-key upsert, same
//! idempotency shape every other projection in this crate uses).

use avalon_protocol::events::ProtocolEvent;
use sqlx::{PgPool, Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::IndexError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecognitionWrite {
    Published {
        recognizer_id: Uuid,
        recognized_id: Uuid,
        scope: Vec<String>,
        published_at: OffsetDateTime,
    },
    Revoked {
        recognizer_id: Uuid,
        recognized_id: Uuid,
        revoked_at: OffsetDateTime,
    },
}

pub fn decode(event: &ProtocolEvent) -> Option<RecognitionWrite> {
    let recognizer_id = super::uuid_field(&event.payload, "recognizer_id")?;
    let recognized_id = super::uuid_field(&event.payload, "recognized_id")?;
    match event.kind.as_str() {
        "integrator.recognition_published" => {
            let scope: Vec<String> = event
                .payload
                .get("scope")?
                .as_array()?
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            Some(RecognitionWrite::Published {
                recognizer_id,
                recognized_id,
                scope,
                published_at: event.timestamp,
            })
        }
        "integrator.recognition_revoked" => Some(RecognitionWrite::Revoked {
            recognizer_id,
            recognized_id,
            revoked_at: event.timestamp,
        }),
        _ => None,
    }
}

pub async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    write: &RecognitionWrite,
) -> Result<(), IndexError> {
    match write {
        RecognitionWrite::Published {
            recognizer_id,
            recognized_id,
            scope,
            published_at,
        } => {
            sqlx::query(
                "INSERT INTO indexer_integrator_recognitions \
                 (recognizer_id, recognized_id, scope, published_at, revoked_at) \
                 VALUES ($1, $2, $3, $4, NULL) \
                 ON CONFLICT (recognizer_id, recognized_id) DO UPDATE SET \
                     scope = EXCLUDED.scope, published_at = EXCLUDED.published_at, revoked_at = NULL",
            )
            .bind(recognizer_id)
            .bind(recognized_id)
            .bind(scope)
            .bind(published_at)
            .execute(&mut **tx)
            .await?;
        }
        RecognitionWrite::Revoked {
            recognizer_id,
            recognized_id,
            revoked_at,
        } => {
            sqlx::query(
                "UPDATE indexer_integrator_recognitions SET revoked_at = $3 \
                 WHERE recognizer_id = $1 AND recognized_id = $2",
            )
            .bind(recognizer_id)
            .bind(recognized_id)
            .bind(revoked_at)
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecognitionRow {
    pub recognizer_id: Uuid,
    pub recognized_id: Uuid,
    pub scope: Vec<String>,
    pub published_at: OffsetDateTime,
}

/// Every integrator `recognizer_id` currently, actively recognizes.
pub async fn list_recognized_by(
    pool: &PgPool,
    recognizer_id: Uuid,
) -> Result<Vec<RecognitionRow>, IndexError> {
    let rows = sqlx::query(
        "SELECT recognizer_id, recognized_id, scope, published_at \
         FROM indexer_integrator_recognitions \
         WHERE recognizer_id = $1 AND revoked_at IS NULL ORDER BY published_at",
    )
    .bind(recognizer_id)
    .fetch_all(pool)
    .await?;
    rows_to_recognitions(rows)
}

/// Every integrator that currently, actively recognizes `recognized_id`.
pub async fn list_recognizers_of(
    pool: &PgPool,
    recognized_id: Uuid,
) -> Result<Vec<RecognitionRow>, IndexError> {
    let rows = sqlx::query(
        "SELECT recognizer_id, recognized_id, scope, published_at \
         FROM indexer_integrator_recognitions \
         WHERE recognized_id = $1 AND revoked_at IS NULL ORDER BY published_at",
    )
    .bind(recognized_id)
    .fetch_all(pool)
    .await?;
    rows_to_recognitions(rows)
}

fn rows_to_recognitions(
    rows: Vec<sqlx::postgres::PgRow>,
) -> Result<Vec<RecognitionRow>, IndexError> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(RecognitionRow {
            recognizer_id: row.try_get("recognizer_id")?,
            recognized_id: row.try_get("recognized_id")?,
            scope: row.try_get("scope")?,
            published_at: row.try_get("published_at")?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use avalon_protocol::ids::GlobalId;

    use super::*;

    fn event(kind: &str, payload: serde_json::Value) -> ProtocolEvent {
        ProtocolEvent {
            id: Uuid::new_v4(),
            kind: kind.to_string(),
            issuer: GlobalId::new("game", "a", "self", "x"),
            subject: GlobalId::new("game", "b", "self", "x"),
            payload,
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
            identity_chain: None,
        }
    }

    #[test]
    fn decodes_a_published_event() {
        let recognizer_id = Uuid::new_v4();
        let recognized_id = Uuid::new_v4();
        let source_event = event(
            "integrator.recognition_published",
            serde_json::json!({
                "recognizer_id": recognizer_id,
                "recognized_id": recognized_id,
                "scope": ["achievements", "tournament_results"],
            }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write,
            RecognitionWrite::Published {
                recognizer_id,
                recognized_id,
                scope: vec!["achievements".to_string(), "tournament_results".to_string()],
                published_at: source_event.timestamp,
            }
        );
    }

    #[test]
    fn decodes_a_revoked_event() {
        let recognizer_id = Uuid::new_v4();
        let recognized_id = Uuid::new_v4();
        let source_event = event(
            "integrator.recognition_revoked",
            serde_json::json!({
                "recognizer_id": recognizer_id,
                "recognized_id": recognized_id,
            }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write,
            RecognitionWrite::Revoked {
                recognizer_id,
                recognized_id,
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
                "integrator.recognition_published",
                serde_json::json!({ "recognizer_id": Uuid::new_v4() })
            )),
            None
        );
    }
}
