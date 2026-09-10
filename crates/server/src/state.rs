use std::sync::Arc;

use avalon_chain::PostgresSettlementProvider;
use avalon_indexer::postgres::PostgresIndexer;
use sqlx::PgPool;
use webauthn_rs::prelude::Webauthn;

use crate::presence::PresenceStore;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub chain: PostgresSettlementProvider,
    /// The query/index layer (issue #42) — `handlers::register_finish` and
    /// `update_profile` call `indexer.apply_in_tx` in the same transaction
    /// as the identity/outbox rows instead of writing `profiles`
    /// themselves. See `docs/architecture/query-and-indexing.md`.
    pub indexer: PostgresIndexer,
    /// Built once at startup from `AVALON_WEBAUTHN_RP_ID`/`AVALON_WEBAUTHN_ORIGIN`.
    /// `Webauthn` itself isn't cheap to reconstruct (origin parsing/validation),
    /// so it's shared behind an `Arc` rather than rebuilt per request.
    pub webauthn: Arc<Webauthn>,
    /// In-process, per-node presence state (issue #16). Never persisted —
    /// see `crate::presence` module docs / ADR #78.
    pub presence: PresenceStore,
    /// `AVALON_SETTLEMENT_SUBMIT_KEY` (issue #313) — the bearer credential
    /// `POST /ledger/submit` requires. `None` means the endpoint refuses
    /// every request rather than accepting an unauthenticated one; see
    /// `crate::settlement::submit_ledger_batch`.
    pub settlement_submit_key: Option<String>,
}
