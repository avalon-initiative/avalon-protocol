//! The query/index layer.
//!
//! Fast reads (profiles, friend lists, guild rosters, achievement lists)
//! should never require walking settlement data directly. This crate
//! consumes durable `ProtocolEvent`s and maintains a read model that can be
//! rebuilt from those events at any time — see
//! `avalon-docs/architecture/query-and-indexing.md`.
//!
//! Milestone 1: a conventional Postgres-backed read model, kept explicitly
//! separate from `avalon-chain`'s settlement store so the two are never
//! conflated — settlement is not querying; see
//! `avalon-docs/architecture/settlement.md` and issue #75.
//!
//! [`postgres::PostgresIndexer`] is the first real [`Indexer`]:
//! `projections` holds one module per read model (profiles, friendships,
//! guild rosters, attestations), each exposing a pure `decode` (event →
//! typed write, no I/O, unit-testable without Postgres) and an `apply`
//! (typed write → SQL, executed against a caller-supplied transaction).

use async_trait::async_trait;
use avalon_protocol::events::ProtocolEvent;

pub mod identity_chain_store;
pub mod postgres;
pub mod projections;
pub mod registry;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("index storage error: {0}")]
    Storage(String),
    /// Issue #510: `profiles_display_name_lower_idx`'s real unique
    /// constraint was violated — the actual, atomic enforcement point for
    /// display-name uniqueness (case-insensitive), not a separate prior
    /// check a concurrent writer could race past. Distinguished from
    /// [`Self::Storage`] specifically so callers (`crate::server::handlers`)
    /// can map it to a clean "name taken" response instead of a generic
    /// 500.
    #[error("display_name is already taken")]
    DisplayNameTaken,
    /// Issue #661: raised by a network-facing [`Indexer`] implementation
    /// (`avalon_server::internal_role::RemoteIndexer`) when a request to
    /// the remote Indexer process could not be completed — a connection
    /// failure, timeout, or a non-success response, as opposed to
    /// [`Self::Storage`], which means the remote (or local) Indexer *did*
    /// run and reported a real storage failure of its own. Kept in this
    /// crate (rather than only in `avalon-server`, which owns the actual
    /// HTTP client) so any future `Indexer` implementation — remote or
    /// otherwise — has one shared way to report "the role I depend on
    /// didn't answer," instead of every caller having to guess from a
    /// generic [`Self::Storage`] string whether the failure was local or a
    /// downstream dependency being down.
    #[error("remote indexer role unreachable: {0}")]
    RemoteUnreachable(String),
    /// The database could not serve the write (connection, pool, resource or
    /// serialization failure); retrying the same event may succeed.
    #[error("index storage unavailable: {0}")]
    Unavailable(String),
}

impl IndexError {
    /// Whether retrying the same event later can succeed.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Unavailable(_) | Self::RemoteUnreachable(_))
    }
}

/// SQLSTATEs worth retrying: connection (08), serialization/deadlock (40),
/// resources (53), operator shutdown (57), system I/O (58), lock timeout and
/// read-only transaction (failover).
fn is_transient_sqlstate(code: &str) -> bool {
    ["08", "40", "53", "57", "58"]
        .iter()
        .any(|p| code.starts_with(p))
        || matches!(code, "55P03" | "25006")
}

fn is_unavailable(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => true,
        sqlx::Error::Database(db) => db.code().is_some_and(|c| is_transient_sqlstate(&c)),
        _ => false,
    }
}

impl From<sqlx::Error> for IndexError {
    fn from(err: sqlx::Error) -> Self {
        let is_display_name_conflict = err
            .as_database_error()
            .is_some_and(|db_err| db_err.constraint() == Some("profiles_display_name_lower_idx"));
        if is_display_name_conflict {
            return IndexError::DisplayNameTaken;
        }
        if is_unavailable(&err) {
            return IndexError::Unavailable(err.to_string());
        }
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

#[cfg(test)]
mod classification_tests {
    use super::*;

    #[test]
    fn connection_class_errors_are_transient_and_others_are_not() {
        assert!(IndexError::from(sqlx::Error::PoolTimedOut).is_transient());
        assert!(IndexError::from(sqlx::Error::PoolClosed).is_transient());
        assert!(!IndexError::from(sqlx::Error::RowNotFound).is_transient());
        assert!(!IndexError::DisplayNameTaken.is_transient());
    }

    #[test]
    fn every_transient_sqlstate_branch_is_covered() {
        for code in [
            "08006", "40001", "40P01", "53300", "57P01", "58030", "55P03", "25006",
        ] {
            assert!(is_transient_sqlstate(code), "{code}");
        }
        for code in ["23503", "23505", "42P01", "XX000", "22P02", "55000"] {
            assert!(!is_transient_sqlstate(code), "{code}");
        }
    }
}
