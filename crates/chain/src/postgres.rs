//! Postgres-backed `SettlementProvider` — milestone 1's only implementation.
//!
//! Still stubbed: `crates/server` already settled on `sqlx` as the Postgres
//! driver, so this crate should follow suit rather than reopening that
//! choice. What's still open (issue #40) is the entry format itself — this
//! needs to store hash-chained, signed entries from the start (per issue
//! #70's transparency-log decision), not a plain row per commitment, so a
//! future mirror can independently verify the log rather than trusting this
//! database.

use async_trait::async_trait;
use avalon_protocol::events::{Commitment, EventBatch};

use crate::{SettlementError, SettlementProvider};

pub struct PostgresSettlementProvider {
    #[allow(dead_code)]
    connection_string: String,
}

impl PostgresSettlementProvider {
    pub fn new(connection_string: impl Into<String>) -> Self {
        Self {
            connection_string: connection_string.into(),
        }
    }
}

#[async_trait]
impl SettlementProvider for PostgresSettlementProvider {
    async fn commit(&self, _batch: &EventBatch) -> Result<Commitment, SettlementError> {
        // TODO: sign the batch, insert it, return the resulting commitment.
        Err(SettlementError::Storage("not yet implemented".into()))
    }

    async fn verify(&self, _commitment: &Commitment) -> Result<bool, SettlementError> {
        // TODO: re-derive/check the stored signature.
        Err(SettlementError::Storage("not yet implemented".into()))
    }

    async fn get_commitment(&self, _batch_id: uuid::Uuid) -> Result<Commitment, SettlementError> {
        // TODO: look up by batch id.
        Err(SettlementError::BatchNotFound)
    }
}
