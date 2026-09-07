//! Postgres-backed `SettlementProvider` — milestone 1's only implementation.
//!
//! Deliberately stubbed out at scaffolding time: which Postgres driver to use
//! (sqlx vs. tokio-postgres vs. diesel-async) is its own small decision, not
//! yet made. The shape below is what the rest of the workspace should be able
//! to compile against once that decision lands and this is filled in.

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
