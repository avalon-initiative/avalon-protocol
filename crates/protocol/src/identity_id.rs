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
//! [`derive_identity_id`] does not check key acceptability: gate keys with
//! [`crate::ed25519_key::parse_ed25519_public_key`] first.

use std::fmt;
use std::str::FromStr;

use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use utoipa::{
    openapi::{schema::SchemaType, ObjectBuilder, RefOr, Schema, Type},
    PartialSchema, ToSchema,
};
use uuid::Uuid;

/// Domain-separation tag hashed ahead of the key when deriving an identity id.
pub const IDENTITY_ID_DOMAIN_TAG: &[u8] = b"avalon-identity-id-v1";

/// Regex (as a string) every canonical identity id matches.
pub const IDENTITY_ID_PATTERN: &str = "^[0-9a-f]{64}$";

const IDENTITY_ID_LEN: usize = 64;

/// Why a string is not a canonical identity id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IdentityIdParseError {
    #[error("identity id must be exactly 64 characters")]
    WrongLength,
    #[error("identity id must be lowercase hex [0-9a-f]")]
    NotLowercaseHex,
}

/// A self-certifying identity id: the 32-byte SHA-256 digest, shown as 64 lowercase hex characters.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IdentityId([u8; 32]);

impl IdentityId {
    /// Strictly parses the canonical form; nothing is trimmed or lowercased.
    pub fn parse(text: &str) -> Result<Self, IdentityIdParseError> {
        if text.len() != IDENTITY_ID_LEN {
            return Err(IdentityIdParseError::WrongLength);
        }
        if !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(IdentityIdParseError::NotLowercaseHex);
        }
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(text, &mut bytes)
            .map_err(|_| IdentityIdParseError::NotLowercaseHex)?;
        Ok(Self(bytes))
    }

    /// The raw 32-byte digest.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The first 16 digest bytes as a UUID: the deterministic WebAuthn user handle.
    pub fn webauthn_user_handle(&self) -> Uuid {
        let mut handle = [0u8; 16];
        handle.copy_from_slice(&self.0[..16]);
        Uuid::from_bytes(handle)
    }

    /// A random id with no known inception key, for tests that never verify a signature.
    #[doc(hidden)]
    pub fn random_for_tests() -> Self {
        let mut seed = [0u8; 32];
        seed[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        derive_identity_id(&seed)
    }

    /// Whether this id is the one derived from `public_key` (a pure hash comparison).
    /// Callers MUST gate the key with [`crate::ed25519_key::parse_ed25519_public_key`] first.
    pub fn matches_key(&self, public_key: &[u8; 32]) -> bool {
        derive_identity_id(public_key) == *self
    }
}

/// Derives the identity id for an inception public key (pure hash, no key checks).
///
/// Callers MUST gate the key with [`crate::ed25519_key::parse_ed25519_public_key`]
/// first; an unchecked weak or non-canonical key still yields an id.
pub fn derive_identity_id(public_key: &[u8; 32]) -> IdentityId {
    let mut hasher = Sha256::new();
    hasher.update(IDENTITY_ID_DOMAIN_TAG);
    hasher.update(public_key);
    IdentityId(hasher.finalize().into())
}

/// Derives the identity id from an already-parsed verifying key.
pub fn derive_identity_id_for_key(key: &VerifyingKey) -> IdentityId {
    derive_identity_id(key.as_bytes())
}

impl fmt::Display for IdentityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for IdentityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IdentityId({self})")
    }
}

impl FromStr for IdentityId {
    type Err = IdentityIdParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for IdentityId {
    type Error = IdentityIdParseError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<IdentityId> for String {
    fn from(id: IdentityId) -> String {
        id.to_string()
    }
}

impl Serialize for IdentityId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for IdentityId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

impl PartialSchema for IdentityId {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(SchemaType::Type(Type::String))
            .pattern(Some(IDENTITY_ID_PATTERN))
            .description(Some(
                "Self-certifying identity id: lowercase hex SHA-256 of the domain tag and the inception public key.",
            ))
            .into()
    }
}

impl ToSchema for IdentityId {}

#[cfg(feature = "sqlx")]
mod sqlx_impls {
    use super::IdentityId;
    use sqlx::encode::IsNull;
    use sqlx::error::BoxDynError;
    use sqlx::postgres::{PgArgumentBuffer, PgHasArrayType, PgTypeInfo, PgValueRef};
    use sqlx::{Decode, Encode, Postgres, Type};

    impl Type<Postgres> for IdentityId {
        fn type_info() -> PgTypeInfo {
            <String as Type<Postgres>>::type_info()
        }
        fn compatible(ty: &PgTypeInfo) -> bool {
            <String as Type<Postgres>>::compatible(ty)
        }
    }

    impl PgHasArrayType for IdentityId {
        fn array_type_info() -> PgTypeInfo {
            <String as PgHasArrayType>::array_type_info()
        }
        fn array_compatible(ty: &PgTypeInfo) -> bool {
            <String as PgHasArrayType>::array_compatible(ty)
        }
    }

    impl Encode<'_, Postgres> for IdentityId {
        fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
            <String as Encode<Postgres>>::encode(self.to_string(), buf)
        }
    }

    impl Decode<'_, Postgres> for IdentityId {
        fn decode(value: PgValueRef<'_>) -> Result<Self, BoxDynError> {
            Ok(IdentityId::parse(value.as_str()?)?)
        }
    }
}

/// Unicode Default_Ignorable_Code_Point plus control characters and the line/paragraph separators:
/// characters that render as nothing (or reorder text) and so can hide an id lookalike.
fn is_invisible(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{061C}'
                | '\u{115F}'..='\u{1160}'
                | '\u{17B4}'..='\u{17B5}'
                | '\u{180B}'..='\u{180F}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{3164}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0000}'..='\u{E0FFF}'
        )
}

/// Whether `name`, after dropping invisible characters, trimming whitespace and ASCII-lowercasing,
/// is shaped like an identity id. Handle lookup is case-insensitive, so such a name could shadow an id.
pub fn display_name_mimics_identity_id(name: &str) -> bool {
    let visible: String = name.chars().filter(|c| !is_invisible(*c)).collect();
    IdentityId::parse(&visible.trim().to_ascii_lowercase()).is_ok()
}

/// Whether a display name may be used at all: no control, bidi-override or zero-width characters
/// (joiners that glue emoji sequences are fine) and not an identity-id lookalike.
pub fn display_name_permitted(name: &str) -> bool {
    let hidden = name
        .chars()
        .any(|c| is_invisible(c) && !matches!(c, '\u{200C}' | '\u{200D}'));
    !hidden && !display_name_mimics_identity_id(name)
}

/// Network id used by [`TestIdentity::created_payload`].
#[doc(hidden)]
pub const TEST_NETWORK_ID: &str = "avalon-test-network";

/// Shard id used by [`TestIdentity::created_payload`].
#[doc(hidden)]
pub const TEST_SHARD_ID: &str = "core";

/// A test identity: a fresh Ed25519 inception key and the id derived from it.
#[doc(hidden)]
#[derive(Clone)]
pub struct TestIdentity {
    pub signing_key: ed25519_dalek::SigningKey,
    pub id: IdentityId,
}

#[doc(hidden)]
impl TestIdentity {
    /// A new identity with a random key.
    pub fn new() -> Self {
        let mut seed = [0u8; 32];
        seed[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        seed[16..].copy_from_slice(Uuid::new_v4().as_bytes());
        Self::from_seed(seed)
    }

    /// The identity whose inception key comes from `seed`.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&seed);
        let id = derive_identity_id_for_key(&signing_key.verifying_key());
        Self { signing_key, id }
    }

    /// The raw inception public key.
    pub fn public_key(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    /// A correctly self-signed `identity.created` v2 payload for [`TEST_NETWORK_ID`], [`TEST_SHARD_ID`] and a fresh ticket.
    pub fn created_payload(
        &self,
        display_name: &str,
    ) -> crate::event_payloads::IdentityCreatedPayload {
        self.created_payload_for(TEST_NETWORK_ID, TEST_SHARD_ID, Uuid::new_v4(), display_name)
    }

    /// A correctly self-signed `identity.created` v2 payload bound to a network, shard and ticket.
    pub fn created_payload_for(
        &self,
        network_id: &str,
        shard_id: &str,
        ticket_id: Uuid,
        display_name: &str,
    ) -> crate::event_payloads::IdentityCreatedPayload {
        use base64::Engine as _;
        use ed25519_dalek::Signer as _;
        let bytes = identity_created_signing_bytes_v2(
            network_id,
            shard_id,
            ticket_id,
            &self.id,
            &self.public_key(),
            display_name,
        );
        crate::event_payloads::IdentityCreatedPayload {
            identity_id: self.id,
            ticket_id,
            display_name: display_name.to_string(),
            public_key: base64::engine::general_purpose::STANDARD.encode(self.public_key()),
            signature: base64::engine::general_purpose::STANDARD
                .encode(self.signing_key.sign(&bytes).to_bytes()),
        }
    }
}

#[doc(hidden)]
impl Default for TestIdentity {
    fn default() -> Self {
        Self::new()
    }
}

/// Bytes signed for `identity.created` v2:
/// `avalon:identity.created:v2:{len(network_id)}:{network_id}:{len(shard_id)}:{shard_id}:{ticket_id}:{identity_id}:{public_key_hex}:{display_name}`
/// (`len` is the decimal UTF-8 byte length). Network and shard ids are variable-width and may
/// contain `:` (`game:slug/1`), so each is length-prefixed and the encoding is unambiguous. The
/// ticket binds the signature to one registration ceremony, the network and the issuing shard to
/// one ledger stream, so a copied payload does not verify in another shard. The display name is
/// last, so a `:` inside it is harmless.
pub fn identity_created_signing_bytes_v2(
    network_id: &str,
    shard_id: &str,
    ticket_id: Uuid,
    identity_id: &IdentityId,
    public_key: &[u8; 32],
    display_name: &str,
) -> Vec<u8> {
    format!(
        "avalon:identity.created:v2:{}:{network_id}:{}:{shard_id}:{ticket_id}:{identity_id}:{}:{display_name}",
        network_id.len(),
        shard_id.len(),
        hex::encode(public_key)
    )
    .into_bytes()
}

/// Bytes the approving device signs for a grant:
/// `avalon:device_grant.approved:v2:{grant_id}:{identity_id}:{requested_public_key_hex}`.
pub fn device_grant_approval_signing_bytes_v2(
    grant_id: Uuid,
    identity_id: &IdentityId,
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
/// Key ids are UUIDs (never containing `:`), so the fields cannot be re-split ambiguously.
pub fn signing_key_revoked_signing_bytes_v2(
    identity_id: &IdentityId,
    signing_key_id: Uuid,
    revoked_by_signing_key_id: Uuid,
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
    pub(crate) fn test_identity(seed: u8) -> (SigningKey, IdentityId) {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let id = derive_identity_id_for_key(&key.verifying_key());
        (key, id)
    }

    #[test]
    fn derived_id_is_64_lowercase_hex_and_round_trips() {
        let (key, id) = test_identity(7);
        assert_eq!(id.to_string().len(), 64);
        assert_eq!(IdentityId::parse(&id.to_string()), Ok(id));
        assert!(id.matches_key(key.verifying_key().as_bytes()));
        assert!(!id.matches_key(&[1u8; 32]));
    }

    #[test]
    fn parse_is_strict() {
        let (_, id) = test_identity(7);
        let s = id.to_string();
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
            assert!(IdentityId::parse(&b).is_err(), "{b}");
        }
    }

    #[test]
    fn differs_from_the_node_shard_id_for_the_same_key() {
        let (key, id) = test_identity(7);
        let shard = derive_self_certifying_id(&key.verifying_key());
        assert_ne!(shard.strip_prefix("node:"), Some(id.to_string().as_str()));
    }

    #[test]
    fn display_names_shaped_like_ids_are_not_permitted() {
        let (_, id) = test_identity(7);
        let lower = id.to_string();
        let padded = format!("\u{200B} {lower}\t");
        let mixed: String = lower
            .chars()
            .enumerate()
            .map(|(i, c)| {
                if i % 2 == 0 {
                    c.to_ascii_uppercase()
                } else {
                    c
                }
            })
            .collect();
        for bad in [
            lower.clone(),
            lower.to_uppercase(),
            mixed,
            padded,
            format!("{lower}\u{FEFF}"),
        ] {
            assert!(!display_name_permitted(&bad), "{bad:?}");
        }
        for ok in [
            lower[..63].to_string(),
            format!("{lower}0"),
            "Zoë 🎮".to_string(),
            "👨\u{200D}👩".to_string(),
        ] {
            assert!(display_name_permitted(&ok), "{ok:?}");
        }
        for gap in [
            '\u{00AD}',
            '\u{034F}',
            '\u{061C}',
            '\u{115F}',
            '\u{1160}',
            '\u{17B4}',
            '\u{17B5}',
            '\u{180B}',
            '\u{180F}',
            '\u{200B}',
            '\u{200F}',
            '\u{2028}',
            '\u{2029}',
            '\u{202A}',
            '\u{202E}',
            '\u{2060}',
            '\u{206F}',
            '\u{3164}',
            '\u{FE00}',
            '\u{FE0F}',
            '\u{FEFF}',
            '\u{FFA0}',
            '\u{1BCA0}',
            '\u{1BCA3}',
            '\u{1D173}',
            '\u{1D17A}',
            '\u{E0000}',
            '\u{E0FFF}',
            '\u{2003}',
            '\u{00A0}',
        ] {
            let interior = format!("{}{gap}{}", &lower[..30], &lower[30..]);
            assert!(
                !display_name_permitted(&format!("{lower}{gap}")),
                "suffix {gap:?}"
            );
            assert!(
                !display_name_permitted(&format!("{gap}{lower}")),
                "prefix {gap:?}"
            );
            // Interior visible whitespace is a visibly different name; only invisible gaps hide.
            if !gap.is_whitespace() || is_invisible(gap) {
                assert!(!display_name_permitted(&interior), "interior {gap:?}");
            }
        }
        assert!(!display_name_permitted("a\u{202E}b"));
        assert!(!display_name_permitted("line\nbreak"));
    }

    #[test]
    fn network_and_shard_boundaries_are_unambiguous() {
        let (key, id) = test_identity(7);
        let pk = key.verifying_key().to_bytes();
        let t = Uuid::nil();
        let a = identity_created_signing_bytes_v2("a:b", "c", t, &id, &pk, "n");
        let b = identity_created_signing_bytes_v2("a", "b:c", t, &id, &pk, "n");
        let c = identity_created_signing_bytes_v2("a", "b", t, &id, &pk, "c:n");
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn serde_enforces_the_canonical_form() {
        let (_, id) = test_identity(7);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<IdentityId>(&json).unwrap(), id);
        assert!(serde_json::from_str::<IdentityId>(&json.to_uppercase()).is_err());
    }

    #[test]
    fn signing_bytes_have_the_documented_layout() {
        let (key, id) = test_identity(7);
        let pk = key.verifying_key().to_bytes();
        let created =
            identity_created_signing_bytes_v2("net", "game:x/1", Uuid::nil(), &id, &pk, "a:b");
        assert_eq!(
            String::from_utf8(created).unwrap(),
            format!(
                "avalon:identity.created:v2:3:net:8:game:x/1:{}:{id}:{}:a:b",
                Uuid::nil(),
                hex::encode(pk)
            )
        );
        let grant = device_grant_approval_signing_bytes_v2(Uuid::nil(), &id, &pk);
        assert!(String::from_utf8(grant)
            .unwrap()
            .starts_with("avalon:device_grant.approved:v2:00000000-"));
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let revoked = signing_key_revoked_signing_bytes_v2(&id, a, b);
        assert!(String::from_utf8(revoked)
            .unwrap()
            .ends_with(&format!("{id}:{a}:{b}")));
        let swapped = signing_key_revoked_signing_bytes_v2(&id, b, a);
        assert_ne!(signing_key_revoked_signing_bytes_v2(&id, a, b), swapped);
    }
}
