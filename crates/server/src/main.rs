//! Avalon's network-facing service: identity, auth, social, guilds,
//! achievement verification, integrator registration.
//!
//! This is the one thing `hub` and `hub-app` are clients of — per the
//! decision that Hub is frontend-only and never becomes its own backend.
//! Integrators integrate against this service through `avalon-sdk`, not directly.
//!
//! Milestone 1, Epic: Identity & Player Profile — identity/auth endpoints
//! only. Social/guilds/achievements/integrations come with their own epics.

use std::path::Path;
use std::sync::Arc;

use avalon_server::{
    auth, guild_messages, internal_role, migrate, mirror_push, mirror_watcher, nodes, outbox,
    replication, retention,
    state::{AppState, IndexerHandle},
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
    avalon_devenv::load();
    let log_reload_handle = init_tracing();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let addr = std::env::var("AVALON_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());

    // Issue #664: `AVALON_NODE_ROLES` stops being purely advisory here —
    // `settlement_only` is the one combination this process actually gates
    // startup on (see `avalon_server::nodes::is_settlement_only`'s own doc
    // comment for why it's deliberately narrow). Every other roles value,
    // including the `combined` default, gets exactly today's behavior.
    let node_roles = avalon_server::nodes::node_roles();
    let settlement_only = avalon_server::nodes::is_settlement_only(&node_roles);
    // Gateway-facing modules (WebAuthn, sessions, guilds, presence,
    // friends, the identity locator, ...) are wired up for every roles
    // configuration except a standalone Settlement node.
    let gateway_enabled = !settlement_only;
    tracing::info!(
        roles = %node_roles.join(","),
        settlement_only,
        "avalon-server: resolved node roles"
    );

    // Issue #664: WebAuthn is a Gateway-only concept (login/registration) —
    // a Settlement-only node never mounts a single auth route, so it
    // neither needs `AVALON_WEBAUTHN_RP_ID`/`AVALON_WEBAUTHN_ORIGIN`
    // configured nor gets a real `Webauthn` instance built from them. The
    // placeholder built here is never reachable from any handler
    // (`router_settlement_only` mounts none of `crate::auth`/`passkeys`/
    // `recovery`/`device_pairing`'s routes) — it only exists because
    // `AppState::webauthn` is a plain `Arc<Webauthn>`, not `Option`, to
    // keep every Gateway handler's own signature unchanged.
    let webauthn = if gateway_enabled {
        let webauthn_rp_id =
            std::env::var("AVALON_WEBAUTHN_RP_ID").expect("AVALON_WEBAUTHN_RP_ID must be set");
        let webauthn_origin =
            std::env::var("AVALON_WEBAUTHN_ORIGIN").expect("AVALON_WEBAUTHN_ORIGIN must be set");
        Arc::new(
            auth::build_webauthn(&webauthn_rp_id, &webauthn_origin)
                .expect("failed to build Webauthn instance — check AVALON_WEBAUTHN_RP_ID/AVALON_WEBAUTHN_ORIGIN"),
        )
    } else {
        Arc::new(
            auth::build_webauthn("localhost", "http://localhost")
                .expect("placeholder Settlement-only Webauthn config must itself be valid"),
        )
    };
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
    // semantics `docs/projects/backend-server/architecture/nodes.md`'s capability table already
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

    let peers = avalon_server::nodes::PeerTable::new();
    // Issue #599, Layer 2: this node's anti-entropy view of every shard it
    // currently knows exists — see `avalon_server::nodes::ShardRegistry`'s
    // own doc comment. Always constructed (no config needed); starts empty
    // and grows purely from gossip plus this node's own authoritative
    // shard, if any.
    let shard_registry = avalon_server::nodes::ShardRegistry::new();
    // This node's bounded head-summary gossip tracker — see
    // `avalon_server::nodes::HeadGossipTracker`'s own doc comment. Always
    // constructed (no config needed), starts empty.
    let head_gossip = avalon_server::nodes::HeadGossipTracker::new();

    // This node's libp2p DHT identity — on by default
    // (`AVALON_DHT_ENABLED=false`/`0` opts out), resolved (and
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
    let mut announce_config = avalon_server::nodes::AnnounceConfig::from_env(chain.network_id());
    let witness_signer = avalon_server::witness_cosign::WitnessCosignConfig::from_env()
        .and_then(|w| w.announce_signer());
    announce_config.witness = witness_signer.clone();

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

    let interest_redis_fast_path =
        avalon_server::interest::RedisFastPath::from_env(chain.network_id()).await;
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
        //
        // Issue #664: also gated on `gateway_enabled` — a Settlement-only
        // node never runs `crate::auth`/`passkeys`, so it never durably
        // holds an `identity_signing_keys` row for anything; this worker
        // would just be an empty scan on every tick.
        if gateway_enabled {
            tokio::spawn(avalon_server::identity_locator::run_worker(
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
    let known_list_handle = avalon_server::known_list::KnownListHandle::load_or_new(
        avalon_server::known_list::KnownListConfig::from_env(),
        Some(avalon_server::known_list::known_list_path(
            &avalon_server::known_list::data_dir_from_env(),
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
    // `AppState::chain`'s own required field — see `docs/projects/backend-server/architecture/nodes.md`
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
    let own_shard_id = std::env::var("AVALON_OWN_SHARD_ID").unwrap_or_else(|_| "core".to_string());
    if let Err(e) = avalon_protocol::shard::parse_shard_id(&own_shard_id) {
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
    let signs_own_shard_locally = remote_submit
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
            let decision = avalon_server::core_author_guard::evaluate(
                &avalon_server::core_author_guard::CoreAuthorInputs {
                    own_shard_id: &own_shard_id,
                    network_id: &network_id,
                    node_verify_key_hex: &node_verify_key_hex,
                    node_signing_key_id: &signing_key_id,
                    peers_configured,
                },
                avalon_protocol::network_trust::bundled_trust_anchors(),
            );
            match &decision {
                avalon_server::core_author_guard::CoreAuthorDecision::Refuse(msg) => {
                    tracing::error!("refusing to start: {msg}");
                    std::process::exit(1);
                }
                avalon_server::core_author_guard::CoreAuthorDecision::WarnUnpinned(msg) => {
                    tracing::warn!("{msg}");
                }
                _ => {}
            }
            avalon_server::core_author_guard::record_outcome(&decision);
        }
    }

    let mirror_peers_configured = std::env::var("AVALON_MIRROR_PEERS")
        .map(|v| v.split(',').any(|s| !s.trim().is_empty()))
        .unwrap_or(false);
    if let Some(msg) = avalon_server::core_author_guard::missing_core_mirror_advisory(
        &own_shard_id,
        mirror_peers_configured,
    ) {
        tracing::warn!("{msg}");
    }

    // Issue #629, implementing #622's decision: this node's own record of
    // which peers have confirmed mirroring which shard, plus its resolved
    // minimum-replication gate config — see `avalon_server::replication`'s
    // module doc comment. Both always constructed (no config needed to
    // exist; the gate's own defaults are what apply when nothing is set).
    let mirror_confirmations = avalon_server::replication::MirrorConfirmationRegistry::new();
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
    let node_roles = avalon_server::nodes::node_roles();
    let realtime_remote_url = avalon_server::nodes::realtime_mode_from_env(&node_roles)
        .unwrap_or_else(|e| {
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
    // `avalon_server::backing_services`'s own module doc comment): a
    // backing service that's briefly down (e.g. mid rolling-restart) is a
    // normal operational moment this process should still start through,
    // unlike the missing/malformed cases above, which stay fatal.
    {
        let mut backing_service_targets = Vec::new();
        if let IndexerHandle::Remote(remote) = &indexer {
            backing_service_targets.push(
                avalon_server::backing_services::BackingServiceTarget::new(
                    "indexer",
                    remote.base_url(),
                ),
            );
        }
        if let Some(url) = &realtime_remote_url {
            backing_service_targets.push(
                avalon_server::backing_services::BackingServiceTarget::new("realtime", url),
            );
        }
        if let Some(remote_submit) = &remote_submit {
            for (shard_id, url) in remote_submit.targets() {
                backing_service_targets.push(
                    avalon_server::backing_services::BackingServiceTarget::new(
                        format!("settlement (shard {shard_id})"),
                        url,
                    ),
                );
            }
        }
        if !backing_service_targets.is_empty() {
            let client = reqwest::Client::new();
            avalon_server::backing_services::check_all_reachable(&client, &backing_service_targets)
                .await;
        }
    }

    let redis_limiter = avalon_server::redis_limits::RedisLimiterState::from_env().await;
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
        own_witness: witness_signer.clone(),
        own_base_url: announce_config.own_base_url.clone(),
        own_libp2p_peer_id: dht_identity.as_ref().map(|d| d.peer_id.clone()),
        interest_redis_fast_path,
        principal_limiter: avalon_server::principal_limits::PrincipalLimiter::from_env(
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
    if gateway_enabled {
        tokio::spawn(outbox::run_worker(
            pool.clone(),
            chain.clone(),
            remote_submit,
            mirror_push_config,
        ));
    }

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
                witness: avalon_server::witness_cosign::WitnessCosignConfig::from_env(),
                peers: state.peers.clone(),
                trust_anchors: avalon_protocol::network_trust::bundled_trust_anchors().to_vec(),
            },
        ));
    }

    // Minimum replication guarantee — spawned unconditionally,
    // same posture the announce worker just below takes: even a node with
    // no peers known yet still needs this loop running so it picks up
    // peers (and therefore confirmed-mirror counts) the moment any appear.
    // See `avalon_server::replication`'s module doc comment.
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
    // `avalon_server::known_list`'s own module doc comment for the
    // boundary). Spawned unconditionally, same posture the announce worker
    // just below takes: even a node with no peers yet still has its
    // bundled anchors to try to admit, and needs probation/freshness
    // maintenance running regardless. The handle itself was built earlier
    // (before `state`) and lives on in `state.known_list`; this worker gets
    // its own clone, mutating the same underlying persisted list.
    tokio::spawn(avalon_server::known_list::run_worker(
        peers.clone(),
        chain.network_id().to_string(),
        state.known_list.clone(),
        avalon_server::known_list::refill_interval_from_env(),
    ));

    // Node-to-node announce/bootstrap discovery — spawned
    // unconditionally, unlike the mirror-watcher above: even this
    // network's anchor node (an empty resolved peer list) still needs to
    // serve announce/list-peers requests from everyone else. See
    // `crate::nodes`'s module doc for why. (`announce_config` itself is
    // built earlier now — see that site's comment.)
    tokio::spawn(avalon_server::nodes::run_worker(
        chain.clone(),
        peers,
        shard_registry.clone(),
        head_gossip.clone(),
        own_shard_id.clone(),
        announce_config,
        dht_identity,
    ));

    // Issue #545: per-hoster shared rate-limit/concurrency-ceiling state
    // across this operator's own processes — `None` (the default,
    // AVALON_REDIS_URL unset) keeps `router` on its existing in-process
    // layers. See `avalon_server::redis_limits`'s own module doc comment.
    if redis_limiter.is_some() {
        tracing::info!(
            "avalon-server: rate limit / concurrency ceiling backed by Redis (AVALON_REDIS_URL set)"
        );
    }
    // Issue #664: a genuinely standalone Settlement node gets the reduced
    // route table (`/ledger/*`, `/nodes/*`, `/mirror/notify` only) — every
    // other roles configuration, including the `combined` default, gets
    // today's full router unchanged. See
    // `avalon_server::router_settlement_only`'s own doc comment.
    let app = if settlement_only {
        tracing::info!("avalon-server: Settlement-only mode — no Gateway-facing routes mounted");
        avalon_server::router_settlement_only(state, redis_limiter)
    } else {
        avalon_server::router(state, redis_limiter)
    };

    tracing::info!(%addr, "avalon-server listening");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind address");
    // `crate::serve` in place of `axum::serve`: it inserts `ConnectInfo`
    // itself (the rate limiter's IP-fallback key extractor needs it) and
    // also configures hyper's HTTP/1 header-read timeout, which
    // `axum::serve` has no hook for.
    avalon_server::serve::serve(listener, app, avalon_server::header_read_timeout_from_env()).await;
}
