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
//! STH-only Ed25519 signing scheme #39 settled on. Mirror-facing proof/sync
//! endpoints that consume this are issue #211, not yet built.
//!
//! Nothing outside this crate should depend on *how* commitments are
//! produced — only on this trait — so that evolving the implementation
//! toward that design doesn't ripple into `protocol`, `server`, `sdk`, or any
//! game integration.

pub mod merkle;
mod postgres;
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

    async fn verify(&self, commitment: &Commitment) -> Result<bool, SettlementError>;

    async fn get_commitment(&self, batch_id: uuid::Uuid) -> Result<Commitment, SettlementError>;
}
