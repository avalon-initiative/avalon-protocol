//! Avalon's network-facing service: identity, auth, social, guilds,
//! achievement verification, game registration.
//!
//! This is the one thing `hub` and `mobile-hub` are clients of — per the
//! decision that Hub is frontend-only and never becomes its own backend.
//! Games integrate against this service through `avalon-sdk`, not directly.
//!
//! Milestone 1, Epic: Identity & Player Profile — identity/auth endpoints
//! only. Social/guilds/achievements/games come with their own epics.

use std::path::Path;
use std::sync::Arc;

use avalon_server::{auth, migrate, outbox, state::AppState};
use sqlx::postgres::PgPoolOptions;

fn migrations_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("db/migrations")
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let addr = std::env::var("AVALON_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let webauthn_rp_id =
        std::env::var("AVALON_WEBAUTHN_RP_ID").expect("AVALON_WEBAUTHN_RP_ID must be set");
    let webauthn_origin =
        std::env::var("AVALON_WEBAUTHN_ORIGIN").expect("AVALON_WEBAUTHN_ORIGIN must be set");

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres");

    migrate::migrate_up(&pool, &migrations_dir())
        .await
        .expect("failed to run migrations");

    let webauthn = Arc::new(
        auth::build_webauthn(&webauthn_rp_id, &webauthn_origin)
            .expect("failed to build Webauthn instance — check AVALON_WEBAUTHN_RP_ID/AVALON_WEBAUTHN_ORIGIN"),
    );
    let chain = avalon_chain::PostgresSettlementProvider::new(pool.clone());
    let indexer = avalon_indexer::postgres::PostgresIndexer::new(pool.clone());

    // Drains the identity/etc. outbox into the ledger at its own pace —
    // see crates/server/src/outbox.rs (issue #71).
    tokio::spawn(outbox::run_worker(pool.clone(), chain.clone()));

    let app = avalon_server::router(AppState {
        pool,
        chain,
        indexer,
        webauthn,
        presence: avalon_server::presence::PresenceStore::from_env(),
    });

    println!("avalon-server listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind address");
    axum::serve(listener, app).await.expect("server error");
}
