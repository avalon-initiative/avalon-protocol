//! The `SettlementProvider` boundary and its milestone-1 implementation.
//!
//! Milestone 1 is a signed, append-only ledger, not a blockchain (decided:
//! GitHub issue #68). The durable settlement layer this grows into is a
//! public transparency log — independently verifiable and mirrorable by
//! anyone, without federation (servers whitelisting each other) or
//! mining/consensus (decided: issue #70). The exact technical design of that
//! log (hash structure, signed tree heads, mirror sync) is still open —
//! tracked in issue #40.
//!
//! Nothing outside this crate should depend on *how* commitments are
//! produced — only on this trait — so that evolving the implementation
//! toward that design doesn't ripple into `protocol`, `server`, `sdk`, or any
//! game integration.

mod hashing;
mod postgres;
#[cfg(feature = "rocksdb-backend")]
mod rocksdb_backend;

#[cfg(feature = "rocksdb-backend")]
pub use rocksdb_backend::RocksDbSettlementProvider;

pub use postgres::{IssuerHistoryEntry, PostgresSettlementProvider};

use async_trait::async_trait;
use avalon_protocol::events::{Commitment, EventBatch};

/// One ledger entry plus whether it's actually intact — both that its own
/// content still matches its claimed hash, and that it correctly links to
/// the entry before it. `payload` is carried through mainly for
/// `avalon inspect-ledger-full`; the concise `avalon inspect-ledger` view
/// doesn't print it. Backend-agnostic (issue #178) — every
/// `SettlementProvider` implementation's own inspection method returns this
/// same shape.
pub struct LedgerEntryView {
    pub seq: i64,
    pub event_id: uuid::Uuid,
    pub kind: String,
    pub issuer: String,
    pub subject: String,
    pub payload: serde_json::Value,
    pub version: i32,
    pub event_timestamp: time::OffsetDateTime,
    pub prev_hash: String,
    pub entry_hash: String,
    pub batch_id: uuid::Uuid,
    pub chain_intact: bool,
}

/// One committed batch — the unit of settlement (issue #38): entries are
/// hash-chained individually, but a batch is what `get_commitment` looks up
/// and what `avalon inspect-ledger` prints boundaries for. `batch_root` is a
/// placeholder deterministic root (the batch's chain tip — its last entry's
/// `entry_hash`); a Merkle root over the batch is #40's call. Backend-
/// agnostic (issue #178), same reasoning as `LedgerEntryView`.
pub struct LedgerBatchView {
    pub batch_id: uuid::Uuid,
    pub first_seq: i64,
    pub last_seq: i64,
    pub batch_root: String,
    pub committed_at: time::OffsetDateTime,
}

#[derive(Debug, thiserror::Error)]
pub enum SettlementError {
    #[error("batch not found")]
    BatchNotFound,
    #[error("commitment verification failed")]
    VerificationFailed,
    #[error("storage error: {0}")]
    Storage(String),
}

/// Anything capable of durably committing event batches and letting a caller
/// verify a commitment later. Deliberately minimal — no consensus, block
/// production, or P2P networking belongs on this trait (issues #68, #70):
/// the target shape is a transparency log, which needs verifiability and
/// mirrorability, not a validator network.
#[async_trait]
pub trait SettlementProvider: Send + Sync {
    async fn commit(&self, batch: &EventBatch) -> Result<Commitment, SettlementError>;

    async fn verify(&self, commitment: &Commitment) -> Result<bool, SettlementError>;

    async fn get_commitment(&self, batch_id: uuid::Uuid) -> Result<Commitment, SettlementError>;
}
