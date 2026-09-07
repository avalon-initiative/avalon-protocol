//! The query/index layer.
//!
//! Fast reads (profiles, friend lists, guild rosters, achievement lists)
//! should never require walking settlement data directly. This crate
//! consumes durable `ProtocolEvent`s and maintains a read model that can be
//! rebuilt from those events at any time — see `PROMPT.md` §21.
//!
//! Milestone 1: a conventional Postgres-backed read model, kept explicitly
//! separate from `avalon-chain`'s settlement store so the two are never
//! conflated (`Proposal.md` / `PROMPT.md` §6: "Settlement should not be
//! confused with querying").

use async_trait::async_trait;
use avalon_protocol::events::ProtocolEvent;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("index storage error: {0}")]
    Storage(String),
}

/// Applies durable protocol events to a rebuildable read model.
///
/// `apply` must be idempotent — replaying the same event twice (during a
/// rebuild, or after a redelivery) must not corrupt the index.
#[async_trait]
pub trait Indexer: Send + Sync {
    async fn apply(&self, event: &ProtocolEvent) -> Result<(), IndexError>;

    /// Drop and rebuild this index from scratch by replaying every durable
    /// event again. Proves the index is genuinely derived state, not a
    /// second source of truth.
    async fn rebuild(&self, events: &[ProtocolEvent]) -> Result<(), IndexError> {
        for event in events {
            self.apply(event).await?;
        }
        Ok(())
    }
}
