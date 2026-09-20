//! Durable event-signing keys (issue #525, surfaced while implementing
//! Part 2 of #521's decision) — `identity.signing_key_added`/
//! `.signing_key_revoked` decode into an upsert of
//! `indexer_identity_signing_keys`. Same shape and rationale as
//! `identity_passkeys`: a live node keeps writing `identity_signing_keys`
//! directly (`crates/server/src/handlers.rs`, `crates/server/src/devices.rs`),
//! but a mirror-only node reconstructs a verification-ready table from
//! replayed history alone via this projection. This is what
//! `crate::continuation` (session-continuation token verification) reads
//! from, on *any* node — including one that authored the key locally,
//! since that node's own writes are dual-written here too (same pattern
//! `recognitions.rs` already uses).

use avalon_protocol::events::ProtocolEvent;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::IndexError;

#[derive(Debug, Clone, PartialEq)]
pub enum SigningKeyWrite {
    Added {
        signing_key_id: Uuid,
        identity_id: Uuid,
        public_key: Vec<u8>,
        label: Option<String>,
        added_at: OffsetDateTime,
    },
    Revoked {
        signing_key_id: Uuid,
        revoked_at: OffsetDateTime,
    },
}

pub fn decode(event: &ProtocolEvent) -> Option<SigningKeyWrite> {
    match event.kind.as_str() {
        "identity.signing_key_added" => {
            let signing_key_id = super::uuid_field(&event.payload, "signing_key_id")?;
            let identity_id = super::uuid_field(&event.payload, "identity_id")?;
            let public_key_b64 = event.payload.get("public_key")?.as_str()?;
            let public_key =
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, public_key_b64)
                    .ok()?;
            let label = event
                .payload
                .get("device_label")
                .and_then(|v| v.as_str().map(str::to_string));
            Some(SigningKeyWrite::Added {
                signing_key_id,
                identity_id,
                public_key,
                label,
                added_at: event.timestamp,
            })
        }
        "identity.signing_key_revoked" => {
            let signing_key_id = super::uuid_field(&event.payload, "signing_key_id")?;
            Some(SigningKeyWrite::Revoked {
                signing_key_id,
                revoked_at: event.timestamp,
            })
        }
        _ => None,
    }
}

pub async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    write: &SigningKeyWrite,
) -> Result<(), IndexError> {
    match write {
        SigningKeyWrite::Added {
            signing_key_id,
            identity_id,
            public_key,
            label,
            added_at,
        } => {
            sqlx::query(
                "INSERT INTO indexer_identity_signing_keys \
                 (signing_key_id, identity_id, public_key, label, added_at, revoked_at) \
                 VALUES ($1, $2, $3, $4, $5, NULL) \
                 ON CONFLICT (signing_key_id) DO UPDATE SET \
                     identity_id = EXCLUDED.identity_id, public_key = EXCLUDED.public_key, \
                     label = EXCLUDED.label, added_at = EXCLUDED.added_at",
            )
            .bind(signing_key_id)
            .bind(identity_id)
            .bind(public_key)
            .bind(label)
            .bind(added_at)
            .execute(&mut **tx)
            .await?;
        }
        SigningKeyWrite::Revoked {
            signing_key_id,
            revoked_at,
        } => {
            sqlx::query(
                "UPDATE indexer_identity_signing_keys SET revoked_at = $2 WHERE signing_key_id = $1",
            )
            .bind(signing_key_id)
            .bind(revoked_at)
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct SigningKeyRow {
    pub signing_key_id: Uuid,
    pub identity_id: Uuid,
    pub public_key: Vec<u8>,
}

/// The active (not revoked) signing key `signing_key_id`, if any — what
/// `crate::continuation`-equivalent verification (in `avalon-server`) needs
/// to check a self-signed continuation assertion against, on any node,
/// authoring or mirror-only alike. `None` for an unknown or revoked key —
/// deliberately not distinguished further, same "don't help an attacker
/// enumerate" posture `handlers::authenticate`'s own doc comment states for
/// session tokens.
pub async fn find_active_by_id(
    pool: &sqlx::PgPool,
    signing_key_id: Uuid,
) -> Result<Option<SigningKeyRow>, IndexError> {
    let row = sqlx::query(
        "SELECT signing_key_id, identity_id, public_key FROM indexer_identity_signing_keys \
         WHERE signing_key_id = $1 AND revoked_at IS NULL",
    )
    .bind(signing_key_id)
    .fetch_optional(pool)
    .await?;
    row.map(signing_key_row_from_row).transpose()
}

/// Every distinct `identity_id` this node has at least one signing-key row
/// for (active or revoked) — issue #635's identity locator uses this to
/// know which identities to keep registering DHT interest for. Includes
/// identities whose only key is revoked (their history is still real and
/// still worth locating), so this is "identities this node knows about,"
/// not "identities this node can currently authenticate."
pub async fn distinct_identity_ids(pool: &sqlx::PgPool) -> Result<Vec<Uuid>, IndexError> {
    let rows = sqlx::query("SELECT DISTINCT identity_id FROM indexer_identity_signing_keys")
        .fetch_all(pool)
        .await?;
    rows.into_iter()
        .map(|row| row.try_get("identity_id").map_err(IndexError::from))
        .collect()
}

fn signing_key_row_from_row(row: sqlx::postgres::PgRow) -> Result<SigningKeyRow, IndexError> {
    Ok(SigningKeyRow {
        signing_key_id: row.try_get("signing_key_id")?,
        identity_id: row.try_get("identity_id")?,
        public_key: row.try_get("public_key")?,
    })
}

#[cfg(test)]
mod tests {
    use avalon_protocol::ids::GlobalId;

    use super::*;

    fn event(kind: &str, issuer_identity_id: Uuid, payload: serde_json::Value) -> ProtocolEvent {
        ProtocolEvent {
            id: Uuid::new_v4(),
            kind: kind.to_string(),
            issuer: GlobalId::new(
                "identity",
                &issuer_identity_id.to_string(),
                "self",
                "signing_key_added",
            ),
            subject: GlobalId::new(
                "identity",
                &issuer_identity_id.to_string(),
                "self",
                "signing_key_added",
            ),
            payload,
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }
    }

    #[test]
    fn decodes_an_added_event() {
        let signing_key_id = Uuid::new_v4();
        let identity_id = Uuid::new_v4();
        let source_event = event(
            "identity.signing_key_added",
            identity_id,
            serde_json::json!({
                "signing_key_id": signing_key_id,
                "public_key": "AAAA",
                "device_label": "Pixel 9",
                "approved_by_signing_key_id": signing_key_id,
                "identity_id": identity_id,
            }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write,
            SigningKeyWrite::Added {
                signing_key_id,
                identity_id,
                public_key: vec![0, 0, 0],
                label: Some("Pixel 9".to_string()),
                added_at: source_event.timestamp,
            }
        );
    }

    #[test]
    fn decodes_a_revoked_event() {
        let signing_key_id = Uuid::new_v4();
        let source_event = event(
            "identity.signing_key_revoked",
            Uuid::new_v4(),
            serde_json::json!({ "signing_key_id": signing_key_id }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write,
            SigningKeyWrite::Revoked {
                signing_key_id,
                revoked_at: source_event.timestamp,
            }
        );
    }

    #[test]
    fn unrecognized_kind_decodes_to_none() {
        assert_eq!(
            decode(&event(
                "guild.created",
                Uuid::new_v4(),
                serde_json::json!({})
            )),
            None
        );
    }

    #[test]
    fn malformed_public_key_decodes_to_none() {
        assert_eq!(
            decode(&event(
                "identity.signing_key_added",
                Uuid::new_v4(),
                serde_json::json!({
                    "signing_key_id": Uuid::new_v4(),
                    "public_key": "not valid base64!!",
                }),
            )),
            None
        );
    }
}
