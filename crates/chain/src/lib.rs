//! The `SettlementProvider` boundary and its milestone-1 implementation.
//!
//! Per `docs/adr/0002-attestations-before-blockchain.md`: milestone 1 is a
//! signed, append-only ledger, not a blockchain. Nothing outside this crate
//! should depend on *how* commitments are produced — only on this trait —
//! so that swapping the implementation later (an appchain, an existing
//! high-throughput chain, or staying Postgres-backed indefinitely) doesn't
//! ripple into `protocol`, `server`, `sdk`, or any game integration.

mod postgres;

pub use postgres::PostgresSettlementProvider;

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
/// verify a commitment later. Deliberately minimal — see ADR 0002 for why a
/// full blockchain interface (consensus, block production, P2P) is not part
/// of this trait.
#[async_trait]
pub trait SettlementProvider: Send + Sync {
    async fn commit(&self, batch: &EventBatch) -> Result<Commitment, SettlementError>;

    async fn verify(&self, commitment: &Commitment) -> Result<bool, SettlementError>;

    async fn get_commitment(&self, batch_id: uuid::Uuid) -> Result<Commitment, SettlementError>;
}
