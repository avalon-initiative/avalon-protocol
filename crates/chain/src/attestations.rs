//! Attestation authenticity verification (issue #33, implementing ADR
//! #76's first of three separate questions: authentic, valid, recognized —
//! never merged into one boolean). Lives here, not in `avalon-protocol`,
//! for the same reason `sth::verify_tree_head` does: it needs real Ed25519
//! verification, and this crate is where actual cryptographic checks
//! against ledger-adjacent data already live.
//!
//! [`verify_authenticity`] is the one function both the issuing endpoint
//! (`crates/server/src/achievements.rs`, #32) and a future independent
//! reader (`GET /attestations/{id}`) should call — the same signature
//! check either way, never reimplemented at each call site.

use avalon_protocol::achievements::{attestation_signing_bytes, AchievementAttestation};
use avalon_protocol::games::{resolve_valid_signing_key, IssuerKey};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

/// The result of checking whether `attestation`'s embedded signature was
/// genuinely produced by one of `issuer_keys`, at the point in time the
/// attestation claims to have been issued (#84's point-in-time key
/// resolution — a since-rotated, not-yet-revoked-at-`issued_at` key still
/// verifies correctly).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authenticity {
    Authentic { key_id: String },
    NotAuthentic { reason: String },
}

/// Verifies `attestation` against `issuer_keys` (the issuer's full key
/// history — see `crate::mirror`/`avalon-server`'s `fetch_issuer_keys` for
/// where that comes from). `claim_kind` (`"achievement"`/`"milestone"`)
/// and `issuer_ref` (`"<namespace>:<slug>"`) must match exactly what the
/// issuer signed over ([`attestation_signing_bytes`]) — a caller checking
/// the wrong claim kind or issuer string will correctly get
/// `NotAuthentic`, not a false positive, since the signed bytes themselves
/// would differ.
pub fn verify_authenticity(
    attestation: &AchievementAttestation,
    claim_kind: &str,
    issuer_ref: &str,
    issuer_keys: &[IssuerKey],
) -> Authenticity {
    let Ok(key_id) = attestation.proof.key_id.parse::<uuid::Uuid>() else {
        return Authenticity::NotAuthentic {
            reason: "proof.key_id is not a valid key id".to_string(),
        };
    };
    let Some(key) = resolve_valid_signing_key(issuer_keys, key_id, attestation.issued_at) else {
        return Authenticity::NotAuthentic {
            reason: "no key in the issuer's history resolves as valid at issued_at".to_string(),
        };
    };

    let signing_bytes = attestation_signing_bytes(
        claim_kind,
        issuer_ref,
        attestation.subject,
        attestation.achievement.as_str(),
    );

    let Ok(key_array) = <[u8; 32]>::try_from(key.public_key.as_slice()) else {
        return Authenticity::NotAuthentic {
            reason: "issuer key is malformed".to_string(),
        };
    };
    let Ok(verifying_key) = VerifyingKey::from_bytes(&key_array) else {
        return Authenticity::NotAuthentic {
            reason: "issuer key is malformed".to_string(),
        };
    };
    let Ok(sig_array) = <[u8; 64]>::try_from(attestation.proof.bytes.as_slice()) else {
        return Authenticity::NotAuthentic {
            reason: "signature is malformed".to_string(),
        };
    };
    let signature = Signature::from_bytes(&sig_array);

    if verifying_key.verify(&signing_bytes, &signature).is_ok() {
        Authenticity::Authentic {
            key_id: attestation.proof.key_id.clone(),
        }
    } else {
        Authenticity::NotAuthentic {
            reason: "signature does not verify against the resolved key".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use avalon_protocol::achievements::{Issuer, Signature as AttestationSignature};
    use avalon_protocol::games::{IssuerKey, KeyRole};
    use avalon_protocol::ids::{AttestationId, GameId, GlobalId, IdentityId};
    use ed25519_dalek::{Signer, SigningKey};
    use time::OffsetDateTime;
    use uuid::Uuid;

    fn signed_attestation(
        signing_key: &SigningKey,
        key_id: Uuid,
        claim_kind: &str,
        issuer_ref: &str,
        subject: IdentityId,
        achievement: GlobalId,
        issued_at: OffsetDateTime,
    ) -> AchievementAttestation {
        let bytes =
            attestation_signing_bytes(claim_kind, issuer_ref, subject, achievement.as_str());
        let signature = signing_key.sign(&bytes);
        AchievementAttestation {
            id: AttestationId(Uuid::new_v4()),
            issuer: Issuer::Game(GameId(Uuid::new_v4())),
            subject,
            achievement,
            issued_at,
            proof: AttestationSignature {
                key_id: key_id.to_string(),
                algorithm: "ed25519".to_string(),
                bytes: signature.to_bytes().to_vec(),
            },
        }
    }

    fn issuer_key(signing_key: &SigningKey, key_id: Uuid, valid_from: OffsetDateTime) -> IssuerKey {
        IssuerKey {
            key_id,
            algorithm: "ed25519".to_string(),
            public_key: signing_key.verifying_key().to_bytes().to_vec(),
            role: KeyRole::Root,
            valid_from,
            valid_until: None,
            revoked_at: None,
        }
    }

    #[test]
    fn a_genuine_signature_from_a_valid_key_is_authentic() {
        let signing_key = SigningKey::generate(&mut rand::rng());
        let key_id = Uuid::new_v4();
        let issued_at = OffsetDateTime::now_utc();
        let achievement = GlobalId::new("game", "ashen-realms", "achievement", "dragon_slayer");
        let subject = IdentityId(Uuid::new_v4());

        let attestation = signed_attestation(
            &signing_key,
            key_id,
            "achievement",
            "game:ashen-realms",
            subject,
            achievement,
            issued_at,
        );
        let keys = [issuer_key(&signing_key, key_id, OffsetDateTime::UNIX_EPOCH)];

        assert_eq!(
            verify_authenticity(&attestation, "achievement", "game:ashen-realms", &keys),
            Authenticity::Authentic {
                key_id: key_id.to_string()
            }
        );
    }

    #[test]
    fn a_signature_from_the_wrong_key_is_not_authentic() {
        let signing_key = SigningKey::generate(&mut rand::rng());
        let other_key = SigningKey::generate(&mut rand::rng());
        let key_id = Uuid::new_v4();
        let issued_at = OffsetDateTime::now_utc();
        let achievement = GlobalId::new("game", "ashen-realms", "achievement", "dragon_slayer");
        let subject = IdentityId(Uuid::new_v4());

        // Signed by other_key, but claims to be key_id (which is
        // registered as signing_key's public half below).
        let attestation = signed_attestation(
            &other_key,
            key_id,
            "achievement",
            "game:ashen-realms",
            subject,
            achievement,
            issued_at,
        );
        let keys = [issuer_key(&signing_key, key_id, OffsetDateTime::UNIX_EPOCH)];

        assert!(matches!(
            verify_authenticity(&attestation, "achievement", "game:ashen-realms", &keys),
            Authenticity::NotAuthentic { .. }
        ));
    }

    /// Scenario E's authenticity half: a claim signed before rotation
    /// stays authentic even after the key that signed it is later revoked.
    #[test]
    fn a_claim_stays_authentic_after_its_key_is_later_revoked() {
        let signing_key = SigningKey::generate(&mut rand::rng());
        let key_id = Uuid::new_v4();
        let issued_at = OffsetDateTime::UNIX_EPOCH + time::Duration::hours(5);
        let achievement = GlobalId::new("game", "ashen-realms", "achievement", "dragon_slayer");
        let subject = IdentityId(Uuid::new_v4());

        let attestation = signed_attestation(
            &signing_key,
            key_id,
            "achievement",
            "game:ashen-realms",
            subject,
            achievement,
            issued_at,
        );
        let mut key = issuer_key(&signing_key, key_id, OffsetDateTime::UNIX_EPOCH);
        key.revoked_at = Some(OffsetDateTime::UNIX_EPOCH + time::Duration::hours(20)); // revoked well after issuance
        let keys = [key];

        assert_eq!(
            verify_authenticity(&attestation, "achievement", "game:ashen-realms", &keys),
            Authenticity::Authentic {
                key_id: key_id.to_string()
            }
        );
    }

    /// Scenario F's authenticity half: a claim "signed" after revocation
    /// (i.e. the key was no longer valid at `issued_at`) is not authentic,
    /// even with a structurally valid signature.
    #[test]
    fn a_claim_issued_after_its_keys_revocation_is_not_authentic() {
        let signing_key = SigningKey::generate(&mut rand::rng());
        let key_id = Uuid::new_v4();
        let revoked_at = OffsetDateTime::UNIX_EPOCH + time::Duration::hours(10);
        let issued_at = revoked_at + time::Duration::hours(1); // after revocation
        let achievement = GlobalId::new("game", "ashen-realms", "achievement", "dragon_slayer");
        let subject = IdentityId(Uuid::new_v4());

        let attestation = signed_attestation(
            &signing_key,
            key_id,
            "achievement",
            "game:ashen-realms",
            subject,
            achievement,
            issued_at,
        );
        let mut key = issuer_key(&signing_key, key_id, OffsetDateTime::UNIX_EPOCH);
        key.revoked_at = Some(revoked_at);
        let keys = [key];

        assert!(matches!(
            verify_authenticity(&attestation, "achievement", "game:ashen-realms", &keys),
            Authenticity::NotAuthentic { .. }
        ));
    }

    #[test]
    fn a_signature_produced_for_the_wrong_claim_kind_is_not_authentic() {
        // The exact property attestation_signing_bytes's own doc comment
        // calls out: folding claim_kind into the signed bytes means a
        // signature can't be replayed across vocabularies.
        let signing_key = SigningKey::generate(&mut rand::rng());
        let key_id = Uuid::new_v4();
        let issued_at = OffsetDateTime::now_utc();
        let achievement = GlobalId::new("app", "wallet-app", "milestone", "onboarded");
        let subject = IdentityId(Uuid::new_v4());

        // Signed as a milestone...
        let attestation = signed_attestation(
            &signing_key,
            key_id,
            "milestone",
            "app:wallet-app",
            subject,
            achievement,
            issued_at,
        );
        let keys = [issuer_key(&signing_key, key_id, OffsetDateTime::UNIX_EPOCH)];

        // ...but verified as an achievement.
        assert!(matches!(
            verify_authenticity(&attestation, "achievement", "app:wallet-app", &keys),
            Authenticity::NotAuthentic { .. }
        ));
    }

    #[test]
    fn an_unregistered_key_id_is_not_authentic() {
        let signing_key = SigningKey::generate(&mut rand::rng());
        let key_id = Uuid::new_v4();
        let issued_at = OffsetDateTime::now_utc();
        let achievement = GlobalId::new("game", "ashen-realms", "achievement", "dragon_slayer");
        let subject = IdentityId(Uuid::new_v4());

        let attestation = signed_attestation(
            &signing_key,
            key_id,
            "achievement",
            "game:ashen-realms",
            subject,
            achievement,
            issued_at,
        );
        // No matching key in the issuer's history at all.
        let keys: [IssuerKey; 0] = [];

        assert!(matches!(
            verify_authenticity(&attestation, "achievement", "game:ashen-realms", &keys),
            Authenticity::NotAuthentic { .. }
        ));
    }
}
