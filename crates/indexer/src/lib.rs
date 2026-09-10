//! The query/index layer.
//!
//! Fast reads (profiles, friend lists, guild rosters, achievement lists)
//! should never require walking settlement data directly. This crate
//! consumes durable `ProtocolEvent`s and maintains a read model that can be
//! rebuilt from those events at any time — see
//! `docs/architecture/query-and-indexing.md`.
//!
//! Milestone 1: a conventional Postgres-backed read model, kept explicitly
//! separate from `avalon-chain`'s settlement store so the two are never
//! conflated — settlement is not querying; see
//! `docs/architecture/settlement.md` and issue #75.
//!
//! [`postgres::PostgresIndexer`] is the first real [`Indexer`] (issue #42):
//! `projections` holds one module per read model (profiles, friendships,
//! guild rosters, attestations), each exposing a pure `decode` (event →
//! typed write, no I/O, unit-testable without Postgres) and an `apply`
//! (typed write → SQL, executed against a caller-supplied transaction).

use async_trait::async_trait;
use avalon_protocol::events::ProtocolEvent;

pub mod postgres;
pub mod projections;
pub mod registry;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("index storage error: {0}")]
    Storage(String),
}

impl From<sqlx::Error> for IndexError {
    fn from(err: sqlx::Error) -> Self {
        IndexError::Storage(err.to_string())
    }
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
