//! The ledger entry hash (#1226): one structured layout every authoring node and mirror recomputes.
//!
//! Layout (tag `avalon.ledger.entry`, `u16` version = the entry's own `version`): network id `str`,
//! shard id `str`, `seq` `u64`, previous entry hash (32 raw), event id (16 raw), kind, issuer and
//! subject `str`, payload hash (32 raw), event time as `i64` unix microseconds. The entry commits to
//! the payload hash, not the payload, so a row with a pruned payload still verifies. A different
//! field set is a new tag, since the version slot carries the event version.
//!
//! Exception to the signing-bytes rule "a new field means a new version": the version slot here is
//! the event's own `version`, so it cannot version this layout. A changed field set gets a new tag.

use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::canonical_payload::{canonicalize, CanonicalPayloadError};
use crate::signing_bytes::{tags, Builder, SigningBytesError};

/// Why an entry hash could not be computed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EntryHashError {
    #[error("{0} is not a 32-byte lowercase hex hash")]
    InvalidHash(&'static str),
    #[error("{0} is out of range for the entry layout")]
    OutOfRange(&'static str),
    #[error(transparent)]
    Layout(#[from] SigningBytesError),
}

/// Every field the entry hash covers.
pub struct EntryHashInput<'a> {
    pub network_id: &'a str,
    pub shard_id: &'a str,
    pub seq: u64,
    pub prev_hash: &'a [u8; 32],
    pub event_id: Uuid,
    pub kind: &'a str,
    pub issuer: &'a str,
    pub subject: &'a str,
    pub payload_hash: &'a [u8; 32],
    pub timestamp_micros: i64,
    pub version: u16,
}

/// SHA-256 of the canonical payload bytes (#1308): what an entry commits to in place of the payload.
pub fn payload_hash(payload: &Value) -> Result<[u8; 32], CanonicalPayloadError> {
    Ok(Sha256::digest(canonicalize(payload)?.as_bytes()).into())
}

/// Unix microseconds, the precision the ledger stores, truncated toward negative infinity.
pub fn timestamp_micros(time: OffsetDateTime) -> Result<i64, EntryHashError> {
    i64::try_from(time.unix_timestamp_nanos().div_euclid(1000))
        .map_err(|_| EntryHashError::OutOfRange("timestamp"))
}

/// The layout's `u16` version slot for an event version, rejecting anything above `u16::MAX`.
pub fn layout_version(version: u32) -> Result<u16, EntryHashError> {
    u16::try_from(version).map_err(|_| EntryHashError::OutOfRange("version"))
}

/// The instant floored to whole microseconds: the value that is both hashed and stored.
pub fn floor_to_micros(time: OffsetDateTime) -> Result<OffsetDateTime, EntryHashError> {
    let micros = i128::from(timestamp_micros(time)?);
    OffsetDateTime::from_unix_timestamp_nanos(micros * 1000)
        .map_err(|_| EntryHashError::OutOfRange("timestamp"))
}

/// Parses a 64-character lowercase hex hash into its raw bytes.
pub fn parse_hash(name: &'static str, text: &str) -> Result<[u8; 32], EntryHashError> {
    if text.len() != 64 || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(EntryHashError::InvalidHash(name));
    }
    let mut out = [0u8; 32];
    hex::decode_to_slice(text, &mut out).map_err(|_| EntryHashError::InvalidHash(name))?;
    Ok(out)
}

/// The exact bytes the entry hash is the SHA-256 of.
pub fn entry_signing_bytes(input: &EntryHashInput<'_>) -> Result<Vec<u8>, EntryHashError> {
    Ok(Builder::new(tags::LEDGER_ENTRY, input.version)
        .str(input.network_id)
        .str(input.shard_id)
        .u64(input.seq)
        .hash(input.prev_hash)
        .uuid(input.event_id)
        .str(input.kind)
        .str(input.issuer)
        .str(input.subject)
        .hash(input.payload_hash)
        .i64(input.timestamp_micros)
        .finish()?)
}

/// The entry hash: SHA-256 of [`entry_signing_bytes`].
pub fn entry_hash(input: &EntryHashInput<'_>) -> Result<[u8; 32], EntryHashError> {
    Ok(Sha256::digest(entry_signing_bytes(input)?).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(prev: &'a [u8; 32], payload: &'a [u8; 32]) -> EntryHashInput<'a> {
        EntryHashInput {
            network_id: "net",
            shard_id: "core",
            seq: 1,
            prev_hash: prev,
            event_id: Uuid::from_u128(1),
            kind: "k",
            issuer: "i",
            subject: "s",
            payload_hash: payload,
            timestamp_micros: 1,
            version: 1,
        }
    }

    #[test]
    fn every_field_changes_the_hash() {
        let (p, h) = ([0u8; 32], [1u8; 32]);
        let base = entry_hash(&input(&p, &h)).unwrap();
        let other = [9u8; 32];
        let variants: Vec<EntryHashInput<'_>> = vec![
            EntryHashInput {
                network_id: "net2",
                ..input(&p, &h)
            },
            EntryHashInput {
                shard_id: "core2",
                ..input(&p, &h)
            },
            EntryHashInput {
                seq: 2,
                ..input(&p, &h)
            },
            EntryHashInput {
                prev_hash: &other,
                ..input(&p, &h)
            },
            EntryHashInput {
                event_id: Uuid::from_u128(2),
                ..input(&p, &h)
            },
            EntryHashInput {
                kind: "k2",
                ..input(&p, &h)
            },
            EntryHashInput {
                issuer: "i2",
                ..input(&p, &h)
            },
            EntryHashInput {
                subject: "s2",
                ..input(&p, &h)
            },
            EntryHashInput {
                payload_hash: &other,
                ..input(&p, &h)
            },
            EntryHashInput {
                timestamp_micros: 2,
                ..input(&p, &h)
            },
            EntryHashInput {
                version: 2,
                ..input(&p, &h)
            },
        ];
        for v in variants {
            assert_ne!(entry_hash(&v).unwrap(), base);
        }
    }

    #[test]
    fn adjacent_strings_cannot_shift_boundaries() {
        let (p, h) = ([0u8; 32], [1u8; 32]);
        let a = EntryHashInput {
            kind: "ab",
            issuer: "c",
            ..input(&p, &h)
        };
        let b = EntryHashInput {
            kind: "a",
            issuer: "bc",
            ..input(&p, &h)
        };
        assert_ne!(entry_hash(&a).unwrap(), entry_hash(&b).unwrap());
    }

    #[test]
    fn sub_second_time_is_covered() {
        let t = |nanos: i128| {
            timestamp_micros(OffsetDateTime::from_unix_timestamp_nanos(nanos).unwrap()).unwrap()
        };
        assert_ne!(t(1_000_000_000), t(1_000_001_000));
        assert_eq!(t(1_000_000_999), t(1_000_000_000));
        assert_eq!(t(-1), -1);
    }

    #[test]
    fn floor_to_micros_floors_before_and_after_the_pg_epoch() {
        for nanos in [
            -1i128,
            -1_500,
            1_500,
            -946_684_800_000_000_500,
            946_684_799_999_999_999,
        ] {
            let t = OffsetDateTime::from_unix_timestamp_nanos(nanos).unwrap();
            let f = floor_to_micros(t).unwrap();
            assert_eq!(f.unix_timestamp_nanos() % 1000, 0);
            assert_eq!(timestamp_micros(f).unwrap(), timestamp_micros(t).unwrap());
            assert!(f <= t && t - f < time::Duration::microseconds(1));
        }
    }

    #[test]
    fn layout_version_rejects_above_u16() {
        assert_eq!(layout_version(65535).unwrap(), 65535);
        assert!(layout_version(65536).is_err());
    }

    #[test]
    fn parse_hash_is_strict() {
        assert!(parse_hash("h", &"0".repeat(64)).is_ok());
        for bad in [
            "",
            &"0".repeat(63),
            &"A".repeat(64),
            &"g".repeat(64),
            &" 0".repeat(32),
        ] {
            assert!(parse_hash("h", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn layout_reads_back() {
        let (p, h) = ([3u8; 32], [4u8; 32]);
        let bytes = entry_signing_bytes(&input(&p, &h)).unwrap();
        let mut r = crate::signing_bytes::Reader::new(tags::LEDGER_ENTRY, &bytes).unwrap();
        assert_eq!(r.version(), 1);
        assert_eq!(r.str().unwrap(), "net");
        assert_eq!(r.str().unwrap(), "core");
        assert_eq!(r.u64().unwrap(), 1);
        assert_eq!(r.fixed::<32>().unwrap(), p);
        assert_eq!(r.uuid().unwrap(), Uuid::from_u128(1));
        assert_eq!(
            (r.str().unwrap(), r.str().unwrap(), r.str().unwrap()),
            ("k", "i", "s")
        );
        assert_eq!(r.fixed::<32>().unwrap(), h);
        assert_eq!(r.i64().unwrap(), 1);
        r.finish().unwrap();
    }
}
