//! Signing-byte builders for the live attestation tests.
#![allow(dead_code)]

use avalon_protocol::achievements::{
    attestation_signing_bytes, bulk_attestation_signing_bytes, revocation_signing_bytes,
    AttestationSigner,
};
use avalon_protocol::ids::{AttestationId, IdentityId};
use uuid::Uuid;

/// Now, as the unix microseconds an issuer signs for `issued_at`.
pub fn now_micros() -> i64 {
    avalon_protocol::achievements::issued_at_micros(time::OffsetDateTime::now_utc())
}

fn signer<'a>(claim_kind: &'a str, issuer_ref: &'a str, key_id: &str) -> AttestationSigner<'a> {
    AttestationSigner {
        claim_kind,
        issuer_ref,
        signing_key_id: key_id.parse::<Uuid>().expect("key id is a uuid"),
    }
}

pub fn issue_bytes(
    claim_kind: &str,
    issuer_ref: &str,
    key_id: &str,
    subject: IdentityId,
    achievement: &str,
    issued_at: i64,
) -> Vec<u8> {
    let signer = signer(claim_kind, issuer_ref, key_id);
    attestation_signing_bytes(&signer, subject, achievement, issued_at)
}

pub fn bulk_bytes(
    claim_kind: &str,
    issuer_ref: &str,
    key_id: &str,
    subject: IdentityId,
    achievements: &[String],
    issued_at: i64,
) -> Vec<u8> {
    let signer = signer(claim_kind, issuer_ref, key_id);
    bulk_attestation_signing_bytes(&signer, subject, achievements, issued_at)
}

pub fn revoke_bytes(
    claim_kind: &str,
    issuer_ref: &str,
    key_id: &str,
    attestation_id: Uuid,
    reason_code: &str,
    reason: &str,
) -> Vec<u8> {
    let signer = signer(claim_kind, issuer_ref, key_id);
    revocation_signing_bytes(&signer, AttestationId(attestation_id), reason_code, reason)
}
