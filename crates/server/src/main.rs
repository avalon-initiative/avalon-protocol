//! Avalon's network-facing service: identity, auth, social, guilds,
//! achievement verification, integrator registration.
//!
//! This is the one thing `hub` and `mobile-hub` are clients of — per the
//! decision that Hub is frontend-only and never becomes its own backend.
//! Integrators integrate against this service through `avalon-sdk`, not directly.
//!
//! Milestone 1, Epic: Identity & Player Profile — identity/auth endpoints
//! only. Social/guilds/achievements/integrations come with their own epics.

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

    // tracing-subscriber's fmt layer defaults to ANSI color on regardless of
    // whether stdout is a real terminal, which litters log files (e.g. under
    // `make start`, which redirects to a file) with raw escape codes.
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());

    let registry = tracing_subscriber::registry().with(env_filter);
    if json_format {
        registry
            .with(tracing_subscriber::fmt::layer().json())
            .init();
    } else {
        registry
            .with(tracing_subscriber::fmt::layer().with_ansi(ansi))
            .init();
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

    // Issue #363, implementing #287's decision: every hoster-configurable
    // resource limit defaults to exactly what this process hardcoded
    // before — an unconfigured node behaves exactly as it always has, not
    // "unlimited" and not "fails to start."
    let max_db_connections = std::env::var("AVALON_MAX_DB_CONNECTIONS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(10);

    let pool = PgPoolOptions::new()
        .max_connections(max_db_connections)
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

    let peers = avalon_server::nodes::PeerTable::new();

    // Issue #313/#532: built here (rather than down by `run_worker`'s own
    // spawn, its previous location) so its issue #526 failure-tracking
    // handle can be threaded into `AppState` below, before `remote_submit`
    // itself is moved into the worker.
    let remote_submit = outbox::RemoteSubmitConfig::from_env();
    if remote_submit.is_some() {
        tracing::info!("avalon-server: outbox committing via remote Settlement authority (AVALON_SETTLEMENT_REMOTE_URL set)");
    }

    let state = AppState {
        pool: pool.clone(),
        chain: chain.clone(),
        indexer,
        webauthn,
        presence: avalon_server::presence::PresenceStore::from_env(),
        chat: avalon_server::chat::ChatBus::new(),
        settlement_submit_key: std::env::var("AVALON_SETTLEMENT_SUBMIT_KEY")
            .ok()
            .filter(|s| !s.is_empty()),
        peers: peers.clone(),
        // Issue #531: interim, single-key managed-hosting verify key —
        // see `AppState::managed_hosting_verify_key`'s own doc comment.
        managed_hosting_verify_key: std::env::var("AVALON_MANAGED_HOSTING_VERIFY_KEY")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|hex_value| {
                let bytes = hex::decode(&hex_value)
                    .expect("AVALON_MANAGED_HOSTING_VERIFY_KEY must be valid hex");
                let bytes: [u8; 32] = bytes.try_into().unwrap_or_else(|v: Vec<u8>| {
                    panic!(
                        "AVALON_MANAGED_HOSTING_VERIFY_KEY must decode to exactly 32 bytes, got {}",
                        v.len()
                    )
                });
                ed25519_dalek::VerifyingKey::from_bytes(&bytes)
                    .expect("AVALON_MANAGED_HOSTING_VERIFY_KEY is not a valid Ed25519 key")
            }),
        // Issue #529: see `avalon_server::cross_shard::KnownShardsConfig`'s
        // own doc comment — `None` falls back to the one-shard degenerate
        // case, no config needed for a milestone-1 deployment.
        known_shards: avalon_server::cross_shard::KnownShardsConfig::from_env(),
        // Issue #526: `None` when `remote_submit` itself is `None` —
        // nothing this node could ever report as failing.
        remote_submit_status: remote_submit.as_ref().map(|r| r.status()),
        // Issue #573: `AVALON_OWN_SHARD_ID`, defaulting to `"core"` —
        // every pre-#573 deployment's implicit single shard, so unset
        // means zero behavior change.
        own_shard_id: std::env::var("AVALON_OWN_SHARD_ID").unwrap_or_else(|_| "core".to_string()),
        shard_mirror_sources: avalon_server::settlement::ShardMirrorSources::from_env(),
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
    // changes. `remote_submit` itself was built earlier, above `state`'s
    // own construction — see that site's comment.
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

    // Node-to-node announce/bootstrap discovery (issue #362) — spawned
    // unconditionally, unlike the mirror-watcher above: even this
    // network's anchor node (an empty resolved peer list) still needs to
    // serve announce/list-peers requests from everyone else. See
    // `crate::nodes`'s module doc for why.
    let announce_config = avalon_server::nodes::AnnounceConfig::from_env(chain.network_id());
    tokio::spawn(avalon_server::nodes::run_worker(
        chain.clone(),
        peers,
        announce_config,
    ));

    // Issue #545: per-hoster shared rate-limit/concurrency-ceiling state
    // across this operator's own processes — `None` (the default,
    // AVALON_REDIS_URL unset) keeps `router` on its existing in-process
    // layers. See `avalon_server::redis_limits`'s own module doc comment.
    let redis_limiter = avalon_server::redis_limits::RedisLimiterState::from_env().await;
    if redis_limiter.is_some() {
        tracing::info!(
            "avalon-server: rate limit / concurrency ceiling backed by Redis (AVALON_REDIS_URL set)"
        );
    }
    let app = avalon_server::router(state, redis_limiter);

    tracing::info!(%addr, "avalon-server listening");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind address");
    // Issue #363: the rate limiter's IP-fallback key extractor
    // (`IntegratorOrIpKeyExtractor`) needs the real peer address in
    // `ConnectInfo`, which only `into_make_service_with_connect_info`
    // populates — the plain `into_make_service()` this used to be leaves
    // it unset.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .expect("server error");
}
