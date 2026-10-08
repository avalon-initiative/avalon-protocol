//! Issuer registrations and keys projected from `game.registered`, `issuer.key_added` and
//! `issuer.key_revoked`, scoped to the delivering network and shard, to verify revocations.

use avalon_protocol::event_payloads::{
    GameRegisteredPayload, IssuerKeyAddedPayload, IssuerKeyRevokedPayload,
};
use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::integrators::{IssuerKey, KeyPurpose, KeyRole};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::identity_proof::EventOrigin;
use crate::IndexError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRow {
    pub key_id: Uuid,
    pub algorithm: String,
    pub public_key: Vec<u8>,
    pub role: KeyRole,
    pub purpose: KeyPurpose,
    pub valid_from: OffsetDateTime,
    pub valid_until: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssuerKeyWrite {
    Registered {
        integrator_id: Uuid,
        issuer_ref: String,
        key: KeyRow,
    },
    KeyAdded {
        integrator_id: Uuid,
        key: KeyRow,
    },
    KeyRevoked {
        key_id: Uuid,
        revoked_at: OffsetDateTime,
    },
}

pub fn decode(event: &ProtocolEvent) -> Option<IssuerKeyWrite> {
    match event.kind.as_str() {
        "game.registered" => {
            let p: GameRegisteredPayload = serde_json::from_value(event.payload.clone()).ok()?;
            Some(IssuerKeyWrite::Registered {
                integrator_id: p.game_id,
                issuer_ref: format!("{}:{}", p.category, p.slug),
                key: KeyRow {
                    key_id: p.initial_key.key_id,
                    algorithm: p.initial_key.algorithm,
                    public_key: BASE64.decode(&p.initial_key.public_key).ok()?,
                    role: KeyRole::Root,
                    purpose: KeyPurpose::Attestation,
                    valid_from: event.timestamp,
                    valid_until: None,
                },
            })
        }
        "issuer.key_added" => {
            let p: IssuerKeyAddedPayload = serde_json::from_value(event.payload.clone()).ok()?;
            Some(IssuerKeyWrite::KeyAdded {
                integrator_id: p.game_id,
                key: KeyRow {
                    key_id: p.key_id,
                    algorithm: p.algorithm,
                    public_key: BASE64.decode(&p.public_key).ok()?,
                    role: KeyRole::parse(&p.role)?,
                    purpose: KeyPurpose::parse(&p.purpose)?,
                    valid_from: event.timestamp,
                    valid_until: p.valid_until,
                },
            })
        }
        "issuer.key_revoked" => {
            let p: IssuerKeyRevokedPayload = serde_json::from_value(event.payload.clone()).ok()?;
            Some(IssuerKeyWrite::KeyRevoked {
                key_id: p.key_id,
                revoked_at: p.revoked_at,
            })
        }
        _ => None,
    }
}

pub async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    origin: &EventOrigin,
    write: &IssuerKeyWrite,
) -> Result<(), IndexError> {
    match write {
        IssuerKeyWrite::Registered {
            integrator_id,
            issuer_ref,
            key,
        } => {
            let holder: Option<Uuid> = sqlx::query_scalar(
                "SELECT integrator_id FROM indexer_issuers \
                 WHERE network_id = $1 AND shard_id = $2 AND issuer_ref = $3",
            )
            .bind(&origin.network_id)
            .bind(&origin.shard_id)
            .bind(issuer_ref)
            .fetch_optional(&mut **tx)
            .await?;
            if holder.is_some_and(|h| h != *integrator_id) {
                return Err(IndexError::Rejected(
                    "issuer ref is already registered by another integrator".to_string(),
                ));
            }
            sqlx::query(
                "INSERT INTO indexer_issuers (network_id, shard_id, integrator_id, issuer_ref) \
                 VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
            )
            .bind(&origin.network_id)
            .bind(&origin.shard_id)
            .bind(integrator_id)
            .bind(issuer_ref)
            .execute(&mut **tx)
            .await?;
            insert_key(tx, origin, *integrator_id, key).await
        }
        IssuerKeyWrite::KeyAdded { integrator_id, key } => {
            insert_key(tx, origin, *integrator_id, key).await
        }
        IssuerKeyWrite::KeyRevoked { key_id, revoked_at } => {
            sqlx::query(
                "INSERT INTO indexer_issuer_key_revocations (network_id, shard_id, key_id, revoked_at) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (network_id, shard_id, key_id) \
                 DO UPDATE SET revoked_at = LEAST(indexer_issuer_key_revocations.revoked_at, EXCLUDED.revoked_at)",
            )
            .bind(&origin.network_id)
            .bind(&origin.shard_id)
            .bind(key_id)
            .bind(revoked_at)
            .execute(&mut **tx)
            .await?;
            Ok(())
        }
    }
}

/// A key id names one key: a second event with the same id and another public key is refused.
async fn insert_key(
    tx: &mut Transaction<'_, Postgres>,
    origin: &EventOrigin,
    integrator_id: Uuid,
    key: &KeyRow,
) -> Result<(), IndexError> {
    let inserted = sqlx::query(
        "INSERT INTO indexer_issuer_keys \
         (network_id, shard_id, key_id, integrator_id, algorithm, public_key, role, purpose, valid_from, valid_until) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) ON CONFLICT DO NOTHING",
    )
    .bind(&origin.network_id)
    .bind(&origin.shard_id)
    .bind(key.key_id)
    .bind(integrator_id)
    .bind(&key.algorithm)
    .bind(&key.public_key)
    .bind(key.role.as_str())
    .bind(key.purpose.as_str())
    .bind(key.valid_from)
    .bind(key.valid_until)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if inserted == 0 {
        let stored: Option<(Uuid, Vec<u8>)> = sqlx::query_as(
            "SELECT integrator_id, public_key FROM indexer_issuer_keys \
             WHERE network_id = $1 AND shard_id = $2 AND key_id = $3",
        )
        .bind(&origin.network_id)
        .bind(&origin.shard_id)
        .bind(key.key_id)
        .fetch_optional(&mut **tx)
        .await?;
        if stored.is_some_and(|(owner, public_key)| {
            owner != integrator_id || public_key != key.public_key
        }) {
            return Err(IndexError::Rejected(
                "issuer key id is already used by another key".to_string(),
            ));
        }
    }
    Ok(())
}

/// The registered integrator for `issuer_ref` on the delivering stream, if any.
pub async fn integrator_for(
    tx: &mut Transaction<'_, Postgres>,
    origin: &EventOrigin,
    issuer_ref: &str,
) -> Result<Option<Uuid>, IndexError> {
    Ok(sqlx::query_scalar(
        "SELECT integrator_id FROM indexer_issuers \
         WHERE network_id = $1 AND shard_id = $2 AND issuer_ref = $3",
    )
    .bind(&origin.network_id)
    .bind(&origin.shard_id)
    .bind(issuer_ref)
    .fetch_optional(&mut **tx)
    .await?)
}

/// Every key the integrator has registered on the delivering stream, with revocations applied.
pub async fn keys_for(
    tx: &mut Transaction<'_, Postgres>,
    origin: &EventOrigin,
    integrator_id: Uuid,
) -> Result<Vec<IssuerKey>, IndexError> {
    let rows = sqlx::query(
        "SELECT k.key_id, k.algorithm, k.public_key, k.role, k.purpose, k.valid_from, k.valid_until, \
                r.revoked_at \
         FROM indexer_issuer_keys k \
         LEFT JOIN indexer_issuer_key_revocations r \
           ON r.network_id = k.network_id AND r.shard_id = k.shard_id AND r.key_id = k.key_id \
         WHERE k.network_id = $1 AND k.shard_id = $2 AND k.integrator_id = $3",
    )
    .bind(&origin.network_id)
    .bind(&origin.shard_id)
    .bind(integrator_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut keys = Vec::with_capacity(rows.len());
    for row in &rows {
        let role: String = row.try_get("role")?;
        let purpose: String = row.try_get("purpose")?;
        let (Some(role), Some(purpose)) = (KeyRole::parse(&role), KeyPurpose::parse(&purpose))
        else {
            continue;
        };
        keys.push(IssuerKey {
            key_id: row.try_get("key_id")?,
            algorithm: row.try_get("algorithm")?,
            public_key: row.try_get("public_key")?,
            role,
            purpose,
            valid_from: row.try_get("valid_from")?,
            valid_until: row.try_get("valid_until")?,
            revoked_at: row.try_get("revoked_at")?,
        });
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use avalon_protocol::ids::GlobalId;

    use super::*;

    fn event(kind: &str, payload: serde_json::Value) -> ProtocolEvent {
        let gid = GlobalId::new("game", "ashen-realms", "self", "x");
        ProtocolEvent {
            id: Uuid::new_v4(),
            kind: kind.to_string(),
            issuer: gid.clone(),
            subject: gid,
            payload,
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
            identity_chain: None,
        }
    }

    #[test]
    fn a_registration_decodes_to_its_issuer_ref_and_root_key() {
        let (game, key) = (Uuid::new_v4(), Uuid::new_v4());
        let source = event(
            "game.registered",
            serde_json::json!({
                "game_id": game, "slug": "ashen-realms", "name": "n", "developer": "d",
                "category": "game", "requested_capabilities": [],
                "initial_key": {"key_id": key, "algorithm": "ed25519", "public_key": BASE64.encode([7u8; 32])},
            }),
        );
        let Some(IssuerKeyWrite::Registered {
            integrator_id,
            issuer_ref,
            key: row,
        }) = decode(&source)
        else {
            panic!("not decoded");
        };
        assert_eq!(integrator_id, game);
        assert_eq!(issuer_ref, "game:ashen-realms");
        assert_eq!(
            (row.key_id, row.role, row.valid_from),
            (key, KeyRole::Root, source.timestamp)
        );
    }

    #[test]
    fn a_key_with_an_unknown_role_is_not_decoded() {
        let source = event(
            "issuer.key_added",
            serde_json::json!({
                "game_id": Uuid::new_v4(), "slug": "s", "key_id": Uuid::new_v4(),
                "algorithm": "ed25519", "public_key": BASE64.encode([7u8; 32]), "role": "admin",
            }),
        );
        assert_eq!(decode(&source), None);
    }
}
