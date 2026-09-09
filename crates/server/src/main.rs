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

use avalon_server::{auth, guild_messages, migrate, outbox, retention, state::AppState};
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
    // Issue #173: which network this process believes it's part of — e.g.
    // `avalon-mainnet-1` or `avalon-dev-<name>`. Never defaulted; a missing
    // value is a misconfiguration, not "assume dev."
    let network_id = std::env::var("AVALON_NETWORK_ID")
        .expect("AVALON_NETWORK_ID must be set — see .env.example");

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
    // Creates this ledger's genesis on a fresh database, or refuses to start
    // at all if it's already rooted in a different network_id (issue #173) —
    // deliberately fatal, before anything binds a listener or serves a
    // single request.
    let chain = avalon_chain::PostgresSettlementProvider::connect(pool.clone(), &network_id)
        .await
        .unwrap_or_else(|e| {
            eprintln!("refusing to start: {e}");
            std::process::exit(1);
        });
    println!("avalon-server: ledger network_id = {}", chain.network_id());
    let indexer = avalon_indexer::postgres::PostgresIndexer::new(pool.clone());

    let state = AppState {
        pool: pool.clone(),
        chain: chain.clone(),
        indexer,
        webauthn,
        presence: avalon_server::presence::PresenceStore::from_env(),
    };

    // Node-tiered durable history retention (issue #208, implementing
    // #180's decision) — always printed, so an operator always sees which
    // tier and pruning state this process is running as, the same
    // transparency the network_id line above gives. See
    // `avalon_chain::retention`'s module doc comment for the full design
    // and its milestone-1 availability caveat.
    let retention_config =
        avalon_chain::retention::RetentionConfig::from_env().unwrap_or_else(|e| {
            eprintln!("refusing to start: {e}");
            std::process::exit(1);
        });
    println!(
        "avalon-server: retention tier = {}",
        retention_config.describe()
    );
    if retention_config.should_prune() {
        tokio::spawn(retention::run_worker(chain.clone(), retention_config));
    }

    // Drains the identity/etc. outbox into the ledger at its own pace —
    // see crates/server/src/outbox.rs (issue #71).
    tokio::spawn(outbox::run_worker(pool.clone(), chain.clone()));

    // Hard-deletes guild message archive rows past their retention window —
    // see crates/server/src/guild_messages.rs (issue #253).
    tokio::spawn(guild_messages::run_archive_expiry_worker(state.clone()));

    let app = avalon_server::router(state);

    println!("avalon-server listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind address");
    axum::serve(listener, app).await.expect("server error");
}
