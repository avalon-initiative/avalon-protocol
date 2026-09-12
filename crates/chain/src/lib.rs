//! The `SettlementProvider` boundary and its milestone-1 implementation.
//!
//! Milestone 1 is a signed, append-only ledger, not a blockchain (decided:
//! GitHub issue #68). The durable settlement layer this grows into is a
//! public transparency log — independently verifiable and mirrorable by
//! anyone, without federation (servers whitelisting each other) or
//! mining/consensus (decided: issue #70). The log's technical design (hash
//! structure, signed tree heads, mirror sync) is decided too — issues #40
//! and #39, following RFC 6962 (Certificate Transparency) directly — and
//! real as of issue #210: [`merkle`] is the RFC 6962 Merkle Tree Hash over
//! the ledger's `entry_hash` values, layered on top of (not replacing)
//! `postgres`'s existing sequential hash chain, and [`sth`] is the
//! STH-only Ed25519 signing scheme #39 settled on. [`merkle`] also carries
//! issue #211's RFC 6962 inclusion/consistency proof generation and
//! verification (`inclusion_proof`/`consistency_proof` plus their
//! `verify_*` counterparts) — the mirror-facing proof/sync endpoints that
//! consume them live in `avalon-server`'s `settlement` module.
//!
//! [`mirror`] is issue #299's mirror-watcher storage and equivocation
//! detection: a mirror observes STHs from peers over HTTP
//! (`avalon-server`'s `mirror_watcher` module actually does the fetching),
//! but the storage/comparison logic — "is this a genuinely new
//! observation, and does it disagree with anything else this node has
//! seen at the same tree_size" — lives here so it's testable without a
//! network.
//!
//! [`retention`] is issue #208's node-tiered durable history retention,
//! implementing #180's decision: separate from all of the above, it tiers
//! how long a node keeps raw event *bodies* (`ledger_entries.payload`)
//! locally, without ever touching the commitment structures above — see
//! its module doc comment for the full design and its milestone-1
//! availability caveat.
//!
//! Nothing outside this crate should depend on *how* commitments are
//! produced — only on this trait — so that evolving the implementation
//! toward that design doesn't ripple into `protocol`, `server`, `sdk`, or any
//! game integration.

pub mod attestations;
pub mod incremental_merkle;
pub mod merkle;
pub mod mirror;
mod postgres;
pub mod retention;
pub mod sth;

pub use postgres::{
    IssuerHistoryEntry, LedgerBatchView, LedgerEntryView, PostgresSettlementProvider,
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
