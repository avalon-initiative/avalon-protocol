//! The `SettlementProvider` boundary and its milestone-1 implementation:
//! a signed, append-only ledger, not a blockchain (#68/#70/#40/#39). See
//! `docs/architecture/settlement.md` for the decided design. [`merkle`] is
//! the RFC 6962 Merkle tree/proof machinery (#210/#211), [`sth`] the
//! STH-signing scheme (#39, moved into `avalon_protocol::sth` by #773 so
//! the SDK can depend on it without pulling in chain's Postgres stack —
//! re-exported here so existing callers of `avalon_chain::sth` keep
//! working), [`mirror`] the mirror-watcher storage (#299), and
//! [`retention`] node-tiered payload retention (#208/#180) — see each
//! module's own doc comment. Nothing outside this crate should depend on
//! *how* commitments are produced, only on this trait.

pub mod attestations;
pub mod cross_shard;
pub mod incremental_merkle;
pub mod merkle;
pub mod migration;
pub mod mirror;
mod postgres;
pub mod retention;
pub use avalon_protocol::sth;

pub use postgres::{
    hash_entry, EntryContent, GenesisError, IssuerHistoryEntry, LedgerBatchView, LedgerEntryView,
    PostgresSettlementProvider,
};

use async_trait::async_trait;
use avalon_protocol::events::{Commitment, EventBatch};

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

    /// `Ok(true)` means every check this implementation runs passed — it
    /// does **not** promise every entry's content was independently
    /// re-verified. A retention-tiered implementation (issue #208) may
    /// have locally pruned some entries' payloads; content for those
    /// specific entries can't be independently re-checked (an expected
    /// local-retention outcome, not evidence of tampering), while the
    /// link/structure checks and the ledger-wide commitment check still
    /// run and still gate the result. A caller that needs to know *which*
    /// entries were and weren't content-checkable should use
    /// `PostgresSettlementProvider::list_entries`'s per-entry
    /// `payload_pruned`/`chain_intact` fields instead of this coarser
    /// bool.
    async fn verify(&self, commitment: &Commitment) -> Result<bool, SettlementError>;

    async fn get_commitment(&self, batch_id: uuid::Uuid) -> Result<Commitment, SettlementError>;
}
