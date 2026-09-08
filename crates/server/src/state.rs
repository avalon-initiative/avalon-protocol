use std::sync::Arc;

use avalon_chain::PostgresSettlementProvider;
use sqlx::PgPool;
use webauthn_rs::prelude::Webauthn;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub chain: PostgresSettlementProvider,
    /// Built once at startup from `AVALON_WEBAUTHN_RP_ID`/`AVALON_WEBAUTHN_ORIGIN`.
    /// `Webauthn` itself isn't cheap to reconstruct (origin parsing/validation),
    /// so it's shared behind an `Arc` rather than rebuilt per request.
    pub webauthn: Arc<Webauthn>,
}
