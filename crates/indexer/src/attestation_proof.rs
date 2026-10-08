//! Projection-time proof that an attestation revocation was signed by the issuer, for the network of
//! the delivering stream. A missing or wrong signature is refused before any table is touched.

use avalon_protocol::achievements::{revocation_signing_bytes, AttestationSigner};
use avalon_protocol::ed25519_key::{parse_ed25519_public_key, verify_strict_signature};
use avalon_protocol::event_payloads::ClaimRevokedPayload;
use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::ids::AttestationId;
use avalon_protocol::integrators::{resolve_valid_signing_key, IssuerKey};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use sqlx::{Postgres, Transaction};

use crate::identity_proof::EventOrigin;
use crate::projections::issuer_keys;
use crate::IndexError;

fn reject(reason: impl Into<String>) -> IndexError {
    IndexError::Rejected(reason.into())
}

/// The claim kind a revocation event kind is signed under, if it is one.
fn claim_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "achievement.revoked" => Some("achievement"),
        "milestone.revoked" => Some("milestone"),
        _ => None,
    }
}

/// Whether `issuer_ref` names an issuer category that issues `claim_kind` claims.
fn issuer_matches_kind(issuer_ref: &str, claim_kind: &str) -> bool {
    match claim_kind {
        "achievement" => issuer_ref.starts_with("game:"),
        _ => issuer_ref.starts_with("app:") || issuer_ref.starts_with("service:"),
    }
}

/// Checks the revocation's bytes for `network_id` against `keys` valid at `at`. A key id outside
/// `keys` waits ([`IndexError::AwaitingKey`]); every other failure is a refusal.
pub fn verify_signed_revocation(
    network_id: &str,
    claim_kind: &str,
    revoked: &ClaimRevokedPayload,
    keys: &[IssuerKey],
    at: time::OffsetDateTime,
) -> Result<(), IndexError> {
    if revoked.proof.algorithm != "ed25519" {
        return Err(reject("revocation proof algorithm is not ed25519"));
    }
    let signature: [u8; 64] = BASE64
        .decode(&revoked.proof.bytes)
        .ok()
        .and_then(|raw| raw.try_into().ok())
        .ok_or_else(|| reject("revocation signature is not 64 bytes of base64"))?;
    if !keys.iter().any(|k| k.key_id == revoked.proof.key_id) {
        return Err(IndexError::AwaitingKey(
            "the revocation's signing key is not projected for this issuer yet".to_string(),
        ));
    }
    let key = resolve_valid_signing_key(keys, revoked.proof.key_id, at)
        .ok_or_else(|| reject("revocation signing key is not valid for the issuer at this time"))?;
    let public_key: [u8; 32] = key
        .public_key
        .as_slice()
        .try_into()
        .map_err(|_| reject("issuer key is malformed"))?;
    let public_key =
        parse_ed25519_public_key(&public_key).ok_or_else(|| reject("issuer key is malformed"))?;
    let signer = AttestationSigner {
        network_id,
        claim_kind,
        issuer_ref: &revoked.issuer,
        signing_key_id: revoked.proof.key_id,
    };
    let bytes = revocation_signing_bytes(
        &signer,
        AttestationId(revoked.attestation_id),
        revoked.reason_code.as_str(),
        &revoked.reason,
    );
    if verify_strict_signature(&public_key, &bytes, &signature) {
        Ok(())
    } else {
        Err(reject("revocation signature does not verify"))
    }
}

/// Verifies an `achievement.revoked` / `milestone.revoked` event; any other kind passes.
pub async fn verify_revocation(
    tx: &mut Transaction<'_, Postgres>,
    event: &ProtocolEvent,
    origin: Option<&EventOrigin>,
) -> Result<(), IndexError> {
    let Some(claim_kind) = claim_kind(&event.kind) else {
        return Ok(());
    };
    let origin = origin.ok_or_else(|| reject("no origin to verify a revocation"))?;
    let revoked: ClaimRevokedPayload = serde_json::from_value(event.payload.clone())
        .map_err(|_| reject("revocation payload is malformed or carries no proof"))?;
    if !issuer_matches_kind(&revoked.issuer, claim_kind) {
        return Err(reject("revocation issuer does not issue this claim kind"));
    }
    let integrator_id = issuer_keys::integrator_for(tx, origin, &revoked.issuer)
        .await?
        .ok_or_else(|| {
            IndexError::AwaitingKey(
                "the revoking issuer is not registered on this stream yet".to_string(),
            )
        })?;
    let keys = issuer_keys::keys_for(tx, origin, integrator_id).await?;
    verify_signed_revocation(
        &origin.network_id,
        claim_kind,
        &revoked,
        &keys,
        event.timestamp,
    )?;
    let issued_by: Option<String> =
        sqlx::query_scalar("SELECT issuer FROM indexer_attestations WHERE id = $1")
            .bind(revoked.attestation_id)
            .fetch_optional(&mut **tx)
            .await?;
    if issued_by.is_some_and(|issuer| issuer != revoked.issuer) {
        return Err(reject(
            "revocation is signed by an issuer other than the attestation's",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use avalon_protocol::event_payloads::ClaimProofPayload;
    use avalon_protocol::integrators::{KeyPurpose, KeyRole};
    use avalon_protocol::revocation::RevocationReasonCode;
    use ed25519_dalek::{Signer, SigningKey};
    use time::OffsetDateTime;
    use uuid::Uuid;

    use super::*;

    const NET: &str = "avalon-dev-test";

    fn key(signing: &SigningKey, key_id: Uuid, from: OffsetDateTime) -> IssuerKey {
        IssuerKey {
            key_id,
            algorithm: "ed25519".to_string(),
            public_key: signing.verifying_key().to_bytes().to_vec(),
            role: KeyRole::Root,
            purpose: KeyPurpose::Attestation,
            valid_from: from,
            valid_until: None,
            revoked_at: None,
        }
    }

    fn signed(signing: &SigningKey, key_id: Uuid, network_id: &str) -> ClaimRevokedPayload {
        let attestation_id = Uuid::new_v4();
        let signer = AttestationSigner {
            network_id,
            claim_kind: "achievement",
            issuer_ref: "game:ashen-realms",
            signing_key_id: key_id,
        };
        let bytes =
            revocation_signing_bytes(&signer, AttestationId(attestation_id), "cheating", "r");
        ClaimRevokedPayload {
            id: Uuid::new_v4(),
            attestation_id,
            issuer: "game:ashen-realms".to_string(),
            reason_code: RevocationReasonCode::from("cheating".to_string()),
            reason: "r".to_string(),
            proof: ClaimProofPayload {
                key_id,
                algorithm: "ed25519".to_string(),
                bytes: BASE64.encode(signing.sign(&bytes).to_bytes()),
            },
        }
    }

    #[test]
    fn a_genuine_revocation_verifies() {
        let (signing, key_id) = (SigningKey::from_bytes(&[3u8; 32]), Uuid::new_v4());
        let at = OffsetDateTime::now_utc();
        let keys = [key(&signing, key_id, at - time::Duration::hours(1))];
        let revoked = signed(&signing, key_id, NET);
        assert!(verify_signed_revocation(NET, "achievement", &revoked, &keys, at).is_ok());
    }

    #[test]
    fn a_revocation_signed_for_another_network_is_refused() {
        let (signing, key_id) = (SigningKey::from_bytes(&[3u8; 32]), Uuid::new_v4());
        let at = OffsetDateTime::now_utc();
        let keys = [key(&signing, key_id, at - time::Duration::hours(1))];
        let revoked = signed(&signing, key_id, "avalon-dev-other");
        assert!(matches!(
            verify_signed_revocation(NET, "achievement", &revoked, &keys, at),
            Err(IndexError::Rejected(_))
        ));
    }

    #[test]
    fn an_altered_reason_wrong_key_or_claim_kind_is_refused() {
        let (signing, key_id) = (SigningKey::from_bytes(&[3u8; 32]), Uuid::new_v4());
        let at = OffsetDateTime::now_utc();
        let keys = [key(&signing, key_id, at - time::Duration::hours(1))];
        let mut revoked = signed(&signing, key_id, NET);
        assert!(verify_signed_revocation(NET, "milestone", &revoked, &keys, at).is_err());
        revoked.reason = "other".to_string();
        assert!(verify_signed_revocation(NET, "achievement", &revoked, &keys, at).is_err());

        let impostor = SigningKey::from_bytes(&[4u8; 32]);
        let forged = signed(&impostor, key_id, NET);
        assert!(matches!(
            verify_signed_revocation(NET, "achievement", &forged, &keys, at),
            Err(IndexError::Rejected(_))
        ));
    }

    #[test]
    fn an_unknown_key_waits_and_a_revoked_or_future_key_is_refused() {
        let (signing, key_id) = (SigningKey::from_bytes(&[3u8; 32]), Uuid::new_v4());
        let at = OffsetDateTime::now_utc();
        let revoked = signed(&signing, key_id, NET);
        assert!(matches!(
            verify_signed_revocation(NET, "achievement", &revoked, &[], at),
            Err(IndexError::AwaitingKey(_))
        ));
        let mut stale = key(&signing, key_id, at - time::Duration::hours(2));
        stale.revoked_at = Some(at - time::Duration::hours(1));
        assert!(matches!(
            verify_signed_revocation(NET, "achievement", &revoked, &[stale], at),
            Err(IndexError::Rejected(_))
        ));
        let future = key(&signing, key_id, at + time::Duration::hours(1));
        assert!(matches!(
            verify_signed_revocation(NET, "achievement", &revoked, &[future], at),
            Err(IndexError::Rejected(_))
        ));
    }

    #[test]
    fn a_malformed_signature_is_refused() {
        let (signing, key_id) = (SigningKey::from_bytes(&[3u8; 32]), Uuid::new_v4());
        let at = OffsetDateTime::now_utc();
        let keys = [key(&signing, key_id, at - time::Duration::hours(1))];
        let mut revoked = signed(&signing, key_id, NET);
        revoked.proof.bytes = "not base64!".to_string();
        assert!(verify_signed_revocation(NET, "achievement", &revoked, &keys, at).is_err());
        revoked.proof.bytes = BASE64.encode([0u8; 10]);
        assert!(verify_signed_revocation(NET, "achievement", &revoked, &keys, at).is_err());
    }

    #[test]
    fn issuer_category_must_match_the_claim_kind() {
        assert!(issuer_matches_kind("game:a", "achievement"));
        assert!(!issuer_matches_kind("app:a", "achievement"));
        assert!(issuer_matches_kind("app:a", "milestone"));
        assert!(issuer_matches_kind("service:a", "milestone"));
        assert!(!issuer_matches_kind("game:a", "milestone"));
    }
}
