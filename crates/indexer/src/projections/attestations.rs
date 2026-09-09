//! The attestation-status cache, built from `achievement.issued` /
//! `achievement.revoked` — see `docs/architecture/achievements-and-attestations.md`
//! and `avalon_protocol::achievements::AchievementAttestation`, whose shape
//! this projection's payload expectations mirror.
//!
//! Nothing in this repo emits either event kind yet: Epic #30
//! (Achievements & Attestations) is still scaffolding, same as this crate
//! was before this ticket. This projection exists so #30's future issuing
//! flow has a read model ready to consume it on day one, matching the
//! ticket's design (issue #42), and so its own idempotency/decode behavior
//! is provable now via fixture events rather than only once a real issuer
//! exists. `revoked_at` is a cache of the latest relevant event, per
//! `docs/architecture/query-and-indexing.md`'s "current status is a cache"
//! rule — the event log remains the record of *when* and *why*.

use avalon_protocol::events::ProtocolEvent;
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::IndexError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttestationWrite {
    Issue {
        id: Uuid,
        issuer: String,
        subject: Uuid,
        achievement: String,
        issued_at: OffsetDateTime,
    },
    Revoke {
        id: Uuid,
        revoked_at: OffsetDateTime,
    },
}

pub fn decode(event: &ProtocolEvent) -> Option<AttestationWrite> {
    match event.kind.as_str() {
        "achievement.issued" => {
            let id = super::uuid_field(&event.payload, "id")?;
            let issuer = event.payload.get("issuer")?.as_str()?.to_string();
            let subject = super::uuid_field(&event.payload, "subject")?;
            let achievement = event.payload.get("achievement")?.as_str()?.to_string();
            Some(AttestationWrite::Issue {
                id,
                issuer,
                subject,
                achievement,
                issued_at: event.timestamp,
            })
        }
        "achievement.revoked" => {
            let id = super::uuid_field(&event.payload, "id")?;
            Some(AttestationWrite::Revoke {
                id,
                revoked_at: event.timestamp,
            })
        }
        _ => None,
    }
}

pub async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    write: &AttestationWrite,
) -> Result<(), IndexError> {
    match write {
        AttestationWrite::Issue {
            id,
            issuer,
            subject,
            achievement,
            issued_at,
        } => {
            sqlx::query(
                "INSERT INTO indexer_attestations (id, issuer, subject, achievement, issued_at) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (id) DO UPDATE SET \
                     issuer = EXCLUDED.issuer, \
                     subject = EXCLUDED.subject, \
                     achievement = EXCLUDED.achievement",
            )
            .bind(id)
            .bind(issuer)
            .bind(subject)
            .bind(achievement)
            .bind(issued_at)
            .execute(&mut **tx)
            .await?;
        }
        AttestationWrite::Revoke { id, revoked_at } => {
            sqlx::query("UPDATE indexer_attestations SET revoked_at = $2 WHERE id = $1")
                .bind(id)
                .bind(revoked_at)
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
            issuer: GlobalId::new("game", &Uuid::new_v4().to_string(), "self", "x"),
            subject: GlobalId::new("identity", &Uuid::new_v4().to_string(), "self", "x"),
            payload,
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }
    }

    #[test]
    fn decodes_achievement_issued() {
        let id = Uuid::new_v4();
        let subject = Uuid::new_v4();
        let source_event = event(
            "achievement.issued",
            serde_json::json!({
                "id": id,
                "issuer": "game:ashen-realms",
                "subject": subject,
                "achievement": "game:ashen-realms:achievement:dragon_slayer",
            }),
        );
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write,
            AttestationWrite::Issue {
                id,
                issuer: "game:ashen-realms".to_string(),
                subject,
                achievement: "game:ashen-realms:achievement:dragon_slayer".to_string(),
                issued_at: source_event.timestamp,
            }
        );
    }

    #[test]
    fn decodes_achievement_revoked() {
        let id = Uuid::new_v4();
        let source_event = event("achievement.revoked", serde_json::json!({ "id": id }));
        let write = decode(&source_event).unwrap();
        assert_eq!(
            write,
            AttestationWrite::Revoke {
                id,
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
                "achievement.issued",
                serde_json::json!({ "id": "nope" })
            )),
            None
        );
    }
}
