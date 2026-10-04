//! The full `avalon-server` startup/run sequence: shared by the plain
//! `avalon-server` binary and, with the `bundled-postgres` feature,
//! `avalon-server-bundled`. Moved out of `src/main.rs` so both binaries
//! run the exact same sequence rather than duplicating it.

use std::sync::Arc;

use crate::{
    auth, guild_messages, internal_role, migrate, mirror_push, mirror_watcher, nodes, outbox,
    replication, retention,
    state::{AppState, IndexerHandle},
};
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

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
pub fn init_tracing() -> crate::admin::LogReloadHandle {
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

/// Initializes tracing, then runs the full startup/serve sequence. Plain
/// `avalon-server`'s whole `main()`; `avalon-server-bundled` instead calls
/// [`init_tracing`] and [`run_with_tracing`] separately — see that
/// function's own doc comment.
pub async fn run() {
    avalon_devenv::load();
    let handle = init_tracing();
    run_with_tracing(handle, crate::shutdown::install()).await;
}

/// Runs the full startup/serve sequence with tracing already initialized —
/// split out from [`run`] so `avalon-server-bundled` can start its managed
/// Postgres (and have that show up in the logs) between initializing
/// tracing and everything else that follows.
pub async fn run_with_tracing(
    log_reload_handle: crate::admin::LogReloadHandle,
    shutdown: crate::shutdown::Shutdown,
) {
    avalon_devenv::load();
    let shutdown_timeout = crate::shutdown::timeout_from_env();
    tracing::info!(
        timeout_secs = shutdown_timeout.as_secs(),
        "avalon-server: graceful shutdown bound"
    );
    match crate::node_keys::apply_from_env(&crate::known_list::data_dir_from_env()) {
        Ok(keys) => {
            if !keys.generated.is_empty() {
                tracing::info!(keys = ?keys.generated, "generated node keys on first boot");
            }
            if !keys.loaded.is_empty() {
                tracing::info!(keys = ?keys.loaded, "loaded node keys from the data directory");
            }
            if let Some(id) = &keys.own_shard_id {
                tracing::info!(shard = %id, "authoring this node's self-certifying shard");
            }
        }
        Err(e) => {
            tracing::error!("refusing to start: {e}");
            std::process::exit(1);
        }
    }
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let addr = std::env::var("AVALON_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());

    // Issue #664: `AVALON_NODE_ROLES` stops being purely advisory here —
    // `settlement_only` is the one combination this process actually gates
    // startup on (see `crate::nodes::is_settlement_only`'s own doc
    // comment for why it's deliberately narrow). Every other roles value,
    // including the `combined` default, gets exactly today's behavior.
    let node_roles = crate::nodes::node_roles();
    let settlement_only = crate::nodes::is_settlement_only(&node_roles);
    // Gateway-facing modules (WebAuthn, sessions, guilds, presence,
    // friends, the identity locator, ...) are wired up for every roles
    // configuration except a standalone Settlement node.
    let gateway_enabled = !settlement_only;
    tracing::info!(
        roles = %node_roles.join(","),
        settlement_only,
        "avalon-server: resolved node roles"
    );

    // WebAuthn settings are required only when this node serves logins; replica-only and
    // settlement-only nodes fall back to an unreachable placeholder.
    let rp_id = std::env::var("AVALON_WEBAUTHN_RP_ID").ok();
    let rp_origin = std::env::var("AVALON_WEBAUTHN_ORIGIN").ok();
    let webauthn_required = gateway_enabled && !crate::replica::replica_only_from_env();
    let webauthn = match (rp_id, rp_origin) {
        (Some(id), Some(origin)) if gateway_enabled => Arc::new(
            auth::build_webauthn(&id, &origin)
                .expect("failed to build Webauthn instance — check AVALON_WEBAUTHN_RP_ID/AVALON_WEBAUTHN_ORIGIN"),
        ),
        (id, origin) => {
            if webauthn_required {
                id.expect("AVALON_WEBAUTHN_RP_ID must be set");
                origin.expect("AVALON_WEBAUTHN_ORIGIN must be set");
            }
            Arc::new(
                auth::build_webauthn("localhost", "http://localhost")
                    .expect("placeholder Webauthn config must itself be valid"),
            )
        }
    };
    // Issue #173: which network this process believes it's part of — e.g.
    // `avalon-mainnet-1` or `avalon-dev-<name>`. Never defaulted; a missing
    // value is a misconfiguration, not "assume dev."
    let network_id = std::env::var("AVALON_NETWORK_ID")
        .expect("AVALON_NETWORK_ID must be set — see .env.example");
    // Hardening on top of the length-prefixed encoding in the signed identity bytes, which is
    // unambiguous even with ':' in the id.
    if network_id.is_empty()
        || network_id
            .chars()
            .any(|c| c == ':' || c.is_whitespace() || c.is_control())
    {
        panic!(
            "AVALON_NETWORK_ID {network_id:?} is invalid: it must be non-empty and contain no ':', whitespace or control characters (it is part of signed identity bytes)"
        );
    }

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

    let dir = migrate::dir_override_from_env();
    let source = match &dir {
        Some(d) => migrate::MigrationSource::Dir(d),
        None => migrate::MigrationSource::Embedded,
    };
    migrate::migrate_up(&pool, source)
        .await
        .expect("failed to run migrations");

    // Creates this ledger's genesis on a fresh database, or refuses to start
    // at all if it's already rooted in a different network_id —
    // deliberately fatal, before anything binds a listener or serves a
    // single request.
    let chain = avalon_chain::PostgresSettlementProvider::connect(pool.clone(), &network_id)
        .await
        .unwrap_or_else(|e| {
            tracing::error!("refusing to start: {e}");
            std::process::exit(1);
        });
    tracing::info!(network_id = %chain.network_id(), "avalon-server: ledger network_id");

    // Issue #662: `AVALON_NODE_ROLES` now load-bearing for the Indexer role
    // specifically (`combined`, the default, counts as every role — same
    // semantics `avalon-docs/architecture/nodes/README.md`'s capability table already
    // assumes) — a process that doesn't include `indexer` in its roles has
    // no local `PostgresIndexer` at all, and instead routes every indexer
    // read/write over #661's `RemoteIndexer`/`/internal/indexer/*`
    // protocol to another node that does. Resolved once here so both
    // `AppState`'s own `indexer` field and the mirror-watcher spawn below
    // (which always gets its own independent local `PostgresIndexer` —
    // see that spawn's own comment) can be built off the same decision.
    let node_roles = nodes::node_roles();
    let indexer = if nodes::indexer_role_is_local(&node_roles) {
        IndexerHandle::Local(avalon_indexer::postgres::PostgresIndexer::new(pool.clone()))
    } else {
        // Issue #665: `RemoteIndexer::from_env` now distinguishes "unset"
        // (`Ok(None)`) from "set but malformed" (`Err`) — both are still
        // fatal here (this process has no way to reach an Indexer role
        // either way), but the malformed case now gets its own clear
        // message instead of being silently treated the same as unset.
        match internal_role::RemoteIndexer::from_env() {
            Ok(Some(remote)) => IndexerHandle::Remote(remote),
            Ok(None) => {
                tracing::error!(
                    "refusing to start: AVALON_NODE_ROLES={node_roles:?} excludes \"indexer\" \
                     (and isn't \"combined\"), but AVALON_INDEXER_REMOTE_URL is unset — this \
                     process has no way to reach an Indexer role, local or remote; set \
                     AVALON_INDEXER_REMOTE_URL (and AVALON_INTERNAL_ROLE_KEY) to point at a node \
                     that does, or include \"indexer\"/\"combined\" in AVALON_NODE_ROLES to run \
                     one locally"
                );
                std::process::exit(1);
            }
            Err(e) => {
                tracing::error!("refusing to start: {e}");
                std::process::exit(1);
            }
        }
    };

    let peers = crate::nodes::PeerTable::new();
    // Issue #599, Layer 2: this node's anti-entropy view of every shard it
    // currently knows exists — see `crate::nodes::ShardRegistry`'s
    // own doc comment. Always constructed (no config needed); starts empty
    // and grows purely from gossip plus this node's own authoritative
    // shard, if any.
    let shard_registry = crate::nodes::ShardRegistry::new();
    // This node's bounded head-summary gossip tracker — see
    // `crate::nodes::HeadGossipTracker`'s own doc comment. Always
    // constructed (no config needed), starts empty.
    let head_gossip = crate::nodes::HeadGossipTracker::new();

    // This node's identity key: the libp2p identity and the key that signs node-to-node write
    // requests, so it exists with the DHT off too. The DHT (on by default,
    // `AVALON_DHT_ENABLED=false`/`0` opts out) is resolved, and the swarm bound and its worker
    // spawned, before `announce_config` below so the very first announce already carries it.
    let identity = crate::dht::load_or_generate_identity_from_env().unwrap_or_else(|e| {
        tracing::error!("refusing to start: {e}");
        std::process::exit(1);
    });
    let own_peer_id = libp2p::PeerId::from(identity.public());
    match crate::node_http::NodeSigner::new(&identity, chain.network_id()) {
        Some(signer) => crate::node_http::install_node_signer(signer),
        None => {
            tracing::error!("refusing to start: cannot sign node requests (empty network id)");
            std::process::exit(1);
        }
    }
    let dht_config =
        crate::dht::DhtConfig::from_env(chain.network_id(), identity).unwrap_or_else(|e| {
            tracing::error!("refusing to start: {e}");
            std::process::exit(1);
        });
    // Announced even without a swarm, so peers can give this node standing for its signed
    // requests; the swarm's own reachability replaces the unknown one below.
    let mut dht_identity = auth_only_identity(&own_peer_id);
    let mut dht_commands = None;
    let mut dht_router_slot = None;
    let mut reachability = crate::reachability::ReachabilityHandle::unknown();
    if let Some(dht_config) = dht_config {
        let node_http_settings =
            crate::node_http::NodeHttpSettings::from_env().unwrap_or_else(|e| {
                tracing::error!("refusing to start: {e}");
                std::process::exit(1);
            });
        let handle =
            crate::dht::start_with_node_http(peers.clone(), dht_config, node_http_settings).await;
        crate::node_http::install_stream_handle(handle.node_http.clone());
        handle.router_slot.set_shutdown(shutdown.clone());
        dht_router_slot = Some(handle.router_slot.clone());
        tracing::info!(peer_id = %handle.peer_id, "avalon-server: libp2p DHT identity");
        dht_identity = crate::nodes::DhtIdentity {
            peer_id: handle.peer_id.to_string(),
            reachability: handle.reachability.clone(),
        };
        reachability = handle.reachability;
        dht_commands = Some(handle.commands);
    }

    // Moved ahead of `state`'s own construction (from its previous
    // location just before `nodes::run_worker`'s spawn) so `own_base_url`
    // is available here too, for issue #583's interest-refresh worker
    // below — reusing the exact same `AVALON_NODE_URL` identity rather
    // than a second parse of it.
    let mut announce_config = crate::nodes::AnnounceConfig::from_env(chain.network_id())
        .with_p2p_fallback(dht_commands.as_ref().map(|_| dht_identity.peer_id.as_str()));
    if clients_share_one_source(
        announce_config.own_http_base_url().is_some(),
        &addr,
        crate::trusted_proxies::TrustedProxies::from_env().is_empty(),
    ) {
        tracing::warn!(
            "avalon-server: AVALON_TRUSTED_PROXIES is empty but this node looks to be behind a \
             reverse proxy (AVALON_NODE_URL set or a loopback listener): every client then shares \
             the proxy's address, which feeds the per-IP limits and failure counts. List the proxy \
             addresses in AVALON_TRUSTED_PROXIES."
        );
    }
    let witness_signer =
        crate::witness_cosign::WitnessCosignConfig::from_env().and_then(|w| w.announce_signer());
    announce_config.witness = witness_signer.clone();

    // Issue #583: always constructed (cheap, no config) so `crate::chat`'s
    // subscribe handlers have one code path regardless of whether the DHT
    // itself is enabled — see `AppState::interest`'s own doc comment.
    let (interest, interest_newly_active) = crate::interest::InterestRegistry::new();
    // Issue #585: resolved here (ahead of `redis_limiter` below, which is
    // #545's own separate, differently-scoped Redis use) so it's ready
    // before `interest::run_worker` spawns just below. `AVALON_REDIS_URL`
    // being unset returns `None` here exactly as it does for
    // `RedisLimiterState::from_env()`.
    // Issue #517: host resource metrics for `GET /nodes/status`'s
    // `resources` block — always spawned (no config gate, unlike heavier
    // opt-in workers above), since reading a handful of `sysinfo`-provided
    // host stats every few seconds is cheap and diagnostic-only.
    let host_metrics_sampler = crate::resources::HostMetricsSampler::from_env();
    tokio::spawn(crate::resources::start_sampler(
        host_metrics_sampler.clone(),
    ));

    let interest_redis_fast_path =
        crate::interest::RedisFastPath::from_env(chain.network_id()).await;
    if interest_redis_fast_path.is_some() {
        tracing::info!(
            "avalon-server: interest-lookup Redis fast-path enabled (AVALON_REDIS_URL set)"
        );
    }
    if let Some(dht_commands) = dht_commands.clone() {
        tokio::spawn(crate::interest::run_worker(
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
        //
        // Issue #664: also gated on `gateway_enabled` — a Settlement-only
        // node never runs `crate::auth`/`passkeys`, so it never durably
        // holds an `identity_signing_keys` row for anything; this worker
        // would just be an empty scan on every tick.
        if gateway_enabled {
            tokio::spawn(crate::identity_locator::run_worker(
                pool.clone(),
                interest.clone(),
            ));
        }
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

    // This node's own witness known list, built here
    // (rather than down by its own `run_worker` spawn, its previous
    // location) so `AppState::known_list` can hold the same handle that
    // `/ledger/sth/*` verification and mirror sync read live — a
    // membership change (new witness admitted, one dropped) is visible to
    // every reader immediately, with no restart.
    let known_list_handle = crate::known_list::KnownListHandle::load_or_new(
        crate::known_list::KnownListConfig::from_env(),
        Some(crate::known_list::known_list_path(
            &crate::known_list::data_dir_from_env(),
        )),
    );

    // Issue #313/#532: built here (rather than down by `run_worker`'s own
    // spawn, its previous location) so its issue #526 failure-tracking
    // handle can be threaded into `AppState` below, before `remote_submit`
    // itself is moved into the worker.
    let remote_submit = outbox::RemoteSubmitConfig::from_env();
    if remote_submit.is_some() {
        tracing::info!("avalon-server: outbox committing via remote Settlement authority (AVALON_SETTLEMENT_REMOTE_URL set)");
    }
    // Issue #664: `AVALON_NODE_ROLES` excluding `settlement` declares that
    // this process isn't meant to be a Settlement authority of its own —
    // #313's `remote_submit` (above) is the mechanism that actually makes
    // that true for the outbox write path. This doesn't hard-fail when the
    // two disagree (a `chain`/ledger still exists locally either way, per
    // `AppState::chain`'s own required field — see `avalon-docs/architecture/nodes/README.md`
    // for the full reasoning), but
    // it's worth a loud warning: without `AVALON_SETTLEMENT_REMOTE_URL(S)`,
    // this node's outbox worker falls back to committing locally despite
    // its own declared roles saying it shouldn't be a Settlement authority.
    if !node_roles
        .iter()
        .any(|r| r == "combined" || r == "settlement")
        && remote_submit.is_none()
    {
        tracing::warn!(
            roles = %node_roles.join(","),
            "avalon-server: AVALON_NODE_ROLES excludes settlement but AVALON_SETTLEMENT_REMOTE_URL(S) \
             is unset — this node will still commit outbox writes to its own local ledger, not a \
             remote Settlement authority; set AVALON_SETTLEMENT_REMOTE_URL(S) if that's not intended"
        );
    }

    // Issue #573: `AVALON_OWN_SHARD_ID`, defaulting to `"core"` — every
    // pre-#573 deployment's implicit single shard. Resolved here (rather
    // than inline in `state` below, its previous location) so #599's
    // shard-gossip worker can be told the same value.
    let replica_only = crate::replica::replica_only_from_env();
    if replica_only && std::env::var("AVALON_OWN_SHARD_ID").is_ok_and(|v| !v.trim().is_empty()) {
        tracing::error!(
            "refusing to start: AVALON_REPLICA_ONLY=true conflicts with AVALON_OWN_SHARD_ID; a \
             replica authors no shard. Unset AVALON_OWN_SHARD_ID, or unset AVALON_REPLICA_ONLY \
             to author that shard"
        );
        std::process::exit(1);
    }
    crate::replica::set_replica_only(replica_only);
    let own_shard_id = if replica_only {
        tracing::info!(
            "avalon-server: replica-only mode, authoring no shard and signing no tree heads"
        );
        crate::replica::NO_AUTHORED_SHARD.to_string()
    } else {
        std::env::var("AVALON_OWN_SHARD_ID").unwrap_or_else(|_| "core".to_string())
    };
    let shard_check = if replica_only {
        Ok(())
    } else {
        avalon_protocol::shard::parse_shard_id(&own_shard_id).map(|_| ())
    };
    if let Err(e) = shard_check {
        tracing::error!(
            "refusing to start: AVALON_OWN_SHARD_ID={own_shard_id:?} is invalid: {e}. Use `core` \
             (only for the network's pinned core authority), a registered shard id of the form \
             `game|app|service:<integrator-slug>[/<instance>]`, or a self-certifying \
             `node:<key-hash>` id derived from this node's own key \
             (avalon_protocol::shard_identity::derive_self_certifying_id)"
        );
        std::process::exit(1);
    }

    // A node committing `own_shard_id` through a remote authority does not sign it locally.
    let signs_own_shard_locally = !replica_only
        && remote_submit
            .as_ref()
            .is_none_or(|r| !r.targets().contains_key(own_shard_id.as_str()));
    if signs_own_shard_locally {
        if let Ok((signing_key, signing_key_id)) = avalon_protocol::sth::load_signing_key_from_env()
        {
            let node_verify_key_hex = hex::encode(signing_key.verifying_key().to_bytes());
            let peers_configured = ["AVALON_BOOTSTRAP_PEERS", "AVALON_MIRROR_PEERS"]
                .iter()
                .any(|var| {
                    std::env::var(var)
                        .map(|v| v.split(',').any(|s| !s.trim().is_empty()))
                        .unwrap_or(false)
                });
            let decision = crate::core_author_guard::evaluate(
                &crate::core_author_guard::CoreAuthorInputs {
                    own_shard_id: &own_shard_id,
                    network_id: &network_id,
                    node_verify_key_hex: &node_verify_key_hex,
                    node_signing_key_id: &signing_key_id,
                    peers_configured,
                },
                avalon_protocol::network_trust::bundled_trust_anchors(),
            );
            match &decision {
                crate::core_author_guard::CoreAuthorDecision::Refuse(msg) => {
                    tracing::error!("refusing to start: {msg}");
                    std::process::exit(1);
                }
                crate::core_author_guard::CoreAuthorDecision::WarnUnpinned(msg) => {
                    tracing::warn!("{msg}");
                }
                _ => {}
            }
            crate::core_author_guard::record_outcome(&decision);
        }
    }

    mirror_watcher::set_default_core_mirror_peers(
        mirror_watcher::resolve_default_core_mirror_peers(
            &own_shard_id,
            std::env::var("AVALON_MIRROR_PEERS").ok().as_deref(),
            &network_id,
            avalon_protocol::network_trust::bundled_trust_anchors(),
        ),
    );
    let mirror_peers_configured = mirror_watcher::effective_mirror_peers()
        .split(',')
        .any(|s| !s.trim().is_empty());
    if let Some(msg) = crate::core_author_guard::missing_core_mirror_advisory(
        &own_shard_id,
        mirror_peers_configured,
    ) {
        tracing::warn!("{msg}");
    }

    // Issue #629, implementing #622's decision: this node's own record of
    // which peers have confirmed mirroring which shard, plus its resolved
    // minimum-replication gate config — see `crate::replication`'s
    // module doc comment. Both always constructed (no config needed to
    // exist; the gate's own defaults are what apply when nothing is set).
    let mirror_confirmations = crate::replication::MirrorConfirmationRegistry::new();
    let replication_gate = replication::ReplicationGateConfig::from_env();

    // Extends `AVALON_NODE_ROLES` from purely-advertised
    // metadata into a real, load-bearing gate for the `realtime` role —
    // same mechanism the Indexer and Settlement roles also use for
    // consistency. `realtime_remote_url` is `None` when this process
    // holds the role itself (serves `/ws/presence`/`/ws/messages`
    // locally, exactly as every deployment before this issue); `Some(url)`
    // proxies every WebSocket connection through to that remote Realtime
    // node instead (see `crate::realtime_proxy`'s module doc comment for
    // the full design). A role list excluding
    // `realtime` with no valid `AVALON_REALTIME_URL` refuses to start,
    // same "fail loudly, never silently degrade" precedent every other
    // startup-time check in this function already establishes.
    let node_roles = crate::nodes::node_roles();
    let realtime_remote_url =
        crate::nodes::realtime_mode_from_env(&node_roles).unwrap_or_else(|e| {
            tracing::error!("refusing to start: {e}");
            std::process::exit(1);
        });
    match &realtime_remote_url {
        None => {
            tracing::info!(roles = ?node_roles, "avalon-server: serving realtime (presence/chat WebSocket) locally")
        }
        Some(url) => {
            tracing::info!(roles = ?node_roles, remote = %url, "avalon-server: proxying realtime WebSocket connections to a remote Realtime node")
        }
    }

    // Issue #665: every configured backing-service URL above (Indexer,
    // Realtime, Settlement) has now been validated as *well-formed* —
    // this is the one place that also confirms each is actually
    // *reachable* right now, a check none of #662/#663/#664 added on
    // their own. Deliberately a warning, never a startup failure (see
    // `crate::backing_services`'s own module doc comment): a
    // backing service that's briefly down (e.g. mid rolling-restart) is a
    // normal operational moment this process should still start through,
    // unlike the missing/malformed cases above, which stay fatal.
    {
        let mut backing_service_targets = Vec::new();
        if let IndexerHandle::Remote(remote) = &indexer {
            backing_service_targets.push(crate::backing_services::BackingServiceTarget::new(
                "indexer",
                remote.base_url(),
            ));
        }
        if let Some(url) = &realtime_remote_url {
            backing_service_targets.push(crate::backing_services::BackingServiceTarget::new(
                "realtime", url,
            ));
        }
        if let Some(remote_submit) = &remote_submit {
            for (shard_id, url) in remote_submit.targets() {
                backing_service_targets.push(crate::backing_services::BackingServiceTarget::new(
                    format!("settlement (shard {shard_id})"),
                    url,
                ));
            }
        }
        if !backing_service_targets.is_empty() {
            let client = reqwest::Client::new();
            crate::backing_services::check_all_reachable(&client, &backing_service_targets).await;
        }
    }

    let redis_limiter = crate::redis_limits::RedisLimiterState::from_env().await;
    let state = AppState {
        pool: pool.clone(),
        chain: chain.clone(),
        indexer,
        webauthn,
        presence: crate::presence::PresenceStore::from_env(),
        chat: crate::chat::ChatBus::new(),
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
        // Issue #529: see `crate::cross_shard::KnownShardsConfig`'s
        // own doc comment — `None` falls back to the one-shard degenerate
        // case, no config needed for a milestone-1 deployment.
        known_shards: crate::cross_shard::KnownShardsConfig::from_env(),
        // Issue #526: `None` when `remote_submit` itself is `None` —
        // nothing this node could ever report as failing.
        remote_submit_status: remote_submit.as_ref().map(|r| r.status()),
        own_shard_id: own_shard_id.clone(),
        shard_mirror_sources: crate::settlement::ShardMirrorSources::from_env(),
        interest,
        dht_commands,
        reachability,
        own_witness: witness_signer.clone(),
        replica_intake: crate::chat_replication::ReplicaIntake::new(&node_roles),
        own_base_url: announce_config.own_base_url.clone(),
        own_libp2p_peer_id: Some(dht_identity.peer_id.clone()),
        interest_redis_fast_path,
        principal_limiter: crate::principal_limits::PrincipalLimiter::from_env(
            redis_limiter.as_ref(),
        ),
        mirror_wake: mirror_wake.clone(),
        host_metrics: host_metrics_sampler.clone(),
        shard_registry: shard_registry.clone(),
        head_gossip: head_gossip.clone(),
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
        mirror_confirmations: mirror_confirmations.clone(),
        replication_gate,
        realtime_remote_url,
        known_list: known_list_handle.clone(),
    };

    // Node-tiered durable history retention, implementing
    // the decided policy — always printed, so an operator always sees which
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
    // see crates/server/src/outbox.rs. `AVALON_SETTLEMENT_REMOTE_URL`
    // switches this from committing locally to posting each
    // batch to a remote Settlement authority; unset (the default), nothing
    // changes. `remote_submit` itself was built earlier, above `state`'s
    // own construction — see that site's comment.
    //
    // Gated on `gateway_enabled` — the outbox table only ever
    // gets rows from Gateway-facing handlers (identity/guild/etc. writes
    // sharing a transaction with their own outbox insert); a Settlement-only
    // node never runs any of those handlers, so its own outbox table stays
    // empty and this worker would have nothing to drain. Managed-hosting's
    // two-phase flow (`POST /ledger/prepare-batch`/`finalize-batch`)
    // and `POST /ledger/submit` commit directly, bypassing the
    // outbox entirely — neither depends on this worker running.
    let outbox_worker = if gateway_enabled && !replica_only {
        Some(tokio::spawn(outbox::run_worker(
            pool.clone(),
            chain.clone(),
            remote_submit,
            mirror_push_config,
            shutdown.clone(),
        )))
    } else {
        None
    };

    tokio::spawn(crate::sessions::run_prune_worker(state.clone()));

    // Hard-deletes guild message archive rows past their retention window —
    // see crates/server/src/guild_messages.rs. Gateway-only:
    // guild chat archives only exist because a Gateway
    // handler wrote them.
    if gateway_enabled {
        tokio::spawn(guild_messages::run_archive_expiry_worker(state.clone()));
    }

    // Mirror-watcher, implementing the decided design:
    // watches whatever peers `AVALON_MIRROR_PEERS` names, verifying and
    // storing their STHs, detecting equivocation, and backfilling entry
    // content — see crates/server/src/mirror_watcher.rs. Only spawned when
    // configured (either `AVALON_MIRROR_PEERS` or, per issue #599,
    // `AVALON_MIRROR_ALL_DISCOVERED_SHARDS=true`), same "only run what's
    // actually turned on" pattern the retention worker above uses.
    if let Some(mirror_config) = mirror_watcher::MirrorWatcherConfig::from_env() {
        // Issue #662: deliberately its own local `PostgresIndexer`, not
        // `state.indexer.clone()` — mirror-sync (verifying/storing peers'
        // STHs and entries, backfilling content) is a different concern
        // from the Gateway/Indexer role split #662 is about, and isn't
        // gated by `AVALON_NODE_ROLES` the same way. It already writes
        // mirrored entries straight to this process's own local Postgres
        // regardless of role; best-effort applying them to a local indexer
        // projection too (see this module's own doc comment on why that's
        // best-effort, not required for correctness) only makes sense
        // against this process's own local Postgres, never over the
        // network — a `RemoteIndexer` would just add a pointless HTTP hop
        // to what's already a local, same-transaction savepoint apply.
        tokio::spawn(mirror_watcher::run_worker(
            pool.clone(),
            chain.clone(),
            avalon_indexer::postgres::PostgresIndexer::new(pool.clone()),
            mirror_config,
            mirror_watcher::MirrorWatcherHandles {
                interest: state.interest.clone(),
                shard_registry: shard_registry.clone(),
                own_base_url: state.own_base_url.clone(),
                wake: mirror_wake.clone(),
                own_shard_id: own_shard_id.clone(),
                known_list: state.known_list.clone(),
                head_gossip: head_gossip.clone(),
                witness: crate::witness_cosign::WitnessCosignConfig::from_env(),
                peers: state.peers.clone(),
                trust_anchors: avalon_protocol::network_trust::bundled_trust_anchors().to_vec(),
            },
        ));
    }

    if signs_own_shard_locally {
        tokio::spawn(crate::author_cosign_gather::run_worker(
            chain.clone(),
            state.known_list.clone(),
            state.peers.clone(),
            own_shard_id.clone(),
        ));
    }

    if let Some(witness) = crate::witness_cosign::WitnessCosignConfig::from_env() {
        if signs_own_shard_locally {
            tokio::spawn(crate::witness_cosign::run_self_cosign_worker(
                chain.clone(),
                pool.clone(),
                witness.clone(),
                head_gossip.clone(),
                own_shard_id.clone(),
            ));
        }
        tokio::spawn(crate::witness_cosign::run_reattest_worker(
            chain.clone(),
            pool.clone(),
            witness,
            head_gossip.clone(),
            crate::witness_cosign::reattest_interval_from_env(),
        ));
    }

    // Minimum replication guarantee — spawned unconditionally,
    // same posture the announce worker just below takes: even a node with
    // no peers known yet still needs this loop running so it picks up
    // peers (and therefore confirmed-mirror counts) the moment any appear.
    // See `crate::replication`'s module doc comment.
    tokio::spawn(replication::run_worker(
        chain.network_id().to_string(),
        peers.clone(),
        shard_registry.clone(),
        mirror_confirmations,
        own_shard_id.clone(),
        replication::ReplicationConfig::from_env(),
    ));

    // Issue #946: this node's own witness known list — a much smaller,
    // purpose-specific structure than the peer table above (see
    // `crate::known_list`'s own module doc comment for the
    // boundary). Spawned unconditionally, same posture the announce worker
    // just below takes: even a node with no peers yet still has its
    // bundled anchors to try to admit, and needs probation/freshness
    // maintenance running regardless. The handle itself was built earlier
    // (before `state`) and lives on in `state.known_list`; this worker gets
    // its own clone, mutating the same underlying persisted list.
    tokio::spawn(crate::known_list::run_worker(
        peers.clone(),
        chain.network_id().to_string(),
        state.known_list.clone(),
        crate::known_list::refill_interval_from_env(),
    ));

    // Node-to-node announce/bootstrap discovery — spawned
    // unconditionally, unlike the mirror-watcher above: even this
    // network's anchor node (an empty resolved peer list) still needs to
    // serve announce/list-peers requests from everyone else. See
    // `crate::nodes`'s module doc for why. (`announce_config` itself is
    // built earlier now — see that site's comment.)
    tokio::spawn(crate::nodes::run_worker(
        chain.clone(),
        peers,
        shard_registry.clone(),
        head_gossip.clone(),
        known_list_handle.clone(),
        own_shard_id.clone(),
        announce_config,
        Some(dht_identity),
    ));

    // Issue #545: per-hoster shared rate-limit/concurrency-ceiling state
    // across this operator's own processes — `None` (the default,
    // AVALON_REDIS_URL unset) keeps `router` on its existing in-process
    // layers. See `crate::redis_limits`'s own module doc comment.
    if redis_limiter.is_some() {
        tracing::info!(
            "avalon-server: rate limit / concurrency ceiling backed by Redis (AVALON_REDIS_URL set)"
        );
    }
    // Issue #664: a genuinely standalone Settlement node gets the reduced
    // route table (`/ledger/*`, `/nodes/*`, `/mirror/notify` only) — every
    // other roles configuration, including the `combined` default, gets
    // today's full router unchanged. See
    // `crate::router_settlement_only`'s own doc comment.
    let app = if settlement_only {
        tracing::info!("avalon-server: Settlement-only mode — no Gateway-facing routes mounted");
        crate::router_settlement_only(state, redis_limiter)
    } else {
        crate::router(state, redis_limiter)
    };
    if let Some(slot) = dht_router_slot {
        slot.set(app.clone());
    }

    tracing::info!(%addr, "avalon-server listening");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind address");
    // `crate::serve` in place of `axum::serve`: it inserts `ConnectInfo`
    // itself (the rate limiter's IP-fallback key extractor needs it) and
    // also configures hyper's HTTP/1 header-read timeout, which
    // `axum::serve` has no hook for.
    shutdown.mark_running();
    crate::serve::serve(
        listener,
        app,
        crate::header_read_timeout_from_env(),
        shutdown.clone(),
        shutdown_timeout,
    )
    .await;

    // The outbox commits each batch in one transaction, so waiting for its current tick keeps
    // a request's write and its ledger entry together; other workers are cancelled at their
    // next await point when the runtime drops.
    if let Some(worker) = outbox_worker {
        if tokio::time::timeout(shutdown_timeout, worker)
            .await
            .is_err()
        {
            tracing::warn!("outbox worker did not stop within the shutdown timeout");
        }
    }
    shutdown.begin_final_stop();
    tracing::info!("avalon-server stopped");
}

/// Whether every client probably reaches this node through one proxy address: a public URL or a
/// loopback listener, with no trusted proxy configured.
fn clients_share_one_source(
    has_public_url: bool,
    listen_addr: &str,
    no_trusted_proxies: bool,
) -> bool {
    let loopback = listen_addr
        .parse::<std::net::SocketAddr>()
        .is_ok_and(|a| a.ip().is_loopback());
    no_trusted_proxies && (has_public_url || loopback)
}

/// The identity a node announces when no swarm runs: its id for signed requests, no addresses
/// and no connectivity claim.
fn auth_only_identity(peer_id: &libp2p::PeerId) -> crate::nodes::DhtIdentity {
    crate::nodes::DhtIdentity {
        peer_id: peer_id.to_string(),
        reachability: crate::reachability::ReachabilityHandle::unknown(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shared_source_warning_needs_a_proxy_shaped_setup_and_no_trusted_proxies() {
        assert!(clients_share_one_source(true, "0.0.0.0:8080", true));
        assert!(clients_share_one_source(false, "127.0.0.1:8080", true));
        assert!(!clients_share_one_source(false, "0.0.0.0:8080", true));
        assert!(!clients_share_one_source(true, "127.0.0.1:8080", false));
    }

    #[test]
    fn a_node_without_a_swarm_announces_its_id_with_no_addresses_or_connectivity() {
        let key = libp2p::identity::Keypair::generate_ed25519();
        let id = libp2p::PeerId::from(key.public());
        let announced = auth_only_identity(&id);
        assert_eq!(announced.peer_id, id.to_string());
        assert!(announced.reachability.advertised_addrs().is_empty());
        assert_eq!(
            crate::reachability::connectivity_for(&announced.reachability.snapshot()),
            None
        );
        // The same key signs, and without a swarm the node does not announce as `p2p://`.
        let signer = crate::node_http::NodeSigner::new(&key, "net").unwrap();
        assert_eq!(signer.peer_id(), announced.peer_id);
        let config = crate::nodes::AnnounceConfig::from_env("net").with_p2p_fallback(None);
        assert!(!config
            .own_base_url
            .as_deref()
            .is_some_and(crate::node_http::is_p2p_url));
    }
}
