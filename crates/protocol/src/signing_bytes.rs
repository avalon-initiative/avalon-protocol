//! Structured signing bytes: one fixed binary layout for every signed or hashed
//! message, so no field can shift the boundaries of another, and an old node can tell a
//! message it cannot verify from one it can.
//!
//! Every layout is `header | fields | extensions`:
//!
//! | part | encoding |
//! | --- | --- |
//! | domain tag | ASCII from [`tags`], no length prefix, so tags must be prefix-free |
//! | `layout_version` | `u16 BE`, the layout's own version; a payload's event version is an explicit `u32` field |
//! | `rules_version` | `u32 BE`, the rules the author wrote under ([`RULES_VERSION`]; readers accept [`RULES_VERSION_MIN`]..=[`RULES_VERSION_MAX`]) |
//! | fields | fixed order; strings and byte strings are `u32 BE` length + bytes, keys and hashes raw, integers big-endian |
//! | `hash_algo` | `u8` ([`HashAlgo`], `0x01` = SHA-256) immediately before the first hash-valued field, in layouts that contain a hash |
//! | key, signature | `alg u8` (`0x01` = Ed25519) then the raw bytes; written only by [`Builder::key`] / [`Builder::signature`] |
//! | extensions | `count u16`, then `ext_type u16`, `flags u8`, `len u32`, value; see [`Extensions`] |
//!
//! Extensions are always last. Entries are strictly ascending by `ext_type`, flags bit 0 is
//! "critical" and every other bit must be zero, and the entries total at most
//! [`Caps::max_extension_bytes`] of [`caps_for`]. Every consensus cap is a function of the
//! message's `rules_version`: a cap is only ever raised for entries authored under a newer rules
//! version, so a lower cap never invalidates an older entry. An unknown non-critical extension is hashed and preserved byte for byte.
//! An unknown critical extension, a layout version above [`layout_versions`], a rules version above
//! [`RULES_VERSION_MAX`], an unknown hash algorithm or an unknown key or signature algorithm is
//! [`SigningBytesError::NeedsNewerVersion`]: nothing is verified, and the error names what is
//! required. Lower layout versions of a known tag stay verifiable: [`layout_versions`] is the
//! per-tag table of supported versions, and a new field set is a new `layout_version`.
//!
//! Nothing is self-describing beyond that: the layout of a `(tag, layout_version)` is the order of
//! calls. Hashed JSON payloads are a different rule.
//!
//! Name binding and node request keep their own length-prefixed `avalon-...-v1` layouts and are not
//! part of this envelope. The tags whose code still builds the legacy colon text
//! (`cross_node_login`, `session_continuation`, `interest_claim`, the attestation, issuer
//! registration, signature gate and integrator nonce tags) are registered with their layout
//! range here so the header applies the moment they migrate.
//!
//! # Migrating a layout (recipe for the follow-up slices)
//!
//! 1. Pick or add the kind's tag in [`tags`] (one per signed kind, lowercase
//!    `avalon.<area>.<kind>`) and its row in [`layout_versions`]; the uniqueness test must stay green.
//! 2. Replace the `format!` / `extend_from_slice` body with a [`Builder`] chain,
//!    ids and strings via `str`, public keys and hashes via `key`/`hash`/`fixed`
//!    (raw bytes, never hex text), times as `i64` unix seconds, UUIDs via `uuid`, and
//!    [`Builder::hash_algo`] before the first hash. Start at layout version `1`; the old text
//!    layout is deleted, not kept (no shims). `fixed` takes only widths the layout fixes.
//! 3. Verifiers parse with [`Reader`] when they need the fields back, or rebuild
//!    the bytes from the stored [`Envelope`] and fields and compare; never split on a delimiter.
//! 4. Replace the layout's conformance vector with exact `signingBytesHex` and
//!    signature, add boundary cases (empty, `:`, `,`, NUL, multi-byte UTF-8), and
//!    mirror it in the SDK vector directories in the same change.
//!
//! Remaining slices: `cross_node_login`, `continuation`, `interest_claim`, `achievements`
//! (include `issued_at` where it is signed), `signature_gate::canonical_message` (tag
//! `signature_gate.action`, the action name as the first `str` field, then the action's fields),
//! the integrator nonce challenge and `issuer_registration`.

use std::ops::RangeInclusive;

use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

/// The rules version this node authors under. Emitted in every header.
pub const RULES_VERSION: u32 = 1;
/// The lowest rules version a reader still verifies.
pub const RULES_VERSION_MIN: u32 = 1;
/// The highest rules version a reader understands; anything above is [`SigningBytesError::NeedsNewerVersion`].
pub const RULES_VERSION_MAX: u32 = 1;

const EXTENSION_ENTRY_HEADER: usize = 7;
const CRITICAL_FLAG: u8 = 0x01;

/// Consensus size caps, always read through [`caps_for`] for the message's own rules version.
/// A cap may be raised only for entries authored under a newer rules version; a lower cap never
/// invalidates what an older rules version already allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// Upper bound on the extension entries of one message (each entry counts its 7 header bytes).
    pub max_extension_bytes: usize,
}

/// `(first rules version, caps)` rows, ascending. A version uses the last row at or below it.
const CAPS_TABLE: &[(u32, Caps)] = &[(
    1,
    Caps {
        max_extension_bytes: 4096,
    },
)];

/// The caps that apply to a message authored under `rules_version`.
pub fn caps_for(rules_version: u32) -> Caps {
    caps_in(CAPS_TABLE, rules_version)
}

fn caps_in(table: &[(u32, Caps)], rules_version: u32) -> Caps {
    table
        .iter()
        .rev()
        .find(|(from, _)| *from <= rules_version)
        .or(table.first())
        .map(|(_, caps)| *caps)
        .expect("caps table has a row")
}

/// Which part of a message needs a newer version than this node has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionKind {
    Layout,
    Rules,
    HashAlgo,
    /// An unknown key or signature algorithm tag.
    SigAlgo,
    /// A critical extension this node does not understand; `required` is its `ext_type`.
    CriticalExtension,
}

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
    /// Nothing was verified: the message needs a layout, rules version, hash algorithm or
    /// extension this node does not have.
    #[error("needs a newer version: {what:?} {required}")]
    NeedsNewerVersion { what: VersionKind, required: u32 },
    /// A layout or rules version below what this node still supports.
    #[error("unsupported {what:?} {value}")]
    UnsupportedVersion { what: VersionKind, value: u32 },
    #[error("extensions are not in ascending ext_type order")]
    ExtensionsUnsorted,
    #[error("duplicate extension type {0}")]
    ExtensionDuplicate(u16),
    #[error("extension flags {0:#04x} set a reserved bit")]
    ExtensionReservedFlags(u8),
    #[error("extensions exceed the size bound")]
    ExtensionsTooLarge,
}

impl SigningBytesError {
    /// The typed "needs a newer version" result, when that is what this error is.
    pub fn needs_newer_version(&self) -> Option<(VersionKind, u32)> {
        match self {
            Self::NeedsNewerVersion { what, required } => Some((*what, *required)),
            _ => None,
        }
    }
}

/// Hash algorithm named by a layout's `hash_algo` byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlgo {
    Sha256,
    /// A second algorithm that exists only so tests can exercise mismatches; never on the wire.
    #[cfg(test)]
    SyntheticTest,
}

impl HashAlgo {
    pub const fn id(self) -> u8 {
        match self {
            Self::Sha256 => 0x01,
            #[cfg(test)]
            Self::SyntheticTest => 0x7f,
        }
    }

    /// Any other value is a hash algorithm this node does not know.
    pub fn from_id(id: u8) -> Result<Self, SigningBytesError> {
        match id {
            0x01 => Ok(Self::Sha256),
            other => Err(SigningBytesError::NeedsNewerVersion {
                what: VersionKind::HashAlgo,
                required: u32::from(other),
            }),
        }
    }

    pub fn digest(self, data: &[u8]) -> [u8; 32] {
        match self {
            Self::Sha256 => Sha256::digest(data).into(),
            #[cfg(test)]
            Self::SyntheticTest => Sha256::digest(data).into(),
            #[cfg(test)]
            Self::SyntheticTest => Sha256::digest(data).into(),
        }
    }
}

/// Algorithm tag written next to every public key and signature (`0x01` = Ed25519).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigAlgo {
    Ed25519,
}

impl SigAlgo {
    pub const fn id(self) -> u8 {
        match self {
            Self::Ed25519 => 0x01,
        }
    }

    /// Any other value is an algorithm this node does not know.
    pub fn from_id(id: u8) -> Result<Self, SigningBytesError> {
        match id {
            0x01 => Ok(Self::Ed25519),
            other => Err(SigningBytesError::NeedsNewerVersion {
                what: VersionKind::SigAlgo,
                required: u32::from(other),
            }),
        }
    }
}

/// One extension entry. Reserved flag bits are not representable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    pub ext_type: u16,
    pub critical: bool,
    pub value: Vec<u8>,
}

/// The extensions region: entries strictly ascending by type, bounded by [`Caps::max_extension_bytes`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extensions(Vec<Extension>);

impl Extensions {
    pub fn none() -> Self {
        Self::default()
    }

    /// Validates order, uniqueness and the size bound for `rules_version`.
    pub fn new(entries: Vec<Extension>, rules_version: u32) -> Result<Self, SigningBytesError> {
        let max = caps_for(rules_version).max_extension_bytes;
        let mut total = 0usize;
        for (i, e) in entries.iter().enumerate() {
            total = total.saturating_add(EXTENSION_ENTRY_HEADER.saturating_add(e.value.len()));
            if total > max {
                return Err(SigningBytesError::ExtensionsTooLarge);
            }
            if let Some(prev) = i.checked_sub(1).map(|j| &entries[j]) {
                if prev.ext_type == e.ext_type {
                    return Err(SigningBytesError::ExtensionDuplicate(e.ext_type));
                }
                if prev.ext_type > e.ext_type {
                    return Err(SigningBytesError::ExtensionsUnsorted);
                }
            }
        }
        if entries.len() > usize::from(u16::MAX) {
            return Err(SigningBytesError::ExtensionsTooLarge);
        }
        Ok(Self(entries))
    }

    pub fn entries(&self) -> &[Extension] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The region as hashed: `count u16` then each entry.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(self.0.len() as u16).to_be_bytes());
        for e in &self.0 {
            out.extend_from_slice(&e.ext_type.to_be_bytes());
            out.push(if e.critical { CRITICAL_FLAG } else { 0 });
            out.extend_from_slice(&(e.value.len() as u32).to_be_bytes());
            out.extend_from_slice(&e.value);
        }
        out
    }

    /// Parses a whole region; bytes after the last entry are an error.
    pub fn decode(bytes: &[u8], rules_version: u32) -> Result<Self, SigningBytesError> {
        let mut reader = ByteCursor { rest: bytes };
        let ext = reader.extensions(rules_version)?;
        if reader.rest.is_empty() {
            Ok(ext)
        } else {
            Err(SigningBytesError::TrailingBytes)
        }
    }

    /// Errors with the typed result for the first critical extension `tag` does not understand.
    pub fn require_understood(&self, tag: DomainTag) -> Result<(), SigningBytesError> {
        match self
            .0
            .iter()
            .find(|e| e.critical && !understood_extensions(tag).contains(&e.ext_type))
        {
            Some(e) => Err(SigningBytesError::NeedsNewerVersion {
                what: VersionKind::CriticalExtension,
                required: u32::from(e.ext_type),
            }),
            None => Ok(()),
        }
    }
}

/// Critical extension types each tag understands. None are defined yet.
pub fn understood_extensions(_tag: DomainTag) -> &'static [u16] {
    &[]
}

/// The header values of a stored or received message, recorded next to it so verification
/// never guesses a layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub layout_version: u16,
    pub rules_version: u32,
    pub hash_algo: HashAlgo,
    pub extensions: Extensions,
}

impl Envelope {
    /// What this node authors for `tag`: its newest layout, [`RULES_VERSION`], SHA-256, no extensions.
    pub fn current(tag: DomainTag) -> Self {
        Self {
            layout_version: *layout_versions(tag).end(),
            rules_version: RULES_VERSION,
            hash_algo: HashAlgo::Sha256,
            extensions: Extensions::none(),
        }
    }

    /// Rebuilds from stored or wire values, checking `layout_version` and `rules_version`
    /// against this node's ranges, the hash algorithm and the extension region.
    pub fn from_parts(
        tag: DomainTag,
        layout_version: u16,
        rules_version: u32,
        hash_algo: u8,
        extensions: &[u8],
    ) -> Result<Self, SigningBytesError> {
        check_layout(tag, layout_version)?;
        check_rules(rules_version)?;
        let extensions = Extensions::decode(extensions, rules_version)?;
        extensions.require_understood(tag)?;
        Ok(Self {
            layout_version,
            rules_version,
            hash_algo: HashAlgo::from_id(hash_algo)?,
            extensions,
        })
    }
}

/// An [`Envelope`] as it travels in JSON and sits in stored rows: plain integers and hex.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnvelopeWire {
    pub layout_version: u16,
    pub rules_version: u32,
    pub hash_algo: u8,
    /// Hex of the extensions region; `0000` when there are none.
    pub extensions: String,
}

impl From<&Envelope> for EnvelopeWire {
    fn from(envelope: &Envelope) -> Self {
        Self {
            layout_version: envelope.layout_version,
            rules_version: envelope.rules_version,
            hash_algo: envelope.hash_algo.id(),
            extensions: hex::encode(envelope.extensions.encode()),
        }
    }
}

impl EnvelopeWire {
    /// Checks every part against this node's ranges; the typed "needs a newer version" result
    /// for anything above them.
    pub fn to_envelope(&self, tag: DomainTag) -> Result<Envelope, SigningBytesError> {
        let extensions = hex::decode(&self.extensions).map_err(|_| SigningBytesError::Truncated)?;
        Envelope::from_parts(
            tag,
            self.layout_version,
            self.rules_version,
            self.hash_algo,
            &extensions,
        )
    }
}

/// Supported layout versions per tag. Lower versions stay verifiable forever; a tag's first
/// version is 1. Every registered tag has a row (a test enforces it).
pub fn layout_versions(tag: DomainTag) -> RangeInclusive<u16> {
    // The conformance tag spans 1..=2 so vectors can show an older layout verifying beside the newest.
    if tag == tags::CONFORMANCE {
        return 1..=2;
    }
    assert!(tags::ALL.contains(&tag), "unregistered tag");
    1..=1
}

fn check_layout(tag: DomainTag, version: u16) -> Result<(), SigningBytesError> {
    let range = layout_versions(tag);
    if version > *range.end() {
        Err(SigningBytesError::NeedsNewerVersion {
            what: VersionKind::Layout,
            required: u32::from(version),
        })
    } else if version < *range.start() {
        Err(SigningBytesError::UnsupportedVersion {
            what: VersionKind::Layout,
            value: u32::from(version),
        })
    } else {
        Ok(())
    }
}

fn check_rules(version: u32) -> Result<(), SigningBytesError> {
    if version > RULES_VERSION_MAX {
        Err(SigningBytesError::NeedsNewerVersion {
            what: VersionKind::Rules,
            required: version,
        })
    } else if version < RULES_VERSION_MIN {
        Err(SigningBytesError::UnsupportedVersion {
            what: VersionKind::Rules,
            value: version,
        })
    } else {
        Ok(())
    }
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
    pub const IDENTITY_CHAIN_EVENT: DomainTag = DomainTag::new("avalon.identity.chain_event");
    pub const SETTLEMENT_STH: DomainTag = DomainTag::new("avalon.settlement.sth");
    pub const WITNESS_COSIGN: DomainTag = DomainTag::new("avalon.witness.cosign");
    pub const WITNESS_ANNOUNCE: DomainTag = DomainTag::new("avalon.witness.announce");
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
        IDENTITY_CHAIN_EVENT,
        SETTLEMENT_STH,
        WITNESS_COSIGN,
        WITNESS_ANNOUNCE,
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

/// Writes the header, then fields in call order, then the extensions region. The first error
/// sticks and is returned by [`Builder::finish`].
#[derive(Debug)]
pub struct Builder {
    tag: DomainTag,
    rules_version: u32,
    out: Vec<u8>,
    extensions: Vec<Extension>,
    error: Option<SigningBytesError>,
}

impl Builder {
    /// A header at this node's [`RULES_VERSION`].
    pub fn new(tag: DomainTag, layout_version: u16) -> Self {
        Self::with_rules(tag, layout_version, RULES_VERSION)
    }

    /// A header with an explicit rules version, for rebuilding a stored message.
    pub fn with_rules(tag: DomainTag, layout_version: u16, rules_version: u32) -> Self {
        let mut out = Vec::with_capacity(tag.as_bytes().len() + 6);
        out.extend_from_slice(tag.as_bytes());
        out.extend_from_slice(&layout_version.to_be_bytes());
        out.extend_from_slice(&rules_version.to_be_bytes());
        let error = check_layout(tag, layout_version)
            .and_then(|()| check_rules(rules_version))
            .err();
        Self {
            tag,
            rules_version,
            out,
            extensions: Vec::new(),
            error,
        }
    }

    /// The header and extensions of a stored [`Envelope`]; its `hash_algo` is written by
    /// [`Builder::hash_algo`] where the layout places it.
    pub fn with_envelope(tag: DomainTag, envelope: &Envelope) -> Self {
        let mut builder = Self::with_rules(tag, envelope.layout_version, envelope.rules_version);
        builder.extensions = envelope.extensions.0.clone();
        builder
    }

    /// The hash algorithm byte; call it immediately before the first hash-valued field.
    pub fn hash_algo(self, algo: HashAlgo) -> Self {
        self.u8(algo.id())
    }

    /// Adds an extension. Entries must be added in ascending `ext_type` order.
    pub fn extension(mut self, ext_type: u16, critical: bool, value: &[u8]) -> Self {
        self.extensions.push(Extension {
            ext_type,
            critical,
            value: value.to_vec(),
        });
        self
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

    /// An Ed25519 public key: `alg u8` then 32 raw bytes. The only place keys are written.
    pub fn key(self, value: &[u8; 32]) -> Self {
        self.u8(SigAlgo::Ed25519.id()).fixed(value)
    }

    /// An Ed25519 signature: `alg u8` then 64 raw bytes. The only place signatures are written.
    pub fn signature(self, value: &[u8; 64]) -> Self {
        self.u8(SigAlgo::Ed25519.id()).fixed(value)
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

    /// Appends the extensions region and returns the bytes. Fails with the typed result when
    /// a critical extension is not understood for the tag.
    pub fn finish(mut self) -> Result<Vec<u8>, SigningBytesError> {
        if let Some(e) = self.error {
            return Err(e);
        }
        let extensions = Extensions::new(self.extensions, self.rules_version)?;
        extensions.require_understood(self.tag)?;
        self.out.extend_from_slice(&extensions.encode());
        Ok(self.out)
    }
}

#[derive(Debug)]
struct ByteCursor<'a> {
    rest: &'a [u8],
}

impl<'a> ByteCursor<'a> {
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

    fn extensions(&mut self, rules_version: u32) -> Result<Extensions, SigningBytesError> {
        let max = caps_for(rules_version).max_extension_bytes;
        let count = u16::from_be_bytes(self.take_array()?);
        let mut entries = Vec::new();
        let mut total = 0usize;
        for _ in 0..count {
            let ext_type = u16::from_be_bytes(self.take_array()?);
            let [flags] = self.take_array()?;
            if flags & !CRITICAL_FLAG != 0 {
                return Err(SigningBytesError::ExtensionReservedFlags(flags));
            }
            let len = u32::from_be_bytes(self.take_array()?) as usize;
            total = total.saturating_add(EXTENSION_ENTRY_HEADER.saturating_add(len));
            if total > max {
                return Err(SigningBytesError::ExtensionsTooLarge);
            }
            entries.push(Extension {
                ext_type,
                critical: flags & CRITICAL_FLAG != 0,
                value: self.take(len)?.to_vec(),
            });
        }
        Extensions::new(entries, rules_version)
    }
}

/// Reads fields in the order the layout defines. Never allocates from a
/// declared length before checking it fits in the remaining bytes.
#[derive(Debug)]
pub struct Reader<'a> {
    tag: DomainTag,
    cur: ByteCursor<'a>,
    layout_version: u16,
    rules_version: u32,
}

impl<'a> Reader<'a> {
    /// Checks the tag, then the layout and rules versions against this node's ranges. A version
    /// above them is [`SigningBytesError::NeedsNewerVersion`] before any field is read.
    pub fn new(tag: DomainTag, message: &'a [u8]) -> Result<Self, SigningBytesError> {
        let rest = message
            .strip_prefix(tag.as_bytes())
            .ok_or(SigningBytesError::TagMismatch)?;
        let mut reader = Self {
            tag,
            cur: ByteCursor { rest },
            layout_version: 0,
            rules_version: 0,
        };
        reader.layout_version = u16::from_be_bytes(reader.take_array()?);
        check_layout(tag, reader.layout_version)?;
        reader.rules_version = u32::from_be_bytes(reader.take_array()?);
        check_rules(reader.rules_version)?;
        Ok(reader)
    }

    pub fn layout_version(&self) -> u16 {
        self.layout_version
    }

    pub fn rules_version(&self) -> u32 {
        self.rules_version
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], SigningBytesError> {
        self.cur.take(len)
    }

    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], SigningBytesError> {
        self.cur.take_array()
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

    /// A public key; an unknown algorithm tag is the typed "needs a newer version" result.
    pub fn key(&mut self) -> Result<[u8; 32], SigningBytesError> {
        SigAlgo::from_id(self.u8()?)?;
        self.take_array()
    }

    /// A signature; an unknown algorithm tag is the typed "needs a newer version" result.
    pub fn signature(&mut self) -> Result<[u8; 64], SigningBytesError> {
        SigAlgo::from_id(self.u8()?)?;
        self.take_array()
    }

    /// A 32-byte hash, raw.
    pub fn hash(&mut self) -> Result<[u8; 32], SigningBytesError> {
        self.take_array()
    }

    /// The `hash_algo` byte; an unknown value is the typed "needs a newer version" result.
    pub fn hash_algo(&mut self) -> Result<HashAlgo, SigningBytesError> {
        HashAlgo::from_id(self.u8()?)
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

    /// Reads the extensions region, which must be all that remains. Unknown critical extensions
    /// are the typed "needs a newer version" result; unknown non-critical ones are returned
    /// for the caller to keep.
    pub fn finish(mut self) -> Result<Extensions, SigningBytesError> {
        let extensions = self.cur.extensions(self.rules_version)?;
        if !self.cur.rest.is_empty() {
            return Err(SigningBytesError::TrailingBytes);
        }
        extensions.require_understood(self.tag)?;
        Ok(extensions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tags of the layouts that keep their own `avalon-...` / `avalon:...` form.
    const EXISTING_TAGS: &[&str] = &[
        "avalon-name-binding-v1",
        "avalon-node-request-v1",
        "avalon-identity-id-v1",
        "avalon-name-proof-v1",
        "avalon-shard-route-v1",
        "avalon-cross-shard-leaf-v1",
        "avalon-shard-family-leaf-v1",
        "avalon-shard-family-empty-v1",
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
        let msg = Builder::new(tags::CONFORMANCE, 2)
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
        assert_eq!(r.layout_version(), 2);
        assert_eq!(r.rules_version(), RULES_VERSION);
        assert_eq!(r.str().unwrap(), "a:b,c\0d");
        assert_eq!(r.bytes().unwrap(), b"");
        assert_eq!(r.key().unwrap(), [9; 32]);
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

        let mut r = Reader::new(tags::CONFORMANCE, &ok[..ok.len() - 3]).unwrap();
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
        huge.extend_from_slice(&[0, 1, 0, 0, 0, 1, 0xff, 0xff, 0xff, 0xff, b'x']);
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

    fn conf(layout: u16) -> Builder {
        Builder::new(tags::CONFORMANCE, layout)
    }

    #[test]
    fn header_is_tag_layout_rules_then_empty_extensions() {
        let msg = conf(1).finish().unwrap();
        let mut expect = tags::CONFORMANCE.as_bytes().to_vec();
        expect.extend_from_slice(&[0, 1, 0, 0, 0, 1, 0, 0]);
        assert_eq!(msg, expect);
    }

    #[test]
    fn hash_algo_round_trips_and_unknown_needs_newer() {
        let msg = conf(1).hash_algo(HashAlgo::Sha256).finish().unwrap();
        let mut r = Reader::new(tags::CONFORMANCE, &msg).unwrap();
        assert_eq!(r.hash_algo().unwrap(), HashAlgo::Sha256);
        for bad in [0u8, 2, 0xff] {
            let msg = conf(1).u8(bad).finish().unwrap();
            let err = Reader::new(tags::CONFORMANCE, &msg)
                .unwrap()
                .hash_algo()
                .unwrap_err();
            assert_eq!(
                err.needs_newer_version(),
                Some((VersionKind::HashAlgo, u32::from(bad)))
            );
        }
    }

    #[test]
    fn non_critical_extensions_round_trip_byte_for_byte() {
        let msg = conf(1)
            .str("x")
            .extension(5, false, b"abc")
            .extension(9, false, &[])
            .extension(0x7001, false, &[1, 2])
            .finish()
            .unwrap();
        let mut r = Reader::new(tags::CONFORMANCE, &msg).unwrap();
        assert_eq!(r.str().unwrap(), "x");
        let ext = r.finish().unwrap();
        assert_eq!(ext.entries().len(), 3);
        assert_eq!(ext.entries()[0].value, b"abc");
        let env = Envelope {
            layout_version: 1,
            rules_version: RULES_VERSION,
            hash_algo: HashAlgo::Sha256,
            extensions: ext.clone(),
        };
        let rebuilt = Builder::with_envelope(tags::CONFORMANCE, &env)
            .str("x")
            .finish()
            .unwrap();
        assert_eq!(rebuilt, msg);
        assert_eq!(
            Extensions::decode(&ext.encode(), RULES_VERSION).unwrap(),
            ext
        );
    }

    #[test]
    fn unknown_critical_extension_needs_newer_version() {
        let msg = {
            let mut out = conf(1).finish().unwrap();
            out.truncate(out.len() - 2);
            out.extend_from_slice(&[0, 1, 0x12, 0x34, 1, 0, 0, 0, 0]);
            out
        };
        let err = Reader::new(tags::CONFORMANCE, &msg)
            .unwrap()
            .finish()
            .unwrap_err();
        assert_eq!(
            err.needs_newer_version(),
            Some((VersionKind::CriticalExtension, 0x1234))
        );
        let err = conf(1).extension(7, true, b"").finish().unwrap_err();
        assert_eq!(
            err.needs_newer_version(),
            Some((VersionKind::CriticalExtension, 7))
        );
    }

    #[test]
    fn layout_and_rules_above_range_need_newer_before_any_field() {
        let mut msg = tags::CONFORMANCE.as_bytes().to_vec();
        msg.extend_from_slice(&[0, 3, 0, 0, 0, 1, 0xde, 0xad]);
        assert_eq!(
            Reader::new(tags::CONFORMANCE, &msg)
                .unwrap_err()
                .needs_newer_version(),
            Some((VersionKind::Layout, 3))
        );
        let mut msg = tags::CONFORMANCE.as_bytes().to_vec();
        msg.extend_from_slice(&[0, 1, 0, 0, 0, 2]);
        assert_eq!(
            Reader::new(tags::CONFORMANCE, &msg)
                .unwrap_err()
                .needs_newer_version(),
            Some((VersionKind::Rules, 2))
        );
        assert!(Builder::new(tags::CONFORMANCE, 3).finish().is_err());
        assert!(Builder::with_rules(tags::CONFORMANCE, 1, 2)
            .finish()
            .is_err());
    }

    #[test]
    fn below_range_versions_are_unsupported_not_newer() {
        let err = Builder::new(tags::CONFORMANCE, 0).finish().unwrap_err();
        assert_eq!(err.needs_newer_version(), None);
        assert!(matches!(err, SigningBytesError::UnsupportedVersion { .. }));
    }

    #[test]
    fn older_layout_version_of_a_known_tag_still_reads() {
        for v in [1u16, 2] {
            let msg = conf(v).str("a").finish().unwrap();
            let mut r = Reader::new(tags::CONFORMANCE, &msg).unwrap();
            assert_eq!(r.layout_version(), v);
            assert_eq!(r.str().unwrap(), "a");
            r.finish().unwrap();
        }
    }

    #[test]
    fn malformed_extension_regions_are_rejected() {
        let region = |entries: &[(u16, u8, &[u8])]| {
            let mut out = tags::CONFORMANCE.as_bytes().to_vec();
            out.extend_from_slice(&[0, 1, 0, 0, 0, 1]);
            out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
            for (t, f, v) in entries {
                out.extend_from_slice(&t.to_be_bytes());
                out.push(*f);
                out.extend_from_slice(&(v.len() as u32).to_be_bytes());
                out.extend_from_slice(v);
            }
            out
        };
        let finish = |m: Vec<u8>| Reader::new(tags::CONFORMANCE, &m).unwrap().finish();
        assert_eq!(
            finish(region(&[(9, 0, b""), (5, 0, b"")])).unwrap_err(),
            SigningBytesError::ExtensionsUnsorted
        );
        assert_eq!(
            finish(region(&[(5, 0, b""), (5, 0, b"")])).unwrap_err(),
            SigningBytesError::ExtensionDuplicate(5)
        );
        for flags in [0x02u8, 0x80, 0xff] {
            assert_eq!(
                finish(region(&[(5, flags, b"")])).unwrap_err(),
                SigningBytesError::ExtensionReservedFlags(flags)
            );
        }
        let big = vec![0u8; caps_for(RULES_VERSION).max_extension_bytes];
        assert_eq!(
            finish(region(&[(5, 0, &big)])).unwrap_err(),
            SigningBytesError::ExtensionsTooLarge
        );
        let fits = vec![0u8; caps_for(RULES_VERSION).max_extension_bytes - EXTENSION_ENTRY_HEADER];
        assert!(finish(region(&[(5, 0, &fits)])).is_ok());
        let mut trailing = region(&[]);
        trailing.push(0);
        assert_eq!(
            finish(trailing).unwrap_err(),
            SigningBytesError::TrailingBytes
        );
        assert_eq!(
            conf(1)
                .extension(9, false, b"")
                .extension(5, false, b"")
                .finish()
                .unwrap_err(),
            SigningBytesError::ExtensionsUnsorted
        );
    }

    #[test]
    fn keys_and_signatures_carry_an_algorithm_tag() {
        let msg = conf(1).key(&[7; 32]).signature(&[8; 64]).finish().unwrap();
        let header = tags::CONFORMANCE.as_bytes().len() + 6;
        assert_eq!(msg[header], 1);
        assert_eq!(msg[header + 33], 1);
        let mut r = Reader::new(tags::CONFORMANCE, &msg).unwrap();
        assert_eq!(r.key().unwrap(), [7; 32]);
        assert_eq!(r.signature().unwrap(), [8; 64]);
        let mut bad = msg.clone();
        bad[header] = 2;
        let err = Reader::new(tags::CONFORMANCE, &bad)
            .unwrap()
            .key()
            .unwrap_err();
        assert_eq!(err.needs_newer_version(), Some((VersionKind::SigAlgo, 2)));
        let mut bad = msg;
        bad[header + 33] = 0;
        let mut r = Reader::new(tags::CONFORMANCE, &bad).unwrap();
        r.key().unwrap();
        assert_eq!(
            r.signature().unwrap_err().needs_newer_version(),
            Some((VersionKind::SigAlgo, 0))
        );
    }

    #[test]
    fn caps_are_a_function_of_rules_version_and_never_shrink() {
        let at_cap = vec![Extension {
            ext_type: 1,
            critical: false,
            value: vec![0; caps_for(1).max_extension_bytes - EXTENSION_ENTRY_HEADER],
        }];
        assert!(Extensions::new(at_cap.clone(), 1).is_ok());
        let mut over = at_cap.clone();
        over[0].value.push(0);
        assert_eq!(
            Extensions::new(over.clone(), 1).unwrap_err(),
            SigningBytesError::ExtensionsTooLarge
        );
        // A synthetic newer rules version raises the cap; the older entry keeps verifying.
        let table = [
            (
                1,
                Caps {
                    max_extension_bytes: 4096,
                },
            ),
            (
                2,
                Caps {
                    max_extension_bytes: 8192,
                },
            ),
        ];
        assert_eq!(caps_in(&table, 1).max_extension_bytes, 4096);
        assert_eq!(caps_in(&table, 2).max_extension_bytes, 8192);
        assert_eq!(caps_in(&table, 3).max_extension_bytes, 8192);
        assert!(CAPS_TABLE.windows(2).all(|w| {
            w[0].0 < w[1].0 && w[0].1.max_extension_bytes <= w[1].1.max_extension_bytes
        }));
    }

    #[test]
    fn every_registered_tag_has_a_layout_row() {
        for tag in tags::ALL {
            assert!(*layout_versions(*tag).start() >= 1, "{}", tag.as_str());
            assert!(Envelope::current(*tag).layout_version >= 1);
        }
    }

    #[test]
    fn envelope_from_parts_checks_every_part() {
        let ok = Envelope::from_parts(tags::CONFORMANCE, 1, 1, 1, &[0, 0]).unwrap();
        assert_eq!(ok.layout_version, 1);
        let needs = |r: Result<Envelope, SigningBytesError>| r.unwrap_err().needs_newer_version();
        assert_eq!(
            needs(Envelope::from_parts(tags::CONFORMANCE, 9, 1, 1, &[0, 0])),
            Some((VersionKind::Layout, 9))
        );
        assert_eq!(
            needs(Envelope::from_parts(tags::CONFORMANCE, 1, 9, 1, &[0, 0])),
            Some((VersionKind::Rules, 9))
        );
        assert_eq!(
            needs(Envelope::from_parts(tags::CONFORMANCE, 1, 1, 7, &[0, 0])),
            Some((VersionKind::HashAlgo, 7))
        );
        assert_eq!(
            needs(Envelope::from_parts(
                tags::CONFORMANCE,
                1,
                1,
                1,
                &[0, 1, 0, 4, 1, 0, 0, 0, 0]
            )),
            Some((VersionKind::CriticalExtension, 4))
        );
    }
}
