//! Durable passkey credentials (issue #523, Part 1 of #521's decision) —
//! `identity.passkey_registered`/`.passkey_revoked` decode into an upsert
//! of `indexer_identity_passkeys`. This projection exists specifically for
//! a mirror-only node: a live node that actually ran the WebAuthn ceremony
//! keeps writing `identity_keys` directly (`crates/server/src/passkeys.rs`,
//! `crates/server/src/handlers.rs`), same as before this ticket. See
//! `docs/architecture/identity.md`'s durability table.

use avalon_protocol::events::ProtocolEvent;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::IndexError;

#[derive(Debug, Clone, PartialEq)]
pub enum PasskeyWrite {
    Registered {
        passkey_id: Uuid,
        identity_id: Uuid,
        credential_id: Vec<u8>,
        passkey_data: serde_json::Value,
        label: Option<String>,
        added_at: OffsetDateTime,
    },
    Revoked {
        passkey_id: Uuid,
        revoked_at: OffsetDateTime,
    },
}

pub fn decode(event: &ProtocolEvent) -> Option<PasskeyWrite> {
    match event.kind.as_str() {
        "identity.passkey_registered" => {
            let passkey_id = super::uuid_field(&event.payload, "passkey_id")?;
            let identity_id = super::uuid_field(&event.payload, "identity_id")?;
            let credential_id_b64 = event.payload.get("credential_id")?.as_str()?;
            let credential_id = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                credential_id_b64,
            )
            .ok()?;
            let passkey_data = event.payload.get("passkey_data")?.clone();
            let label = event
                .payload
                .get("label")
                .and_then(|v| v.as_str().map(str::to_string));
            Some(PasskeyWrite::Registered {
                passkey_id,
                identity_id,
                credential_id,
                passkey_data,
                label,
                added_at: event.timestamp,
            })
        }
        "identity.passkey_revoked" => {
            let passkey_id = super::uuid_field(&event.payload, "passkey_id")?;
            Some(PasskeyWrite::Revoked {
                passkey_id,
                revoked_at: event.timestamp,
            })
        }
        _ => None,
    }
}

pub async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    write: &PasskeyWrite,
) -> Result<(), IndexError> {
    match write {
        PasskeyWrite::Registered {
            passkey_id,
            identity_id,
            credential_id,
            passkey_data,
            label,
            added_at,
        } => {
            sqlx::query(
                "INSERT INTO indexer_identity_passkeys \
                 (passkey_id, identity_id, credential_id, passkey_data, label, added_at, revoked_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, NULL) \
                 ON CONFLICT (passkey_id) DO UPDATE SET \
                     identity_id = EXCLUDED.identity_id, credential_id = EXCLUDED.credential_id, \
                     passkey_data = EXCLUDED.passkey_data, label = EXCLUDED.label, \
                     added_at = EXCLUDED.added_at",
            )
            .bind(passkey_id)
            .bind(identity_id)
            .bind(credential_id)
            .bind(passkey_data)
            .bind(label)
            .bind(added_at)
            .execute(&mut **tx)
            .await?;
        }
        PasskeyWrite::Revoked {
            passkey_id,
            revoked_at,
        } => {
            sqlx::query(
                "UPDATE indexer_identity_passkeys SET revoked_at = $2 WHERE passkey_id = $1",
            )
            .bind(passkey_id)
            .bind(revoked_at)
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct PasskeyRow {
    pub passkey_id: Uuid,
    pub identity_id: Uuid,
    pub credential_id: Vec<u8>,
    pub passkey_data: serde_json::Value,
    pub label: Option<String>,
    pub added_at: OffsetDateTime,
}

/// The active (not revoked) mirrored passkey for `credential_id`, if any —
/// what a mirror-only node needs to verify a fresh WebAuthn login for a
/// credential it only ever learned about via replayed history. `None` for
/// a credential this node has never mirrored, or one it has mirrored a
/// revocation for.
pub async fn find_active_by_credential_id(
    pool: &sqlx::PgPool,
    credential_id: &[u8],
) -> Result<Option<PasskeyRow>, IndexError> {
    let row = sqlx::query(
        "SELECT passkey_id, identity_id, credential_id, passkey_data, label, added_at \
         FROM indexer_identity_passkeys WHERE credential_id = $1 AND revoked_at IS NULL",
    )
    .bind(credential_id)
    .fetch_optional(pool)
    .await?;
    row.map(passkey_row_from_row).transpose()
}

fn passkey_row_from_row(row: sqlx::postgres::PgRow) -> Result<PasskeyRow, IndexError> {
    Ok(PasskeyRow {
        passkey_id: row.try_get("passkey_id")?,
        identity_id: row.try_get("identity_id")?,
        credential_id: row.try_get("credential_id")?,
        passkey_data: row.try_get("passkey_data")?,
        label: row.try_get("label")?,
        added_at: row.try_get("added_at")?,
    })
}

#[cfg(test)]
mod tests {
    use avalon_protocol::ids::GlobalId;

    use super::*;

    fn event(kind: &str, payload: serde_json::Value) -> ProtocolEvent {
        ProtocolEvent {
            id: Uuid::new_v4(),
            kind: kind.to_string(),
            issuer: GlobalId::new("identity", "a", "self", "x"),
            subject: GlobalId::new("identity", "a", "self", "x"),
            payload,
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }
    }

    #[test]
    fn decodes_a_registered_event() {
        let passkey_id = Uuid::new_v4();
        let identity_id = Uuid::new_v4();
        let source_event = event(
            "identity.passkey_registered",
            serde_json::json!({
                "passkey_id": passkey_id,
                "identity_id": identity_id,
                "credential_id": "AAAA",
                "passkey_data": {"cred": "data"},
                "label": "Work laptop",
            }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write,
            PasskeyWrite::Registered {
                passkey_id,
                identity_id,
                credential_id: vec![0, 0, 0],
                passkey_data: serde_json::json!({"cred": "data"}),
                label: Some("Work laptop".to_string()),
                added_at: source_event.timestamp,
            }
        );
    }

    #[test]
    fn decodes_a_registered_event_with_no_label() {
        let passkey_id = Uuid::new_v4();
        let identity_id = Uuid::new_v4();
        let source_event = event(
            "identity.passkey_registered",
            serde_json::json!({
                "passkey_id": passkey_id,
                "identity_id": identity_id,
                "credential_id": "AAAA",
                "passkey_data": {"cred": "data"},
                "label": null,
            }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write,
            PasskeyWrite::Registered {
                passkey_id,
                identity_id,
                credential_id: vec![0, 0, 0],
                passkey_data: serde_json::json!({"cred": "data"}),
                label: None,
                added_at: source_event.timestamp,
            }
        );
    }

    #[test]
    fn decodes_a_revoked_event() {
        let passkey_id = Uuid::new_v4();
        let source_event = event(
            "identity.passkey_revoked",
            serde_json::json!({
                "passkey_id": passkey_id,
                "identity_id": Uuid::new_v4(),
            }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write,
            PasskeyWrite::Revoked {
                passkey_id,
                revoked_at: source_event.timestamp,
            }
        );
    }

    #[test]
    fn unrecognized_kind_decodes_to_none() {
        assert_eq!(decode(&event("guild.created", serde_json::json!({}))), None);
    }

    #[test]
    fn malformed_credential_id_decodes_to_none() {
        assert_eq!(
            decode(&event(
                "identity.passkey_registered",
                serde_json::json!({
                    "passkey_id": Uuid::new_v4(),
                    "identity_id": Uuid::new_v4(),
                    "credential_id": "not valid base64!!",
                    "passkey_data": {},
                }),
            )),
            None
        );
    }
}
