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
