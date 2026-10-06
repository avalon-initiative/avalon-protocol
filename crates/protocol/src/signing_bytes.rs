//! Structured signing bytes: one fixed binary layout for every signed or hashed
//! message, so no field can shift the boundaries of another.
//!
//! Layout: `tag` (ASCII, from [`tags`]) | `version: u16 BE` | fields in a fixed order.
//! Strings and byte strings are `u32 BE` length + bytes; keys and hashes are raw
//! fixed-width bytes; integers are fixed-width big-endian. Nothing is
//! self-describing: the layout of a `(tag, version)` is the order of calls, and a
//! new field means a new version. Hashed JSON payloads are a different rule (#1308).
//!
//! The tag has no length prefix (like the existing witness and STH layouts), so
//! registered tags must be prefix-free; [`tags`] tests that. Exception: `avalon.ledger.entry` uses
//! the version slot for the event's own `version`, so a changed field set there needs a new tag. A tag is never
//! reused for another kind, and its version lives in the `u16`, not in the tag.
//!
//! # Migrating a layout (recipe for the follow-up slices)
//!
//! 1. Pick or add the kind's tag in [`tags`] (one per signed kind, lowercase
//!    `avalon.<area>.<kind>`); the uniqueness test must stay green.
//! 2. Replace the `format!` / `extend_from_slice` body with a [`Builder`] chain,
//!    ids and strings via `str`, public keys and hashes via `key`/`hash`/`fixed`
//!    (raw bytes, never hex text), times as `i64` unix seconds, UUIDs via `uuid`.
//!    Start at version `1`; the old text layout is deleted, not kept (no shims).
//!    `fixed` takes only widths the layout fixes, never caller-sized data.
//! 3. Verifiers parse with [`Reader`] when they need the fields back, or rebuild
//!    the bytes from the parsed request and compare; never split on a delimiter.
//! 4. Replace the layout's conformance vector with exact `signingBytesHex` and
//!    signature, add boundary cases (empty, `:`, `,`, NUL, multi-byte UTF-8), and
//!    mirror it in the SDK vector directories in the same change.
//!
//! Slices: `cross_node_login`, `continuation`,
//! `interest_claim`, `achievements` (include `issued_at` where it is signed),
//! `signature_gate::canonical_message` (tag `signature_gate.action`, the action
//! name as the first `str` field, then the action's fields), the integrator nonce
//! challenge (has no tag today) and `issuer_registration`. The existing binary
//! layouts (`sth`, `witness`, `shard_identity`, `node_request`, identity chain
//! hash) keep their `avalon-...-v1` tags and are not part of this recipe.

use thiserror::Error;
use uuid::Uuid;

/// Why building or reading structured signing bytes failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SigningBytesError {
    #[error("a field is longer than u32::MAX bytes")]
    FieldTooLong,
    #[error("message does not start with the expected domain tag")]
    TagMismatch,
    #[error("message ends before the field does")]
    Truncated,
    #[error("string field is not valid UTF-8")]
    InvalidUtf8,
    #[error("bytes remain after the last field")]
    TrailingBytes,
}

/// The registry of domain tags: one distinct tag per signed kind.
pub mod tags {
    use super::DomainTag;

    pub const IDENTITY_CREATED: DomainTag = DomainTag::new("avalon.identity.created");
    pub const DEVICE_GRANT_APPROVED: DomainTag = DomainTag::new("avalon.device_grant.approved");
    pub const IDENTITY_SIGNING_KEY_REVOKED: DomainTag =
        DomainTag::new("avalon.identity.signing_key_revoked");
    pub const CROSS_NODE_LOGIN: DomainTag = DomainTag::new("avalon.cross_node_login");
    pub const SESSION_CONTINUATION: DomainTag = DomainTag::new("avalon.session_continuation");
    pub const INTEREST_CLAIM: DomainTag = DomainTag::new("avalon.interest_claim");
    pub const ATTESTATION_ISSUE: DomainTag = DomainTag::new("avalon.attestation.issue");
    pub const ATTESTATION_BULK_ISSUE: DomainTag = DomainTag::new("avalon.attestation.bulk_issue");
    pub const ATTESTATION_REVOKE: DomainTag = DomainTag::new("avalon.attestation.revoke");
    pub const ISSUER_REGISTERED: DomainTag = DomainTag::new("avalon.issuer.registered");
    pub const SIGNATURE_GATE_ACTION: DomainTag = DomainTag::new("avalon.signature_gate.action");
    pub const INTEGRATOR_NONCE_CHALLENGE: DomainTag =
        DomainTag::new("avalon.integrator.nonce_challenge");
    pub const LEDGER_ENTRY: DomainTag = DomainTag::new("avalon.ledger.entry");
    /// Reserved for conformance vectors; no key ever signs it in production.
    pub const CONFORMANCE: DomainTag = DomainTag::new("avalon.conformance.vector");

    /// Every registered tag; the uniqueness test walks this list.
    pub const ALL: &[DomainTag] = &[
        IDENTITY_CREATED,
        DEVICE_GRANT_APPROVED,
        IDENTITY_SIGNING_KEY_REVOKED,
        CROSS_NODE_LOGIN,
        SESSION_CONTINUATION,
        INTEREST_CLAIM,
        ATTESTATION_ISSUE,
        ATTESTATION_BULK_ISSUE,
        ATTESTATION_REVOKE,
        ISSUER_REGISTERED,
        SIGNATURE_GATE_ACTION,
        INTEGRATOR_NONCE_CHALLENGE,
        LEDGER_ENTRY,
        CONFORMANCE,
    ];
}

/// A registered domain tag. Only [`tags`] can construct one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DomainTag(&'static str);

impl DomainTag {
    /// Checked at compile time: 1 to 255 bytes of `[a-z0-9._]`.
    const fn new(tag: &'static str) -> Self {
        let bytes = tag.as_bytes();
        assert!(!bytes.is_empty() && bytes.len() <= 255, "bad tag length");
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            assert!(
                b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_',
                "tag bytes must be [a-z0-9._]"
            );
            i += 1;
        }
        Self(tag)
    }

    pub const fn as_str(&self) -> &'static str {
        self.0
    }

    pub const fn as_bytes(&self) -> &'static [u8] {
        self.0.as_bytes()
    }
}

fn length_prefix(len: usize) -> Result<u32, SigningBytesError> {
    u32::try_from(len).map_err(|_| SigningBytesError::FieldTooLong)
}

/// Writes fields after the tag and version, in call order. The first error
/// sticks and is returned by [`Builder::finish`].
#[derive(Debug)]
pub struct Builder {
    out: Vec<u8>,
    error: Option<SigningBytesError>,
}

impl Builder {
    pub fn new(tag: DomainTag, version: u16) -> Self {
        let mut out = Vec::with_capacity(tag.as_bytes().len() + 2);
        out.extend_from_slice(tag.as_bytes());
        out.extend_from_slice(&version.to_be_bytes());
        Self { out, error: None }
    }

    /// UTF-8 text, `u32 BE` length then bytes.
    pub fn str(self, value: &str) -> Self {
        self.bytes(value.as_bytes())
    }

    /// Variable-length bytes, `u32 BE` length then bytes.
    pub fn bytes(mut self, value: &[u8]) -> Self {
        match length_prefix(value.len()) {
            Ok(len) => {
                self.out.extend_from_slice(&len.to_be_bytes());
                self.out.extend_from_slice(value);
            }
            Err(e) => self.error = self.error.or(Some(e)),
        }
        self
    }

    /// A fixed-width field written raw with no length (`N` is part of the layout).
    pub fn fixed<const N: usize>(mut self, value: &[u8; N]) -> Self {
        self.out.extend_from_slice(value);
        self
    }

    /// A 32-byte public key, raw.
    pub fn key(self, value: &[u8; 32]) -> Self {
        self.fixed(value)
    }

    /// A 32-byte hash, raw.
    pub fn hash(self, value: &[u8; 32]) -> Self {
        self.fixed(value)
    }

    /// A UUID as its 16 raw bytes.
    pub fn uuid(self, value: Uuid) -> Self {
        self.fixed(value.as_bytes())
    }

    pub fn u8(self, value: u8) -> Self {
        self.fixed(&[value])
    }

    pub fn u16(self, value: u16) -> Self {
        self.fixed(&value.to_be_bytes())
    }

    pub fn u32(self, value: u32) -> Self {
        self.fixed(&value.to_be_bytes())
    }

    pub fn u64(self, value: u64) -> Self {
        self.fixed(&value.to_be_bytes())
    }

    pub fn i64(self, value: i64) -> Self {
        self.fixed(&value.to_be_bytes())
    }

    pub fn finish(self) -> Result<Vec<u8>, SigningBytesError> {
        match self.error {
            Some(e) => Err(e),
            None => Ok(self.out),
        }
    }
}

/// Reads fields in the order the layout defines. Never allocates from a
/// declared length before checking it fits in the remaining bytes.
#[derive(Debug)]
pub struct Reader<'a> {
    rest: &'a [u8],
    version: u16,
}

impl<'a> Reader<'a> {
    /// Checks the tag and reads the version.
    pub fn new(tag: DomainTag, message: &'a [u8]) -> Result<Self, SigningBytesError> {
        let rest = message
            .strip_prefix(tag.as_bytes())
            .ok_or(SigningBytesError::TagMismatch)?;
        let mut reader = Self { rest, version: 0 };
        reader.version = u16::from_be_bytes(reader.take_array()?);
        Ok(reader)
    }

    /// The reader accepts any version; the caller must check it before reading fields.
    pub fn version(&self) -> u16 {
        self.version
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], SigningBytesError> {
        if self.rest.len() < len {
            return Err(SigningBytesError::Truncated);
        }
        let (head, tail) = self.rest.split_at(len);
        self.rest = tail;
        Ok(head)
    }

    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], SigningBytesError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    pub fn bytes(&mut self) -> Result<&'a [u8], SigningBytesError> {
        let len = u32::from_be_bytes(self.take_array()?);
        self.take(len as usize)
    }

    pub fn str(&mut self) -> Result<&'a str, SigningBytesError> {
        std::str::from_utf8(self.bytes()?).map_err(|_| SigningBytesError::InvalidUtf8)
    }

    pub fn fixed<const N: usize>(&mut self) -> Result<[u8; N], SigningBytesError> {
        self.take_array()
    }

    pub fn uuid(&mut self) -> Result<Uuid, SigningBytesError> {
        Ok(Uuid::from_bytes(self.take_array()?))
    }

    pub fn u8(&mut self) -> Result<u8, SigningBytesError> {
        Ok(u8::from_be_bytes(self.take_array()?))
    }

    pub fn u16(&mut self) -> Result<u16, SigningBytesError> {
        Ok(u16::from_be_bytes(self.take_array()?))
    }

    pub fn u32(&mut self) -> Result<u32, SigningBytesError> {
        Ok(u32::from_be_bytes(self.take_array()?))
    }

    pub fn u64(&mut self) -> Result<u64, SigningBytesError> {
        Ok(u64::from_be_bytes(self.take_array()?))
    }

    pub fn i64(&mut self) -> Result<i64, SigningBytesError> {
        Ok(i64::from_be_bytes(self.take_array()?))
    }

    /// Errors when any bytes remain, so a message cannot carry hidden trailing data.
    pub fn finish(self) -> Result<(), SigningBytesError> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(SigningBytesError::TrailingBytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tags of the layouts that keep their own `avalon-...` / `avalon:...` form.
    const EXISTING_TAGS: &[&str] = &[
        "avalon-settlement-sth-v1",
        "avalon-witness-cosign-v1",
        "avalon-witness-announce-v1",
        "avalon-name-binding-v1",
        "avalon-node-request-v1",
        "avalon-identity-chain-v1",
        "avalon-identity-id-v1",
        "avalon-name-proof-v1",
        "avalon-shard-route-v1",
        "avalon:",
    ];

    #[test]
    fn tags_are_unique_and_prefix_free() {
        for (i, a) in tags::ALL.iter().enumerate() {
            for b in &tags::ALL[i + 1..] {
                assert!(
                    !a.as_bytes().starts_with(b.as_bytes())
                        && !b.as_bytes().starts_with(a.as_bytes()),
                    "{} and {} collide or share a prefix",
                    a.as_str(),
                    b.as_str()
                );
            }
        }
    }

    #[test]
    fn tags_never_collide_with_existing_layouts() {
        for tag in tags::ALL {
            for existing in EXISTING_TAGS {
                assert!(!tag.as_str().starts_with(existing) && !existing.starts_with(tag.as_str()));
            }
            assert!(tag.as_str().starts_with("avalon."));
        }
    }

    #[test]
    fn round_trips_every_field_type() {
        let id = Uuid::from_u128(7);
        let msg = Builder::new(tags::CONFORMANCE, 3)
            .str("a:b,c\0d")
            .bytes(&[])
            .key(&[9; 32])
            .uuid(id)
            .u8(1)
            .u16(2)
            .u32(3)
            .u64(u64::MAX)
            .i64(i64::MIN)
            .finish()
            .unwrap();
        let mut r = Reader::new(tags::CONFORMANCE, &msg).unwrap();
        assert_eq!(r.version(), 3);
        assert_eq!(r.str().unwrap(), "a:b,c\0d");
        assert_eq!(r.bytes().unwrap(), b"");
        assert_eq!(r.fixed::<32>().unwrap(), [9; 32]);
        assert_eq!(r.uuid().unwrap(), id);
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.u16().unwrap(), 2);
        assert_eq!(r.u32().unwrap(), 3);
        assert_eq!(r.u64().unwrap(), u64::MAX);
        assert_eq!(r.i64().unwrap(), i64::MIN);
        r.finish().unwrap();
    }

    #[test]
    fn moving_a_delimiter_between_fields_changes_the_bytes() {
        let a = Builder::new(tags::CONFORMANCE, 1)
            .str("a:b")
            .str("c")
            .finish();
        let b = Builder::new(tags::CONFORMANCE, 1)
            .str("a")
            .str("b:c")
            .finish();
        assert_ne!(a.unwrap(), b.unwrap());
    }

    #[test]
    fn reader_rejects_malformed_input() {
        let ok = Builder::new(tags::CONFORMANCE, 1)
            .str("ab")
            .finish()
            .unwrap();
        let wrong = Reader::new(tags::INTEREST_CLAIM, &ok).unwrap_err();
        assert_eq!(wrong, SigningBytesError::TagMismatch);

        let mut r = Reader::new(tags::CONFORMANCE, &ok[..ok.len() - 1]).unwrap();
        assert_eq!(r.str().unwrap_err(), SigningBytesError::Truncated);

        let r = Reader::new(tags::CONFORMANCE, &ok).unwrap();
        assert_eq!(r.finish().unwrap_err(), SigningBytesError::TrailingBytes);

        let bad = Builder::new(tags::CONFORMANCE, 1)
            .bytes(&[0xff])
            .finish()
            .unwrap();
        let mut r = Reader::new(tags::CONFORMANCE, &bad).unwrap();
        assert_eq!(r.str().unwrap_err(), SigningBytesError::InvalidUtf8);

        let mut huge = tags::CONFORMANCE.as_bytes().to_vec();
        huge.extend_from_slice(&[0, 1, 0xff, 0xff, 0xff, 0xff, b'x']);
        let mut r = Reader::new(tags::CONFORMANCE, &huge).unwrap();
        assert_eq!(r.bytes().unwrap_err(), SigningBytesError::Truncated);

        assert_eq!(
            Reader::new(tags::CONFORMANCE, tags::CONFORMANCE.as_bytes()).unwrap_err(),
            SigningBytesError::Truncated
        );
    }

    #[test]
    fn length_prefix_rejects_over_u32() {
        assert_eq!(length_prefix(u32::MAX as usize), Ok(u32::MAX));
        #[cfg(target_pointer_width = "64")]
        assert_eq!(
            length_prefix(u32::MAX as usize + 1),
            Err(SigningBytesError::FieldTooLong)
        );
    }
}
