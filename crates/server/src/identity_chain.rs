//! Server-side entry points for per-identity event chains: assigning a
//! chain position to an event an identity authors, and refusing operations
//! that depend on knowing which key controls a forked identity.

use avalon_indexer::identity_chain_store;
use avalon_protocol::events::ProtocolEvent;
use sqlx::{PgExecutor, Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;

/// Assigns `event` its place in its identity's chain within `tx`, so the
/// chain state, the write and the outbox entry commit together. A no-op for
/// kinds that are not chained.
pub async fn assign(
    tx: &mut Transaction<'_, Postgres>,
    event: &mut ProtocolEvent,
) -> Result<(), sqlx::Error> {
    identity_chain_store::assign_local(tx, event).await
}

/// Refuses with [`AppError::IdentityChainForked`] while `identity_id`'s
/// chain is forked.
pub async fn ensure_not_forked<'e>(
    exec: impl PgExecutor<'e>,
    identity_id: Uuid,
) -> Result<(), AppError> {
    if identity_chain_store::is_forked(exec, identity_id).await? {
        return Err(AppError::IdentityChainForked);
    }
    Ok(())
}
