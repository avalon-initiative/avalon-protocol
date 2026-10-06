//! Storage form of the signing envelope: the four columns every stored layout records so a
//! verifier never guesses which layout produced a hash.

use avalon_protocol::signing_bytes::{DomainTag, Envelope, EnvelopeWire, SigningBytesError};
use sqlx::postgres::PgRow;
use sqlx::Row;

use crate::SettlementError;

/// Column list to splice into a `SELECT` for a table that stores an envelope.
pub const ENVELOPE_COLUMNS: &str = "layout_version, rules_version, hash_algo, extensions";

/// `layout_version`, `rules_version`, `hash_algo` and `extensions` as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeRow {
    pub layout_version: i16,
    pub rules_version: i32,
    pub hash_algo: i16,
    pub extensions: Vec<u8>,
}

fn out_of_range(_: std::num::TryFromIntError) -> SettlementError {
    SettlementError::InvalidEntry("envelope value does not fit its column".to_string())
}

impl EnvelopeRow {
    pub fn from_envelope(envelope: &Envelope) -> Result<Self, SettlementError> {
        Self::from_wire(&EnvelopeWire::from(envelope))
    }

    pub fn from_wire(wire: &EnvelopeWire) -> Result<Self, SettlementError> {
        Ok(Self {
            layout_version: i16::try_from(wire.layout_version).map_err(out_of_range)?,
            rules_version: i32::try_from(wire.rules_version).map_err(out_of_range)?,
            hash_algo: i16::from(wire.hash_algo),
            extensions: avalon_protocol::signing_bytes::decode_lower_hex(&wire.extensions)
                .map_err(SettlementError::from_layout)?,
        })
    }

    /// Reads the four columns, each name prefixed by `prefix` (empty for the plain names).
    pub fn read(row: &PgRow, prefix: &str) -> Result<Self, SettlementError> {
        let get = |e: sqlx::Error| SettlementError::Storage(e.to_string());
        Ok(Self {
            layout_version: row
                .try_get(format!("{prefix}layout_version").as_str())
                .map_err(get)?,
            rules_version: row
                .try_get(format!("{prefix}rules_version").as_str())
                .map_err(get)?,
            hash_algo: row
                .try_get(format!("{prefix}hash_algo").as_str())
                .map_err(get)?,
            extensions: row
                .try_get(format!("{prefix}extensions").as_str())
                .map_err(get)?,
        })
    }

    /// The plain-integer form, without checking any value against this node's ranges.
    pub fn to_wire(&self) -> EnvelopeWire {
        EnvelopeWire {
            layout_version: u16::try_from(self.layout_version).unwrap_or(0),
            rules_version: u32::try_from(self.rules_version).unwrap_or(0),
            hash_algo: u8::try_from(self.hash_algo).unwrap_or(0),
            extensions: hex::encode(&self.extensions),
        }
    }

    /// The checked envelope; a value above this node's ranges is the typed "needs a newer version".
    pub fn to_envelope(&self, tag: DomainTag) -> Result<Envelope, SettlementError> {
        self.to_wire()
            .to_envelope(tag)
            .map_err(SettlementError::from_layout)
    }
}

impl SettlementError {
    /// Maps a layout error: "needs a newer version" keeps its type, anything else is an invalid entry.
    pub fn from_layout(error: SigningBytesError) -> Self {
        match error.needs_newer_version() {
            Some((what, required)) => Self::NeedsNewerVersion { what, required },
            None => Self::InvalidEntry(error.to_string()),
        }
    }
}
