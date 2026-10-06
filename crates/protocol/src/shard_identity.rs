//! Self-certifying shard ids and the name-binding claim that maps a
//! human-readable name onto one — the decided direction of the
//! self-certifying-identity ADR: a shard's id is derived from its own
//! tree-head public key, so any verifier who is handed that key can check
//! it belongs to the id with no registry lookup and no authority online,
//! the same way an integrator's `game:<slug>` id is instead resolved
//! through `issuer.key_added` history (`crate::shard::shard_authority`).
//!
//! **Id format: `node:<hex>`, where `<hex>` is the lowercase hex encoding
//! of the full SHA-256 digest of the raw 32-byte Ed25519 public key.**
//! Full-length rather than truncated: truncating would only shorten the id
//! at the direct expense of the collision resistance the whole scheme rests
//! on, and this codebase already treats a full 64-hex-char digest as an
//! ordinary-length identifier (`SignedTreeHead::root_hash`). Hex rather than
//! base32/base58: every other hash or key in this codebase (`root_hash`,
//! `verify_key`, signatures) is already lowercase hex, and adding a second
//! encoding convention for this one id would cost more in consistency than
//! it would save in id length.
//!
//! **A hash, not the key itself, is what an id embeds — so an id alone
//! never yields a key.** A hash is one-way by design; "deriving the
//! expected key from the id" therefore means confirming a *presented*
//! candidate key hashes to the id, not extracting key bytes out of nothing.
//! [`resolve_self_certifying_key`] is that check: whatever channel hands a
//! verifier a shard's tree heads (a direct fetch, a gossiped announcement)
//! also has to hand it the signing key, and this function is the sole
//! arbiter of whether that presented key is the one the id actually
//! certifies — still no database, no network call beyond whatever already
//! delivered the key and the head being verified.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::ed25519_key::{
    is_acceptable_ed25519_key, parse_ed25519_public_key, parse_ed25519_public_key_hex,
};
use crate::shard::{parse_shard_id, ParsedShardId, SELF_CERTIFYING_NAMESPACE};
use crate::sth::{verify_tree_head, SignedTreeHead};

/// Derives `key`'s self-certifying shard id: `node:<sha256-hex-of-key>`.
pub fn derive_self_certifying_id(key: &VerifyingKey) -> String {
    let digest = Sha256::digest(key.as_bytes());
    format!("{SELF_CERTIFYING_NAMESPACE}:{}", hex::encode(digest))
}

/// Parses the raw key bytes presented for a `node:` shard, enforcing
/// [`is_acceptable_ed25519_key`]. The single entry point for such keys.
pub fn parse_shard_public_key(bytes: &[u8; 32]) -> Option<VerifyingKey> {
    parse_ed25519_public_key(bytes)
}

/// Parses a presented key: exactly 64 lowercase hex characters forming a key
/// that passes [`is_acceptable_ed25519_key`].
pub fn parse_shard_public_key_hex(key_hex: &str) -> Option<VerifyingKey> {
    parse_ed25519_public_key_hex(key_hex)
}

/// Whether `id` parses as a self-certifying (`node:<hash>`) shard id —
/// purely syntactic, no key involved.
pub fn is_self_certifying(id: &str) -> bool {
    matches!(parse_shard_id(id), Ok(ParsedShardId::SelfCertifying { .. }))
}

/// Confirms `candidate_key` is the key `id` was derived from: re-derives
/// the id from `candidate_key` and compares. Returns the key back (as the
/// shard's now-confirmed verifying key) on a match, `None` if `id` isn't a
/// self-certifying id at all or the hash doesn't match — the entire
/// verification surface, with no registry and no I/O.
pub fn resolve_self_certifying_key(id: &str, candidate_key: &VerifyingKey) -> Option<VerifyingKey> {
    let ParsedShardId::SelfCertifying { .. } = parse_shard_id(id).ok()? else {
        return None;
    };
    if !is_acceptable_ed25519_key(candidate_key) {
        return None;
    }
    (derive_self_certifying_id(candidate_key) == id).then_some(*candidate_key)
}

/// Verifies `sth` was produced by the key `id` certifies: `candidate_key`
/// must both hash to `id` and have actually signed `sth`. This is the
/// complete self-certifying verification path — no registry, no network
/// call beyond whatever already delivered `candidate_key` and `sth`.
pub fn verify_self_certifying_tree_head(
    id: &str,
    candidate_key: &VerifyingKey,
    sth: &SignedTreeHead,
) -> bool {
    resolve_self_certifying_key(id, candidate_key).is_some() && verify_tree_head(candidate_key, sth)
}

/// The exact bytes a [`NameBindingClaim`] signature covers: `(id, key,
/// name, created_at)` under the tag `avalon-name-binding-v1`, the id and name
/// length-prefixed, `key` fixed-width (32 raw bytes) and `created_at` an `i64` of unix
/// seconds. Intentionally not moved to `crate::signing_bytes`: it already has a distinct tag, a
/// length prefix on every variable field, raw fixed-width key and integer, so no field can shift
/// another's boundary, and no ledger, SDK or vector depends on its bytes yet.
fn name_binding_signing_message(
    self_certifying_id: &str,
    key: &VerifyingKey,
    name: &str,
    created_at: OffsetDateTime,
) -> Vec<u8> {
    let mut message = Vec::new();
    message.extend_from_slice(b"avalon-name-binding-v1");
    message.extend_from_slice(&(self_certifying_id.len() as u32).to_be_bytes());
    message.extend_from_slice(self_certifying_id.as_bytes());
    message.extend_from_slice(key.as_bytes());
    message.extend_from_slice(&(name.len() as u32).to_be_bytes());
    message.extend_from_slice(name.as_bytes());
    message.extend_from_slice(&created_at.unix_timestamp().to_be_bytes());
    message
}

/// A shard's own signed claim that it wants to be known by `name` — the
/// payload shape the (separate, not-yet-built) naming/registry layer
/// resolves and disputes on top of. Binds a human-readable `name` to a
/// self-certifying id, signed by the exact key that id certifies, so
/// verifying the claim needs nothing beyond the claim itself: no lookup of
/// who "owns" `name`, no confirmation that `self_certifying_id` is even
/// real. What it does NOT establish — uniqueness of `name`, priority
/// between conflicting claims, domain-proof — is deliberately out of scope
/// here; that's the registry/resolution logic the naming layer builds on
/// top of this shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameBindingClaim {
    pub self_certifying_id: String,
    /// Raw 32-byte Ed25519 public key, lowercase hex — carried alongside
    /// the id rather than requiring a lookup, so this claim verifies in
    /// total isolation.
    pub public_key: String,
    pub name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Lowercase hex-encoded Ed25519 signature (64 bytes).
    pub signature: String,
}

/// Signs a claim binding `name` to `signing_key`'s self-certifying id.
pub fn sign_name_binding_claim(
    signing_key: &SigningKey,
    name: &str,
    created_at: OffsetDateTime,
) -> NameBindingClaim {
    let verifying_key = signing_key.verifying_key();
    let self_certifying_id = derive_self_certifying_id(&verifying_key);
    let message =
        name_binding_signing_message(&self_certifying_id, &verifying_key, name, created_at);
    let signature: Signature = signing_key.sign(&message);
    NameBindingClaim {
        self_certifying_id,
        public_key: hex::encode(verifying_key.to_bytes()),
        name: name.to_string(),
        created_at,
        signature: hex::encode(signature.to_bytes()),
    }
}

/// Verifies a [`NameBindingClaim`] end to end: `public_key` must actually
/// hash to `self_certifying_id` (proving the claim wasn't assembled from an
/// unrelated key/id pair), and `signature` must verify against that same
/// key over exactly the claimed fields. `false` for any malformed encoding,
/// never panics on attacker-controlled input.
pub fn verify_name_binding_claim(claim: &NameBindingClaim) -> bool {
    let Ok(key_bytes) = hex::decode(&claim.public_key) else {
        return false;
    };
    let Ok(key_array) = <[u8; 32]>::try_from(key_bytes.as_slice()) else {
        return false;
    };
    let Some(verifying_key) = parse_shard_public_key(&key_array) else {
        return false;
    };
    if derive_self_certifying_id(&verifying_key) != claim.self_certifying_id {
        return false;
    }
    let message = name_binding_signing_message(
        &claim.self_certifying_id,
        &verifying_key,
        &claim.name,
        claim.created_at,
    );
    let Ok(signature_bytes) = hex::decode(&claim.signature) else {
        return false;
    };
    let Ok(signature_array) = <[u8; 64]>::try_from(signature_bytes.as_slice()) else {
        return false;
    };
    let signature = Signature::from_bytes(&signature_array);
    verifying_key.verify(&message, &signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sth::sign_tree_head;

    fn root_hash_fixture() -> String {
        "cd".repeat(32)
    }

    #[test]
    fn derived_id_has_the_expected_shape() {
        let key = SigningKey::from_bytes(&[7u8; 32]).verifying_key();
        let id = derive_self_certifying_id(&key);
        assert!(id.starts_with("node:"));
        assert_eq!(id.len(), "node:".len() + 64);
        assert!(is_self_certifying(&id));
        assert!(!is_self_certifying("game:wow"));
        assert!(!is_self_certifying("core"));
    }

    #[test]
    fn resolves_when_the_presented_key_matches() {
        let key = SigningKey::generate(&mut rand::rng()).verifying_key();
        let id = derive_self_certifying_id(&key);
        assert_eq!(resolve_self_certifying_key(&id, &key), Some(key));
    }

    #[test]
    fn rejects_a_key_that_does_not_hash_to_the_id() {
        let real_key = SigningKey::generate(&mut rand::rng()).verifying_key();
        let forged_key = SigningKey::generate(&mut rand::rng()).verifying_key();
        let id = derive_self_certifying_id(&real_key);
        assert_ne!(real_key, forged_key);
        assert_eq!(resolve_self_certifying_key(&id, &forged_key), None);
    }

    #[test]
    fn rejects_a_non_self_certifying_id() {
        let key = SigningKey::generate(&mut rand::rng()).verifying_key();
        assert_eq!(resolve_self_certifying_key("game:wow", &key), None);
        assert_eq!(resolve_self_certifying_key("core", &key), None);
    }

    #[test]
    fn verifies_a_tree_head_purely_from_the_id_with_no_registry_or_db() {
        let signing_key = SigningKey::generate(&mut rand::rng());
        let verifying_key = signing_key.verifying_key();
        let id = derive_self_certifying_id(&verifying_key);

        let sth = sign_tree_head(
            &signing_key,
            "node-key",
            10,
            &root_hash_fixture(),
            "avalon-test",
            OffsetDateTime::UNIX_EPOCH,
        )
        .unwrap();

        assert!(verify_self_certifying_tree_head(&id, &verifying_key, &sth));
    }

    #[test]
    fn rejects_a_forged_id_key_mismatch_even_with_a_validly_signed_head() {
        let real_key = SigningKey::generate(&mut rand::rng());
        let attacker_key = SigningKey::generate(&mut rand::rng());
        // The attacker signs with their own key but claims the real
        // shard's id — resolution must fail before signature checking
        // even matters.
        let claimed_id = derive_self_certifying_id(&real_key.verifying_key());

        let sth = sign_tree_head(
            &attacker_key,
            "node-key",
            1,
            &root_hash_fixture(),
            "avalon-test",
            OffsetDateTime::UNIX_EPOCH,
        )
        .unwrap();

        assert!(!verify_self_certifying_tree_head(
            &claimed_id,
            &attacker_key.verifying_key(),
            &sth
        ));
    }

    #[test]
    fn rejects_a_correctly_matching_key_that_did_not_actually_sign() {
        let signing_key = SigningKey::generate(&mut rand::rng());
        let other_key = SigningKey::generate(&mut rand::rng());
        let id = derive_self_certifying_id(&signing_key.verifying_key());

        // Signed by a different key entirely, so this head never carries a
        // valid signature under the id's own certified key.
        let sth = sign_tree_head(
            &other_key,
            "node-key",
            1,
            &root_hash_fixture(),
            "avalon-test",
            OffsetDateTime::UNIX_EPOCH,
        )
        .unwrap();

        assert!(!verify_self_certifying_tree_head(
            &id,
            &signing_key.verifying_key(),
            &sth
        ));
    }

    fn identity_bytes() -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[0] = 1;
        bytes
    }

    #[test]
    fn shard_key_policy_rejects_weak_and_non_canonical_encodings() {
        let honest = SigningKey::from_bytes(&[7u8; 32]).verifying_key();
        assert_eq!(parse_shard_public_key(honest.as_bytes()), Some(honest));

        // Identity, identity with the sign bit, identity as y = p + 1.
        let mut identity_signed = identity_bytes();
        identity_signed[31] |= 0x80;
        let mut identity_wrapped = [0xffu8; 32];
        identity_wrapped[0] = 0xee;
        identity_wrapped[31] = 0x7f;
        // Order-2 point (y = p - 1), with and without the sign bit; order-4 (y = 0).
        let mut order_two = [0xffu8; 32];
        order_two[0] = 0xec;
        order_two[31] = 0x7f;
        let mut order_two_signed = order_two;
        order_two_signed[31] |= 0x80;
        for weak in [
            identity_bytes(),
            identity_signed,
            identity_wrapped,
            order_two,
            order_two_signed,
            [0u8; 32],
        ] {
            assert_eq!(parse_shard_public_key(&weak), None, "{weak:02x?}");
        }

        // y = 3 is a valid non-weak point; y = 3 + p is its non-canonical encoding.
        let mut canonical_y3 = [0u8; 32];
        canonical_y3[0] = 3;
        assert!(parse_shard_public_key(&canonical_y3).is_some());
        let mut wrapped_y3 = [0xffu8; 32];
        wrapped_y3[0] = 0xed + 3;
        wrapped_y3[31] = 0x7f;
        assert!(VerifyingKey::from_bytes(&wrapped_y3).is_ok());
        assert_eq!(parse_shard_public_key(&wrapped_y3), None);
    }

    #[test]
    fn resolution_refuses_a_weak_key_even_when_the_id_matches() {
        let weak = VerifyingKey::from_bytes(&identity_bytes()).unwrap();
        let id = derive_self_certifying_id(&weak);
        assert_eq!(resolve_self_certifying_key(&id, &weak), None);
    }

    #[test]
    fn name_binding_claim_round_trips() {
        let signing_key = SigningKey::generate(&mut rand::rng());
        let claim = sign_name_binding_claim(&signing_key, "wow-demo", OffsetDateTime::UNIX_EPOCH);

        assert_eq!(
            claim.self_certifying_id,
            derive_self_certifying_id(&signing_key.verifying_key())
        );
        assert!(verify_name_binding_claim(&claim));
    }

    #[test]
    fn name_binding_claim_rejects_tampered_fields() {
        let signing_key = SigningKey::generate(&mut rand::rng());
        let claim = sign_name_binding_claim(&signing_key, "wow-demo", OffsetDateTime::UNIX_EPOCH);

        let mut tampered_name = claim.clone();
        tampered_name.name = "not-wow-demo".to_string();
        assert!(!verify_name_binding_claim(&tampered_name));

        let mut tampered_time = claim.clone();
        tampered_time.created_at = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1);
        assert!(!verify_name_binding_claim(&tampered_time));

        let mut tampered_id = claim.clone();
        tampered_id.self_certifying_id = "node:".to_string() + &"0".repeat(64);
        assert!(!verify_name_binding_claim(&tampered_id));
    }

    #[test]
    fn name_binding_claim_rejects_a_key_not_matching_its_own_claimed_id() {
        // The claim's `public_key` is swapped for an unrelated key that
        // never produced `self_certifying_id` — simulates an attacker
        // assembling a claim from someone else's id with their own key.
        let signing_key = SigningKey::generate(&mut rand::rng());
        let mut claim =
            sign_name_binding_claim(&signing_key, "wow-demo", OffsetDateTime::UNIX_EPOCH);

        let attacker_key = SigningKey::generate(&mut rand::rng());
        claim.public_key = hex::encode(attacker_key.verifying_key().to_bytes());
        assert!(!verify_name_binding_claim(&claim));
    }

    #[test]
    fn name_binding_claim_rejects_a_signature_from_a_different_key() {
        // Same id/public_key pair, but the signature was produced by a
        // different key entirely — never actually authored by the key the
        // claim claims to bind.
        let real_key = SigningKey::generate(&mut rand::rng());
        let attacker_key = SigningKey::generate(&mut rand::rng());
        let mut claim = sign_name_binding_claim(&real_key, "wow-demo", OffsetDateTime::UNIX_EPOCH);

        let message = name_binding_signing_message(
            &claim.self_certifying_id,
            &real_key.verifying_key(),
            &claim.name,
            claim.created_at,
        );
        let forged_signature: Signature = attacker_key.sign(&message);
        claim.signature = hex::encode(forged_signature.to_bytes());
        assert!(!verify_name_binding_claim(&claim));
    }

    #[test]
    fn verify_rejects_malformed_encoding_without_panicking() {
        let signing_key = SigningKey::generate(&mut rand::rng());
        let mut claim =
            sign_name_binding_claim(&signing_key, "wow-demo", OffsetDateTime::UNIX_EPOCH);

        let mut bad_key = claim.clone();
        bad_key.public_key = "not-hex".to_string();
        assert!(!verify_name_binding_claim(&bad_key));

        claim.signature = "not-hex".to_string();
        assert!(!verify_name_binding_claim(&claim));
    }
}
