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
//! check either way, never reimplemented at each call site. [`verify_signature`]
//! is its generic core, reused by revocation verification (#85) for the
//! exact same reason — a different canonical byte shape
//! ([`avalon_protocol::achievements::revocation_signing_bytes`]), the same
//! "resolve the key at this point in time, then check the signature" logic.

use avalon_protocol::achievements::{attestation_signing_bytes, AchievementAttestation};
use avalon_protocol::integrators::{resolve_valid_signing_key, IssuerKey};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use time::OffsetDateTime;
use uuid::Uuid;

/// The result of checking whether a signature was genuinely produced by
/// one of an issuer's keys, at a given point in time (#84's point-in-time
/// key resolution — a since-rotated, not-yet-revoked-at-that-time key
/// still verifies correctly).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authenticity {
    Authentic { key_id: String },
    NotAuthentic { reason: String },
}

/// The generic check: does `signature_bytes` verify against `key_id` (one
/// of `issuer_keys`, resolved as valid at `at`) over `signing_bytes`?
/// Every caller — attestation issuance/reads, attestation revocation — is
/// responsible for building `signing_bytes` correctly for what it's
/// actually checking (different canonical byte shapes for "issued" vs.
/// "revoked", see `avalon_protocol::achievements`'s two `*_signing_bytes`
/// functions); this function only does the cryptography, never assumes
/// which action a signature was for.
pub fn verify_signature(
    key_id: &str,
    signing_bytes: &[u8],
    signature_bytes: &[u8],
    at: OffsetDateTime,
    issuer_keys: &[IssuerKey],
) -> Authenticity {
    let Ok(key_id_uuid) = key_id.parse::<Uuid>() else {
        return Authenticity::NotAuthentic {
            reason: "key_id is not a valid key id".to_string(),
        };
    };
    let Some(key) = resolve_valid_signing_key(issuer_keys, key_id_uuid, at) else {
        return Authenticity::NotAuthentic {
            reason: "no key in the issuer's history resolves as valid at this point in time"
                .to_string(),
        };
    };

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
    let Ok(sig_array) = <[u8; 64]>::try_from(signature_bytes) else {
        return Authenticity::NotAuthentic {
            reason: "signature is malformed".to_string(),
        };
    };
    let signature = Signature::from_bytes(&sig_array);

    if verifying_key.verify(signing_bytes, &signature).is_ok() {
        Authenticity::Authentic {
            key_id: key_id.to_string(),
        }
    } else {
        Authenticity::NotAuthentic {
            reason: "signature does not verify against the resolved key".to_string(),
        }
    }
}

/// Verifies `attestation` against `issuer_keys` (the issuer's full key
/// history — see `crate::mirror`/`avalon-server`'s `fetch_issuer_keys` for
/// where that comes from). `claim_kind` (`"achievement"`/`"milestone"`)
/// and `issuer_ref` (`"<namespace>:<slug>"`) must match exactly what the
/// issuer signed over ([`attestation_signing_bytes`]) — a caller checking
/// the wrong claim kind or issuer string will correctly get
/// `NotAuthentic`, not a false positive, since the signed bytes themselves
/// would differ. Thin wrapper over [`verify_signature`]: builds the
/// issuance-specific canonical bytes and resolves the key at the
/// attestation's own `issued_at`.
pub fn verify_authenticity(
    attestation: &AchievementAttestation,
    claim_kind: &str,
    issuer_ref: &str,
    issuer_keys: &[IssuerKey],
) -> Authenticity {
    let signing_bytes = attestation_signing_bytes(
        claim_kind,
        issuer_ref,
        attestation.subject,
        attestation.achievement.as_str(),
    );
    verify_signature(
        &attestation.proof.key_id,
        &signing_bytes,
        &attestation.proof.bytes,
        attestation.issued_at,
        issuer_keys,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use avalon_protocol::achievements::{Issuer, Signature as AttestationSignature};
    use avalon_protocol::ids::{AttestationId, GlobalId, IdentityId, IntegratorId};
    use avalon_protocol::integrators::{IssuerKey, KeyRole};
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
            issuer: Issuer::Game(IntegratorId(Uuid::new_v4())),
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
            purpose: avalon_protocol::integrators::KeyPurpose::Attestation,
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

    /// #85: revocation verification reuses the exact same [`verify_signature`]
    /// core, just with revocation's own canonical bytes — proving the two
    /// actions genuinely share the generic check rather than each hand-rolling
    /// crypto.
    #[test]
    fn verify_signature_backs_revocation_verification_too() {
        use avalon_protocol::achievements::revocation_signing_bytes;
        use avalon_protocol::ids::AttestationId;

        let signing_key = SigningKey::generate(&mut rand::rng());
        let key_id = Uuid::new_v4();
        let revoked_at = OffsetDateTime::now_utc();
        let attestation_id = AttestationId(Uuid::new_v4());

        let bytes = revocation_signing_bytes(
            "achievement",
            "game:ashen-realms",
            attestation_id,
            "issuer_error",
        );
        let signature = signing_key.sign(&bytes);
        let keys = [issuer_key(&signing_key, key_id, OffsetDateTime::UNIX_EPOCH)];

        assert_eq!(
            verify_signature(
                &key_id.to_string(),
                &bytes,
                &signature.to_bytes(),
                revoked_at,
                &keys,
            ),
            Authenticity::Authentic {
                key_id: key_id.to_string()
            }
        );

        // A different reason code produces different bytes, so a signature
        // for one reason can't be replayed to claim a different one.
        let other_bytes = revocation_signing_bytes(
            "achievement",
            "game:ashen-realms",
            attestation_id,
            "different_reason",
        );
        assert!(matches!(
            verify_signature(
                &key_id.to_string(),
                &other_bytes,
                &signature.to_bytes(),
                revoked_at,
                &keys,
            ),
            Authenticity::NotAuthentic { .. }
        ));
    }
}
