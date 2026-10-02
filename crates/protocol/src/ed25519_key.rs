//! Acceptability policy for Ed25519 public keys that an id commits to
//! (`node:` shard ids and self-certifying identity ids), plus strict
//! signature verification.

use ed25519_dalek::{Signature, VerifyingKey};

/// Whether `key`'s encoding is acceptable: canonical (y below 2^255-19; a sign bit
/// on x = 0 only occurs at the identity and the order-2 point, which the
/// small-order rule rejects) and not of small order. Keys with a torsion component
/// that are neither small-order nor non-canonical are deliberately accepted: an id
/// binds the exact key bytes and verification is cofactorless, so a
/// prime-order-subgroup-only rule would make implementations disagree.
pub fn is_acceptable_ed25519_key(key: &VerifyingKey) -> bool {
    let bytes = key.as_bytes();
    // p = 2^255 - 19, so y >= p iff the low 255 bits are ff..ff with a first byte >= 0xed.
    let non_canonical_y =
        bytes[0] >= 0xed && bytes[1..31].iter().all(|b| *b == 0xff) && bytes[31] & 0x7f == 0x7f;
    !non_canonical_y && !key.is_weak()
}

/// Parses raw key bytes, enforcing [`is_acceptable_ed25519_key`].
pub fn parse_ed25519_public_key(bytes: &[u8; 32]) -> Option<VerifyingKey> {
    let key = VerifyingKey::from_bytes(bytes).ok()?;
    is_acceptable_ed25519_key(&key).then_some(key)
}

/// Parses exactly 64 lowercase hex characters into an acceptable key
/// (see [`is_acceptable_ed25519_key`]); uppercase or any other text is refused.
pub fn parse_ed25519_public_key_hex(key_hex: &str) -> Option<VerifyingKey> {
    let lowercase_hex = key_hex.len() == 64
        && key_hex
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if !lowercase_hex {
        return None;
    }
    let bytes: [u8; 32] = hex::decode(key_hex).ok()?.try_into().ok()?;
    parse_ed25519_public_key(&bytes)
}

/// Strict Ed25519 verification (`verify_strict`: rejects small-order keys and
/// R, and non-canonical S). Identity signatures must be checked with this.
pub fn verify_strict_signature(key: &VerifyingKey, message: &[u8], signature: &[u8; 64]) -> bool {
    key.verify_strict(message, &Signature::from_bytes(signature))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn strict_verification_accepts_a_good_signature_and_rejects_a_bad_one() {
        let signing = SigningKey::from_bytes(&[5u8; 32]);
        let key = signing.verifying_key();
        let sig = signing.sign(b"msg").to_bytes();
        assert!(verify_strict_signature(&key, b"msg", &sig));
        assert!(!verify_strict_signature(&key, b"other", &sig));
    }

    #[test]
    fn hex_parse_refuses_uppercase_and_wrong_length() {
        let key = SigningKey::from_bytes(&[5u8; 32]).verifying_key();
        let lower = hex::encode(key.as_bytes());
        assert_eq!(parse_ed25519_public_key_hex(&lower), Some(key));
        assert_eq!(parse_ed25519_public_key_hex(&lower.to_uppercase()), None);
        assert_eq!(parse_ed25519_public_key_hex(&lower[..62]), None);
    }
}
