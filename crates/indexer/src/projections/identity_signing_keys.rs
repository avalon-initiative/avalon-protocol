//! Durable event-signing keys — `identity.signing_key_added`/
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

use avalon_protocol::event_payloads::IdentitySigningKeyAddedPayload;
use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::identity_id::IdentityId;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::IndexError;

#[derive(Debug, Clone, PartialEq)]
pub enum SigningKeyWrite {
    Added {
        signing_key_id: Uuid,
        identity_id: IdentityId,
        public_key: Vec<u8>,
        label: Option<String>,
        added_at: OffsetDateTime,
    },
    Revoked {
        signing_key_id: Uuid,
        identity_id: IdentityId,
        revoked_at: OffsetDateTime,
    },
}

pub fn decode(event: &ProtocolEvent) -> Option<SigningKeyWrite> {
    match event.kind.as_str() {
        "identity.signing_key_added" => {
            if event.version != 2 {
                return None;
            }
            let added: IdentitySigningKeyAddedPayload =
                serde_json::from_value(event.payload.clone()).ok()?;
            let public_key = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                &added.public_key,
            )
            .ok()?;
            if added.kind == avalon_protocol::event_payloads::SIGNING_KEY_KIND_INCEPTION {
                let key: [u8; 32] = public_key.as_slice().try_into().ok()?;
                avalon_protocol::ed25519_key::parse_ed25519_public_key(&key)?;
                if !added.identity_id.matches_key(&key) {
                    return None;
                }
            }
            Some(SigningKeyWrite::Added {
                signing_key_id: added.signing_key_id,
                identity_id: added.identity_id,
                public_key,
                label: added.device_label,
                added_at: event.timestamp,
            })
        }
        "identity.signing_key_revoked" => {
            if event.version != 2 {
                return None;
            }
            let revoked: avalon_protocol::event_payloads::IdentitySigningKeyRevokedPayload =
                serde_json::from_value(event.payload.clone()).ok()?;
            Some(SigningKeyWrite::Revoked {
                signing_key_id: revoked.signing_key_id,
                identity_id: revoked.identity_id,
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
            // An existing key row keeps its identity and public key; a conflicting event is refused.
            let written = sqlx::query(
                "INSERT INTO indexer_identity_signing_keys AS k \
                 (signing_key_id, identity_id, public_key, label, added_at, revoked_at) \
                 VALUES ($1, $2, $3, $4, $5, NULL) \
                 ON CONFLICT (signing_key_id) DO UPDATE SET \
                     label = EXCLUDED.label, added_at = EXCLUDED.added_at \
                 WHERE k.identity_id = EXCLUDED.identity_id AND k.public_key = EXCLUDED.public_key",
            )
            .bind(signing_key_id)
            .bind(identity_id)
            .bind(public_key)
            .bind(label)
            .bind(added_at)
            .execute(&mut **tx)
            .await?;
            if written.rows_affected() == 0 {
                return Err(IndexError::Rejected(format!(
                    "signing key {signing_key_id} already belongs to a different identity or key"
                )));
            }
        }
        SigningKeyWrite::Revoked {
            signing_key_id,
            identity_id,
            revoked_at,
        } => {
            sqlx::query(
                "UPDATE indexer_identity_signing_keys SET revoked_at = $2 \
                 WHERE signing_key_id = $1 AND identity_id = $3",
            )
            .bind(signing_key_id)
            .bind(revoked_at)
            .bind(identity_id)
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct SigningKeyRow {
    pub signing_key_id: Uuid,
    pub identity_id: IdentityId,
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
pub async fn distinct_identity_ids(pool: &sqlx::PgPool) -> Result<Vec<IdentityId>, IndexError> {
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

    fn event(
        kind: &str,
        issuer_identity_id: IdentityId,
        payload: serde_json::Value,
    ) -> ProtocolEvent {
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
            version: 2,
            identity_chain: None,
        }
    }

    #[test]
    fn decodes_an_added_event() {
        let signing_key_id = Uuid::new_v4();
        let identity_id = IdentityId::random_for_tests();
        let source_event = event(
            "identity.signing_key_added",
            identity_id,
            serde_json::json!({
                "signing_key_id": signing_key_id,
                "public_key": "AAAA",
                "device_label": "Pixel 9",
                "approved_by_signing_key_id": signing_key_id,
                "identity_id": identity_id,
                "kind": "device_grant",
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
        let identity_id = IdentityId::random_for_tests();
        let source_event = event(
            "identity.signing_key_revoked",
            identity_id,
            serde_json::json!({
                "identity_id": identity_id,
                "signing_key_id": signing_key_id,
                "revoked_by_signing_key_id": signing_key_id,
                "signature": "c2ln",
            }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write,
            SigningKeyWrite::Revoked {
                signing_key_id,
                identity_id,
                revoked_at: source_event.timestamp,
            }
        );
    }

    #[test]
    fn an_inception_key_must_hash_to_the_identity_id() {
        use base64::Engine as _;
        let who = avalon_protocol::identity_id::TestIdentity::new();
        let other = avalon_protocol::identity_id::TestIdentity::new();
        let payload = |identity: IdentityId, key: [u8; 32]| {
            serde_json::json!({
                "signing_key_id": Uuid::new_v4(),
                "public_key": base64::engine::general_purpose::STANDARD.encode(key),
                "device_label": null,
                "approved_by_signing_key_id": Uuid::new_v4(),
                "identity_id": identity,
                "kind": "inception",
            })
        };
        let good = event(
            "identity.signing_key_added",
            who.id,
            payload(who.id, who.public_key()),
        );
        assert!(decode(&good).is_some());
        let forged = event(
            "identity.signing_key_added",
            who.id,
            payload(who.id, other.public_key()),
        );
        assert_eq!(decode(&forged), None);
    }

    #[test]
    fn v1_added_event_is_rejected() {
        let id = IdentityId::random_for_tests();
        let mut e = event(
            "identity.signing_key_added",
            id,
            serde_json::json!({
                "signing_key_id": Uuid::new_v4(),
                "public_key": "AAAA",
                "device_label": null,
                "approved_by_signing_key_id": Uuid::new_v4(),
                "identity_id": id,
                "kind": "device_grant",
            }),
        );
        assert!(decode(&e).is_some());
        e.version = 1;
        assert_eq!(decode(&e), None);
    }

    #[test]
    fn unrecognized_kind_decodes_to_none() {
        assert_eq!(
            decode(&event(
                "guild.created",
                IdentityId::random_for_tests(),
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
                IdentityId::random_for_tests(),
                serde_json::json!({
                    "signing_key_id": Uuid::new_v4(),
                    "public_key": "not valid base64!!",
                }),
            )),
            None
        );
    }
}
