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

use avalon_server::{
    auth, guild_messages, migrate, mirror_watcher, outbox, retention, state::AppState,
};
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

fn migrations_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("db/migrations")
}

/// Issue #265: structured logging via `tracing`, replacing the bare
/// `println!`/`eprintln!` call sites this crate used to have. Level is
/// `RUST_LOG`-style env-filter controlled (defaults to `info` for this
/// crate, `warn` for dependencies, if `RUST_LOG` is unset) — configurable
/// without a rebuild, per this ticket's own invariant. Output format is
/// selectable via `AVALON_LOG_FORMAT`: `json` for a log-aggregator-friendly
/// (Grafana/Loki, etc.) shape, anything else (including unset, the default)
/// for a human-readable dev format.
fn init_tracing() {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,tower_http=info"));
    let json_format = std::env::var("AVALON_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    let registry = tracing_subscriber::registry().with(env_filter);
    if json_format {
        registry
            .with(tracing_subscriber::fmt::layer().json())
            .init();
    } else {
        registry.with(tracing_subscriber::fmt::layer()).init();
    }
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    init_tracing();
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
            tracing::error!("refusing to start: {e}");
            std::process::exit(1);
        });
    tracing::info!(network_id = %chain.network_id(), "avalon-server: ledger network_id");
    let indexer = avalon_indexer::postgres::PostgresIndexer::new(pool.clone());

    let state = AppState {
        pool: pool.clone(),
        chain: chain.clone(),
        indexer,
        webauthn,
        presence: avalon_server::presence::PresenceStore::from_env(),
        settlement_submit_key: std::env::var("AVALON_SETTLEMENT_SUBMIT_KEY")
            .ok()
            .filter(|s| !s.is_empty()),
    };

    // Node-tiered durable history retention (issue #208, implementing
    // #180's decision) — always printed, so an operator always sees which
    // tier and pruning state this process is running as, the same
    // transparency the network_id line above gives. See
    // `avalon_chain::retention`'s module doc comment for the full design
    // and its milestone-1 availability caveat.
    let retention_config =
        avalon_chain::retention::RetentionConfig::from_env().unwrap_or_else(|e| {
            tracing::error!("refusing to start: {e}");
            std::process::exit(1);
        });
    tracing::info!(tier = %retention_config.describe(), "avalon-server: retention tier");
    if retention_config.should_prune() {
        tokio::spawn(retention::run_worker(chain.clone(), retention_config));
    }

    // Drains the identity/etc. outbox into the ledger at its own pace —
    // see crates/server/src/outbox.rs (issue #71). `AVALON_SETTLEMENT_REMOTE_URL`
    // (issue #313) switches this from committing locally to posting each
    // batch to a remote Settlement authority; unset (the default), nothing
    // changes.
    let remote_submit = outbox::RemoteSubmitConfig::from_env();
    if remote_submit.is_some() {
        tracing::info!("avalon-server: outbox committing via remote Settlement authority (AVALON_SETTLEMENT_REMOTE_URL set)");
    }
    tokio::spawn(outbox::run_worker(
        pool.clone(),
        chain.clone(),
        remote_submit,
    ));

    // Hard-deletes guild message archive rows past their retention window —
    // see crates/server/src/guild_messages.rs (issue #253).
    tokio::spawn(guild_messages::run_archive_expiry_worker(state.clone()));

    // Mirror-watcher (issue #299, implementing #40's decided design):
    // watches whatever peers `AVALON_MIRROR_PEERS` names, verifying and
    // storing their STHs, detecting equivocation, and backfilling entry
    // content — see crates/server/src/mirror_watcher.rs. Only spawned when
    // configured, same "only run what's actually turned on" pattern the
    // retention worker above uses.
    if let Some(mirror_config) = mirror_watcher::MirrorWatcherConfig::from_env() {
        tokio::spawn(mirror_watcher::run_worker(
            pool.clone(),
            chain.clone(),
            state.indexer.clone(),
            mirror_config,
        ));
    }

    let app = avalon_server::router(state);

    tracing::info!(%addr, "avalon-server listening");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind address");
    axum::serve(listener, app).await.expect("server error");
}
