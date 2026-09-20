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
    auth, guild_messages, migrate, mirror_push, mirror_watcher, outbox, retention, state::AppState,
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
///
/// Issue #658: the filter is wrapped in a `reload::Layer` and its `Handle`
/// returned, so `POST /nodes/log-level` (`crate::admin`) can swap the
/// active filter live, without a restart — the initial value below is just
/// the *starting* filter, not a fixed-for-the-process-lifetime one anymore.
fn init_tracing() -> avalon_server::admin::LogReloadHandle {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,tower_http=info"));
    let (filter_layer, reload_handle) = tracing_subscriber::reload::Layer::new(env_filter);
    let json_format = std::env::var("AVALON_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    // tracing-subscriber's fmt layer defaults to ANSI color on regardless of
    // whether stdout is a real terminal, which litters log files (e.g. under
    // `make start`, which redirects to a file) with raw escape codes.
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());

    let registry = tracing_subscriber::registry().with(filter_layer);
    if json_format {
        registry
            .with(tracing_subscriber::fmt::layer().json())
            .init();
    } else {
        registry
            .with(tracing_subscriber::fmt::layer().with_ansi(ansi))
            .init();
    }
    reload_handle
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    let log_reload_handle = init_tracing();
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
    // Issue #599, Layer 2: this node's anti-entropy view of every shard it
    // currently knows exists — see `avalon_server::nodes::ShardRegistry`'s
    // own doc comment. Always constructed (no config needed); starts empty
    // and grows purely from gossip plus this node's own authoritative
    // shard, if any.
    let shard_registry = avalon_server::nodes::ShardRegistry::new();

    // Issue #582/#580: this node's libp2p DHT identity — on by default as
    // of ADR #593 (`AVALON_DHT_ENABLED=false`/`0` opts out), resolved (and
    // the swarm bound and its worker spawned) before `announce_config`
    // below so this node's very first outbound announce already carries
    // it.
    let dht_config =
        avalon_server::dht::DhtConfig::from_env(chain.network_id()).unwrap_or_else(|e| {
            tracing::error!("refusing to start: {e}");
            std::process::exit(1);
        });
    let mut dht_identity = None;
    let mut dht_commands = None;
    if let Some(dht_config) = dht_config {
        let handle = avalon_server::dht::start(peers.clone(), dht_config).await;
        tracing::info!(peer_id = %handle.peer_id, "avalon-server: libp2p DHT identity");
        dht_identity = Some(avalon_server::nodes::DhtIdentity {
            peer_id: handle.peer_id.to_string(),
            listen_addrs: handle.listen_addrs.iter().map(|a| a.to_string()).collect(),
        });
        dht_commands = Some(handle.commands);
    }

    // Moved ahead of `state`'s own construction (from its previous
    // location just before `nodes::run_worker`'s spawn) so `own_base_url`
    // is available here too, for issue #583's interest-refresh worker
    // below — reusing the exact same `AVALON_NODE_URL` identity rather
    // than a second parse of it.
    let announce_config = avalon_server::nodes::AnnounceConfig::from_env(chain.network_id());

    // Issue #583: always constructed (cheap, no config) so `crate::chat`'s
    // subscribe handlers have one code path regardless of whether the DHT
    // itself is enabled — see `AppState::interest`'s own doc comment.
    let (interest, interest_newly_active) = avalon_server::interest::InterestRegistry::new();
    // Issue #585: resolved here (ahead of `redis_limiter` below, which is
    // #545's own separate, differently-scoped Redis use) so it's ready
    // before `interest::run_worker` spawns just below. `AVALON_REDIS_URL`
    // being unset returns `None` here exactly as it does for
    // `RedisLimiterState::from_env()`.
    // Issue #517: host resource metrics for `GET /nodes/status`'s
    // `resources` block — always spawned (no config gate, unlike heavier
    // opt-in workers above), since reading a handful of `sysinfo`-provided
    // host stats every few seconds is cheap and diagnostic-only.
    let host_metrics_sampler = avalon_server::resources::HostMetricsSampler::from_env();
    tokio::spawn(avalon_server::resources::start_sampler(
        host_metrics_sampler.clone(),
    ));

    let interest_redis_fast_path = avalon_server::interest::RedisFastPath::from_env().await;
    if interest_redis_fast_path.is_some() {
        tracing::info!(
            "avalon-server: interest-lookup Redis fast-path enabled (AVALON_REDIS_URL set)"
        );
    }
    if let Some(dht_commands) = dht_commands.clone() {
        tokio::spawn(avalon_server::interest::run_worker(
            interest.clone(),
            interest_newly_active,
            dht_commands,
            announce_config.own_base_url.clone(),
            interest_redis_fast_path.clone(),
        ));
        // Epic #623, issue #635: identity locator — registers DHT interest
        // for every identity this node durably has signing keys for. Gated
        // on a real DHT identity existing, same as `interest::run_worker`
        // just above, whose already-running refresh loop is what actually
        // keeps each registration's DHT record alive.
        tokio::spawn(avalon_server::identity_locator::run_worker(
            pool.clone(),
            interest.clone(),
        ));
    }

    // Issue #596: `Some` only when this node has a DHT identity to look
    // up interested peers through at all — `None` end to end makes
    // `outbox`'s push call site a no-op, exactly its behavior before this
    // ticket existed. See `crate::mirror_push`'s own module doc comment.
    let mirror_push_config = dht_commands.clone().map(|dht_commands| {
        mirror_push::MirrorPushConfig::new(
            dht_commands,
            interest_redis_fast_path.clone(),
            announce_config.own_base_url.clone(),
        )
    });
    // Issue #596: shared between `mirror_watcher::run_worker` (if spawned)
    // and `POST /mirror/notify`'s handler — always constructed, same
    // "cheap, no config" posture as `interest` above.
    let mirror_wake = std::sync::Arc::new(tokio::sync::Notify::new());

    // Issue #313/#532: built here (rather than down by `run_worker`'s own
    // spawn, its previous location) so its issue #526 failure-tracking
    // handle can be threaded into `AppState` below, before `remote_submit`
    // itself is moved into the worker.
    let remote_submit = outbox::RemoteSubmitConfig::from_env();
    if remote_submit.is_some() {
        tracing::info!("avalon-server: outbox committing via remote Settlement authority (AVALON_SETTLEMENT_REMOTE_URL set)");
    }

    // Issue #573: `AVALON_OWN_SHARD_ID`, defaulting to `"core"` — every
    // pre-#573 deployment's implicit single shard. Resolved here (rather
    // than inline in `state` below, its previous location) so #599's
    // shard-gossip worker can be told the same value.
    let own_shard_id = std::env::var("AVALON_OWN_SHARD_ID").unwrap_or_else(|_| "core".to_string());

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
        own_shard_id: own_shard_id.clone(),
        shard_mirror_sources: avalon_server::settlement::ShardMirrorSources::from_env(),
        interest,
        dht_commands,
        own_base_url: announce_config.own_base_url.clone(),
        interest_redis_fast_path,
        mirror_wake: mirror_wake.clone(),
        host_metrics: host_metrics_sampler.clone(),
        shard_registry: shard_registry.clone(),
        // Issue #658: deliberately a *separate* shared secret from
        // `settlement_submit_key` above — see `crate::admin`'s own module
        // doc comment for why that key's trust domain doesn't fit here.
        admin_token: std::env::var("AVALON_ADMIN_TOKEN")
            .ok()
            .filter(|s| !s.is_empty()),
        log_reload_handle,
        // Issue #661: deliberately a *third* shared secret, distinct from
        // both `settlement_submit_key` and `admin_token` above — see
        // `AppState::internal_role_key`'s own doc comment.
        internal_role_key: std::env::var("AVALON_INTERNAL_ROLE_KEY")
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
    // changes. `remote_submit` itself was built earlier, above `state`'s
    // own construction — see that site's comment.
    tokio::spawn(outbox::run_worker(
        pool.clone(),
        chain.clone(),
        remote_submit,
        mirror_push_config,
    ));

    // Hard-deletes guild message archive rows past their retention window —
    // see crates/server/src/guild_messages.rs (issue #253).
    tokio::spawn(guild_messages::run_archive_expiry_worker(state.clone()));

    // Mirror-watcher (issue #299, implementing #40's decided design):
    // watches whatever peers `AVALON_MIRROR_PEERS` names, verifying and
    // storing their STHs, detecting equivocation, and backfilling entry
    // content — see crates/server/src/mirror_watcher.rs. Only spawned when
    // configured (either `AVALON_MIRROR_PEERS` or, per issue #599,
    // `AVALON_MIRROR_ALL_DISCOVERED_SHARDS=true`), same "only run what's
    // actually turned on" pattern the retention worker above uses.
    if let Some(mirror_config) = mirror_watcher::MirrorWatcherConfig::from_env() {
        tokio::spawn(mirror_watcher::run_worker(
            pool.clone(),
            chain.clone(),
            state.indexer.clone(),
            mirror_config,
            mirror_watcher::MirrorWatcherHandles {
                interest: state.interest.clone(),
                shard_registry: shard_registry.clone(),
                own_base_url: state.own_base_url.clone(),
                wake: mirror_wake.clone(),
                own_shard_id: own_shard_id.clone(),
            },
        ));
    }

    // Node-to-node announce/bootstrap discovery (issue #362) — spawned
    // unconditionally, unlike the mirror-watcher above: even this
    // network's anchor node (an empty resolved peer list) still needs to
    // serve announce/list-peers requests from everyone else. See
    // `crate::nodes`'s module doc for why. (`announce_config` itself is
    // built earlier now — see that site's comment.)
    tokio::spawn(avalon_server::nodes::run_worker(
        chain.clone(),
        peers,
        shard_registry.clone(),
        own_shard_id.clone(),
        announce_config,
        dht_identity,
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
