use avalon_chain::PostgresSettlementProvider;
use sqlx::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub chain: PostgresSettlementProvider,
}
