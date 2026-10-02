//! Self-certifying identity ids and the signing bytes that carry them.
//!
//! **Id format: `lowercase_hex(SHA-256("avalon-identity-id-v1" || key32))`**, where
//! `key32` is the identity's inception Ed25519 public key. The full 256 bits are
//! kept: exactly 64 characters of `[0-9a-f]`, no prefix (a prefix would collide with
//! the `:`-delimited global ids and signing bytes). The domain tag keeps the id
//! distinct from a `node:` shard id derived from the same key. The id commits to
//! the inception key only, so later key changes never change it.
//!
//! Parsing is strict and never normalises: uppercase, wrong length, UUID text and
//! `id:`/`node:` prefixes are all rejected.
//!
//! **Encodings.** A public key is lowercase HEX inside every signing-byte string
//! below, and standard BASE64 on the wire. Callers convert at the boundary.
//!
//! [`SelfCertifyingIdentityId`] is deliberately named apart from the legacy
//! `ids::IdentityId(Uuid)`; a later slice swaps the representation.
//! [`derive_identity_id`] does not check key acceptability: gate keys with
//! [`crate::ed25519_key::parse_ed25519_public_key`] first.

use std::fmt;
use std::str::FromStr;

use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

/// Domain-separation tag hashed ahead of the key when deriving an identity id.
pub const IDENTITY_ID_DOMAIN_TAG: &[u8] = b"avalon-identity-id-v1";

const IDENTITY_ID_LEN: usize = 64;

/// Why a string is not a canonical identity id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IdentityIdParseError {
    #[error("identity id must be exactly 64 characters")]
    WrongLength,
    #[error("identity id must be lowercase hex [0-9a-f]")]
    NotLowercaseHex,
}

/// A self-certifying identity id: the canonical 64-char lowercase hex string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SelfCertifyingIdentityId(String);

impl SelfCertifyingIdentityId {
    /// Strictly parses the canonical form; nothing is trimmed or lowercased.
    pub fn parse(text: &str) -> Result<Self, IdentityIdParseError> {
        if text.len() != IDENTITY_ID_LEN {
            return Err(IdentityIdParseError::WrongLength);
        }
        if !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(IdentityIdParseError::NotLowercaseHex);
        }
        Ok(Self(text.to_owned()))
    }

    /// The canonical 64-character string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this id is the one derived from `public_key`.
    pub fn matches_key(&self, public_key: &[u8; 32]) -> bool {
        derive_identity_id(public_key) == *self
    }
}

/// Derives the identity id for an inception public key (pure hash, no key checks).
pub fn derive_identity_id(public_key: &[u8; 32]) -> SelfCertifyingIdentityId {
    let mut hasher = Sha256::new();
    hasher.update(IDENTITY_ID_DOMAIN_TAG);
    hasher.update(public_key);
    SelfCertifyingIdentityId(hex::encode(hasher.finalize()))
}

/// Derives the identity id from an already-parsed verifying key.
pub fn derive_identity_id_for_key(key: &VerifyingKey) -> SelfCertifyingIdentityId {
    derive_identity_id(key.as_bytes())
}

impl fmt::Display for SelfCertifyingIdentityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SelfCertifyingIdentityId {
    type Err = IdentityIdParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for SelfCertifyingIdentityId {
    type Error = IdentityIdParseError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<SelfCertifyingIdentityId> for String {
    fn from(id: SelfCertifyingIdentityId) -> String {
        id.0
    }
}

/// Bytes signed for `identity.created` v2:
/// `avalon:identity.created:v2:{identity_id}:{public_key_hex}:{display_name}`.
/// The display name is last, so a `:` inside it is harmless.
pub fn identity_created_signing_bytes_v2(
    identity_id: &SelfCertifyingIdentityId,
    public_key: &[u8; 32],
    display_name: &str,
) -> Vec<u8> {
    format!(
        "avalon:identity.created:v2:{identity_id}:{}:{display_name}",
        hex::encode(public_key)
    )
    .into_bytes()
}

/// Bytes the approving device signs for a grant:
/// `avalon:device_grant.approved:v2:{grant_id}:{identity_id}:{requested_public_key_hex}`.
pub fn device_grant_approval_signing_bytes_v2(
    grant_id: Uuid,
    identity_id: &SelfCertifyingIdentityId,
    requested_public_key: &[u8; 32],
) -> Vec<u8> {
    format!(
        "avalon:device_grant.approved:v2:{grant_id}:{identity_id}:{}",
        hex::encode(requested_public_key)
    )
    .into_bytes()
}

/// Bytes a signing-key revocation signs:
/// `avalon:identity.signing_key_revoked:v2:{identity_id}:{signing_key_id}:{revoked_by_signing_key_id}`.
pub fn signing_key_revoked_signing_bytes_v2(
    identity_id: &SelfCertifyingIdentityId,
    signing_key_id: &str,
    revoked_by_signing_key_id: &str,
) -> Vec<u8> {
    format!(
        "avalon:identity.signing_key_revoked:v2:{identity_id}:{signing_key_id}:{revoked_by_signing_key_id}"
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shard_identity::derive_self_certifying_id;
    use ed25519_dalek::SigningKey;

    /// An identity with a fixed-seed signing key and its derived id.
    pub(crate) fn test_identity(seed: u8) -> (SigningKey, SelfCertifyingIdentityId) {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let id = derive_identity_id_for_key(&key.verifying_key());
        (key, id)
    }

    #[test]
    fn derived_id_is_64_lowercase_hex_and_round_trips() {
        let (key, id) = test_identity(7);
        assert_eq!(id.as_str().len(), 64);
        assert_eq!(SelfCertifyingIdentityId::parse(id.as_str()), Ok(id.clone()));
        assert!(id.matches_key(key.verifying_key().as_bytes()));
        assert!(!id.matches_key(&[1u8; 32]));
    }

    #[test]
    fn parse_is_strict() {
        let (_, id) = test_identity(7);
        let s = id.as_str();
        let bad = [
            s.to_uppercase(),
            s[..63].to_string(),
            format!("{s}0"),
            format!("id:{}", &s[3..]),
            format!("node:{s}"),
            Uuid::nil().to_string(),
            format!(" {}", &s[1..]),
        ];
        for b in bad {
            assert!(SelfCertifyingIdentityId::parse(&b).is_err(), "{b}");
        }
    }

    #[test]
    fn differs_from_the_node_shard_id_for_the_same_key() {
        let (key, id) = test_identity(7);
        let shard = derive_self_certifying_id(&key.verifying_key());
        assert_ne!(shard.strip_prefix("node:"), Some(id.as_str()));
    }

    #[test]
    fn serde_enforces_the_canonical_form() {
        let (_, id) = test_identity(7);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(
            serde_json::from_str::<SelfCertifyingIdentityId>(&json).unwrap(),
            id
        );
        assert!(serde_json::from_str::<SelfCertifyingIdentityId>(&json.to_uppercase()).is_err());
    }

    #[test]
    fn signing_bytes_have_the_documented_layout() {
        let (key, id) = test_identity(7);
        let pk = key.verifying_key().to_bytes();
        let created = identity_created_signing_bytes_v2(&id, &pk, "a:b");
        assert_eq!(
            String::from_utf8(created).unwrap(),
            format!("avalon:identity.created:v2:{id}:{}:a:b", hex::encode(pk))
        );
        let grant = device_grant_approval_signing_bytes_v2(Uuid::nil(), &id, &pk);
        assert!(String::from_utf8(grant)
            .unwrap()
            .starts_with("avalon:device_grant.approved:v2:00000000-"));
        let revoked = signing_key_revoked_signing_bytes_v2(&id, "k1", "k2");
        assert!(String::from_utf8(revoked)
            .unwrap()
            .ends_with(&format!("{id}:k1:k2")));
    }
}
