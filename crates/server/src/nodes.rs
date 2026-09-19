//! Node-to-node announce/bootstrap discovery — issue #362, implementing
//! #292's decided design: a lightweight, protocol-native peer table, no
//! orchestrator node, no third-party service mesh.
//!
//! `POST /nodes/announce` upserts the caller into this node's local peer
//! table and responds with that table's current contents (excluding the
//! caller). `GET /nodes/peers` is a read-only dump — no auth, same posture
//! `crate::settlement`'s public transparency-log reads already take, since
//! a peer table isn't sensitive the way ledger-write endpoints are.
//! [`run_worker`] re-announces to every configured (or network-seeded)
//! bootstrap peer on its own timer and prunes any locally-known peer not
//! re-announced within a few multiples of that interval.
//!
//! Peer table storage is in-memory only (same pattern `crate::presence`
//! already establishes for ephemeral, non-durable state) — a long-running
//! node simply re-announces and repopulates its table after a restart; no
//! Postgres table is needed unless a real requirement to persist across
//! restarts shows up later.
//!
//! A peer table never merges entries across different `network_id`s: an
//! announcement naming a different network than this node's own is
//! rejected outright (mirrors the genesis-mismatch-is-fatal precedent,
//! issue #173, at the level of one request rather than the whole node).
//! Nothing here ever brokers or is required for reads/writes on the
//! settlement log itself — this is purely peer discovery, independent of
//! #40/#299's mirror-sync trust model.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use avalon_chain::PostgresSettlementProvider;
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::AppError;
use crate::state::AppState;

/// One entry of this node's local peer table. `Deserialize` too: also the
/// shape a peer's own peer-list response is parsed back into.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub base_url: String,
    pub roles: Vec<String>,
    pub protocol_version: String,
    pub network_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub last_announced_at: OffsetDateTime,
    /// Issue #582: this peer's libp2p `PeerId` (`to_string()`'s base58
    /// form), when it runs the DHT (`AVALON_DHT_ENABLED`) — #580's own
    /// identity, held *in addition to* this `base_url`-keyed one, not a
    /// replacement (see `crate::dht`'s module doc for why no existing
    /// identity in this codebase could be reused for it). `#[serde(default)]`
    /// so an older peer's announce/response JSON (no such field yet)
    /// deserializes as `None` rather than failing outright — the same
    /// forward-compatibility posture #368's protocol-version floor already
    /// established for this exact struct.
    #[serde(default)]
    pub libp2p_peer_id: Option<String>,
    /// This peer's dialable libp2p multiaddrs, as reported by its own DHT
    /// swarm at startup — empty when `libp2p_peer_id` is `None`, or when
    /// the peer's own bind produced no externally-visible address in time
    /// (see `crate::dht::start`).
    #[serde(default)]
    pub libp2p_listen_addrs: Vec<String>,
}

/// `Arc<RwLock<_>>` around a plain map, cheap to clone into [`AppState`] —
/// same shape `crate::presence::PresenceStore` already establishes for
/// in-process, non-durable state. Keyed by `base_url`: a peer is uniquely
/// identified by where it's reachable, not by any self-reported id.
#[derive(Clone, Default)]
pub struct PeerTable {
    peers: Arc<RwLock<HashMap<String, PeerInfo>>>,
}

impl PeerTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or refreshes `info`, keyed by its own `base_url`.
    pub fn upsert(&self, info: PeerInfo) {
        self.peers
            .write()
            .expect("peer table lock poisoned")
            .insert(info.base_url.clone(), info);
    }

    /// Issue #368's floor enforcement, shared by `announce`'s handler and
    /// `run_worker`'s gossip merge — the one place a `protocol_version`
    /// claim has any actual effect. `info` below the effective floor is
    /// dropped (logged, never upserted) instead of erroring: a version
    /// claim is a compatibility/availability signal, never a security
    /// gate (#308's cross-cutting invariant), so the worst case is a
    /// self-inflicted, reversible availability loss — the same peer is
    /// admitted normally the moment it reports an upgraded version.
    /// Returns whether `info` was admitted, so a caller (like `announce`)
    /// can decide whether to also log at its own call site.
    pub fn admit_if_supported(&self, info: PeerInfo) -> bool {
        if crate::version::is_supported(&info.protocol_version) {
            self.upsert(info);
            true
        } else {
            tracing::warn!(
                event = "incompatible_peer_version",
                peer = %info.base_url,
                peer_version = %info.protocol_version,
                floor = %crate::version::effective_min_peer_version(),
                "peer's protocol_version is below this node's effective floor — not added to \
                 the peer table; will be admitted normally once it upgrades",
            );
            false
        }
    }

    /// Every known peer except `exclude_base_url` — what `announce`
    /// returns to a caller (never echoing its own entry back to it).
    pub fn list_excluding(&self, exclude_base_url: &str) -> Vec<PeerInfo> {
        self.peers
            .read()
            .expect("peer table lock poisoned")
            .values()
            .filter(|p| p.base_url != exclude_base_url)
            .cloned()
            .collect()
    }

    /// Every known peer — what `GET /nodes/peers` returns.
    pub fn list_all(&self) -> Vec<PeerInfo> {
        self.peers
            .read()
            .expect("peer table lock poisoned")
            .values()
            .cloned()
            .collect()
    }

    /// Drops any entry last announced before `cutoff` — a peer that never
    /// re-announces (crashed, network-partitioned, or genuinely gone)
    /// eventually falls out of the table rather than being remembered
    /// forever.
    pub fn prune_older_than(&self, cutoff: OffsetDateTime) {
        self.peers
            .write()
            .expect("peer table lock poisoned")
            .retain(|_, info| info.last_announced_at >= cutoff);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.peers.read().expect("peer table lock poisoned").len()
    }
}

/// `Serialize` too: this is also the outbound request body `run_worker`
/// sends when announcing itself to a peer.
#[derive(Debug, Serialize, Deserialize)]
pub struct AnnounceRequest {
    pub base_url: String,
    pub roles: Vec<String>,
    pub protocol_version: String,
    pub network_id: String,
    /// Issue #582 — see `PeerInfo::libp2p_peer_id`.
    #[serde(default)]
    pub libp2p_peer_id: Option<String>,
    /// Issue #582 — see `PeerInfo::libp2p_listen_addrs`.
    #[serde(default)]
    pub libp2p_listen_addrs: Vec<String>,
}

/// `Deserialize` too: this is also the shape `run_worker` parses back out
/// of a peer's response.
#[derive(Debug, Serialize, Deserialize)]
pub struct AnnounceResponse {
    pub peers: Vec<PeerInfo>,
}

/// `POST /nodes/announce`. Rejects an announcement naming a different
/// `network_id` than this node's own without touching the peer table —
/// two different networks' peer tables must never merge.
///
/// Issue #368: a caller reporting a `protocol_version` below this node's
/// effective floor is **not** rejected outright the way a `network_id`
/// mismatch is — the request still succeeds and still gets this node's own
/// peer list back (a version claim is a compatibility signal, never a
/// security gate, per #308's cross-cutting invariant), but the caller
/// itself is not upserted into the peer table / gossiped to others.
/// Reversible and self-correcting: the same caller re-announcing with an
/// upgraded version on its next cycle is admitted normally, no manual
/// unban step.
pub async fn announce(
    State(state): State<AppState>,
    Json(body): Json<AnnounceRequest>,
) -> Result<Json<AnnounceResponse>, AppError> {
    if body.network_id != state.chain.network_id() {
        return Err(AppError::PeerNetworkMismatch);
    }

    let caller_base_url = body.base_url.clone();
    state.peers.admit_if_supported(PeerInfo {
        base_url: body.base_url,
        roles: body.roles,
        protocol_version: body.protocol_version,
        network_id: body.network_id,
        last_announced_at: OffsetDateTime::now_utc(),
        libp2p_peer_id: body.libp2p_peer_id,
        libp2p_listen_addrs: body.libp2p_listen_addrs,
    });

    Ok(Json(AnnounceResponse {
        peers: state.peers.list_excluding(&caller_base_url),
    }))
}

/// `GET /nodes/peers` — read-only, no auth beyond whatever this repo
/// already requires for other read endpoints (see module docs).
pub async fn list_peers(State(state): State<AppState>) -> Json<Vec<PeerInfo>> {
    Json(state.peers.list_all())
}

/// This node's own reported operational status — issue #368's "node
/// status/health output... gains its own protocol version and, when known
/// via peer gossip, a staleness flag." No such surface existed before this
/// ticket (checked before assuming a new endpoint was needed, per the
/// ticket body), so `/nodes/status` is it, alongside the peer table it
/// reads from.
#[derive(Debug, Serialize)]
pub struct NodeStatusResponse {
    pub protocol_version: String,
    pub network_id: String,
    /// `true` when some known peer (via #362's peer table) reports a
    /// *newer* `protocol_version` than this node's own — a self-diagnostic
    /// "you may want to upgrade" signal for the operator, never used to
    /// gate anything: the version-floor exclusion above is the only place
    /// a version claim has any actual effect.
    pub stale: bool,
    /// The newest `protocol_version` any known peer currently reports, if
    /// any peer is known and its version parses.
    pub newest_known_peer_version: Option<String>,
    /// Issue #517: this node's own host resource metrics — CPU/memory/disk/
    /// process, plus the DB pool's own tracked size/in-use. Diagnostic-only,
    /// same posture as `stale` above: never used to gate protocol behavior
    /// (peer admission, mirroring, consensus), never a request failure when
    /// a metric can't be read on a given platform (every leaf field is its
    /// own `Option`). See `crate::resources`'s module doc comment for why
    /// `sysinfo` rather than hand-rolled `/proc` parsing.
    pub resources: crate::resources::NodeResourceMetrics,
}

/// `GET /nodes/status` — read-only, same public posture as `list_peers`.
pub async fn status(State(state): State<AppState>) -> Json<NodeStatusResponse> {
    let own_version = semver::Version::parse(crate::version::PROTOCOL_VERSION).ok();
    let newest_known_peer_version = state
        .peers
        .list_all()
        .iter()
        .filter_map(|p| semver::Version::parse(&p.protocol_version).ok())
        .max();

    let stale = match (&own_version, &newest_known_peer_version) {
        (Some(own), Some(newest)) => newest > own,
        _ => false,
    };

    let (cpu, memory, disks, open_file_count, process_uptime_seconds) =
        state.host_metrics.current();
    let pool_size = state.pool.size();
    let resources = crate::resources::NodeResourceMetrics {
        cpu,
        memory,
        disks,
        process_uptime_seconds: Some(process_uptime_seconds),
        open_file_count,
        db_pool: crate::resources::DbPoolMetrics {
            size: Some(pool_size),
            in_use: Some(pool_size.saturating_sub(state.pool.num_idle() as u32)),
        },
    };

    Json(NodeStatusResponse {
        protocol_version: crate::version::PROTOCOL_VERSION.to_string(),
        network_id: state.chain.network_id().to_string(),
        stale,
        newest_known_peer_version: newest_known_peer_version.map(|v| v.to_string()),
        resources,
    })
}

/// This node's own libp2p DHT identity (issue #582), computed once at
/// startup by `crate::dht::start` — `None` when `AVALON_DHT_ENABLED` isn't
/// set, in which case this node announces exactly as it did before #582
/// existed. Threaded into [`run_worker`] so every outbound announce also
/// tells peers how to find this node in the DHT, the same way `roles`/
/// `protocol_version` already do for the HTTP peer table.
#[derive(Debug, Clone)]
pub struct DhtIdentity {
    pub peer_id: String,
    pub listen_addrs: Vec<String>,
}

/// This node's own outbound announce/bootstrap configuration.
pub struct AnnounceConfig {
    /// Base URLs to announce to. Resolved once at startup by
    /// [`Self::from_env`] — either `AVALON_BOOTSTRAP_PEERS` verbatim, or
    /// this network's `seed_nodes` from `docs/trusted-networks.json` if
    /// unset. Empty for a network's anchor node (nothing to seed from
    /// yet) — that's an expected, not an error, configuration.
    pub peers: Vec<String>,
    pub interval: Duration,
    /// This node's own externally-reachable base URL, from
    /// `AVALON_NODE_URL` — required to announce meaningfully (a peer
    /// needs a real URL to reach this node back at), but not to run the
    /// `announce`/`list_peers` endpoints themselves, which work
    /// regardless. `None` here means this node can still be announced TO,
    /// it just can't announce itself anywhere.
    pub own_base_url: Option<String>,
}

const DEFAULT_ANNOUNCE_INTERVAL_SECS: u64 = 180;
/// A peer not re-announced within this many multiples of the announce
/// interval is pruned — generous enough that one or two missed ticks
/// (a transient network blip) never evicts a genuinely live peer.
const PRUNE_INTERVAL_MULTIPLE: u32 = 3;

/// Pure resolution logic, split out for direct unit testing (same "pure
/// function behind the env-reading wrapper" pattern `registry::coarsen`
/// already uses in this repo) — no real `bundled_trust_anchors()` call, so
/// a test can feed
/// it a controlled anchor list instead of depending on
/// `docs/trusted-networks.json`'s actual (currently empty) `seed_nodes`.
///
/// `pub` (issue #511): `avalon-cli`'s `discover-mirror-peers` command
/// reuses this exact same resolution — `AVALON_BOOTSTRAP_PEERS` verbatim,
/// or this network's seed nodes — rather than re-implementing it, so the
/// two never drift apart on what "the configured bootstrap peers" means.
pub fn resolve_bootstrap_peers(
    bootstrap_env: Option<&str>,
    network_id: &str,
    anchors: &[avalon_sdk::network::TrustAnchorEntry],
) -> Vec<String> {
    match bootstrap_env {
        Some(raw) if !raw.trim().is_empty() => raw
            .split(',')
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => anchors
            .iter()
            .find(|entry| entry.network_id == network_id)
            .map(|entry| entry.seed_nodes.clone())
            .unwrap_or_default(),
    }
}

impl AnnounceConfig {
    /// `network_id` is this node's own — used only to resolve the default
    /// seed list when `AVALON_BOOTSTRAP_PEERS` is unset.
    pub fn from_env(network_id: &str) -> Self {
        let peers = resolve_bootstrap_peers(
            std::env::var("AVALON_BOOTSTRAP_PEERS").ok().as_deref(),
            network_id,
            avalon_sdk::network::bundled_trust_anchors(),
        );

        let interval = std::env::var("AVALON_ANNOUNCE_INTERVAL_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|secs| *secs > 0)
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(DEFAULT_ANNOUNCE_INTERVAL_SECS));

        let own_base_url = std::env::var("AVALON_NODE_URL")
            .ok()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty());

        Self {
            peers,
            interval,
            own_base_url,
        }
    }
}

/// `AVALON_NODE_ROLES` — comma-separated (matching `docs/architecture/nodes.md`'s
/// `Settlement`/`Indexer`/`Realtime`/`Gateway` capability names), defaulting
/// to `combined` — milestone 1's "one `avalon-server` process" reality, per
/// that doc's own capability table.
fn node_roles() -> Vec<String> {
    std::env::var("AVALON_NODE_ROLES")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|roles| !roles.is_empty())
        .unwrap_or_else(|| vec!["combined".to_string()])
}

/// Spawned unconditionally at startup (see `main.rs`) — unlike the
/// mirror-watcher, this always runs: even a network's anchor node (with an
/// empty resolved peer list) still needs to serve `announce`/`list_peers`
/// requests from everyone else, and an operator can always add
/// `AVALON_BOOTSTRAP_PEERS` later without a restart-time config check
/// gating whether this task exists at all. Never returns.
pub async fn run_worker(
    chain: PostgresSettlementProvider,
    peers: PeerTable,
    config: AnnounceConfig,
    dht_identity: Option<DhtIdentity>,
) {
    if config.peers.is_empty() {
        tracing::info!(
            "node-announce: no bootstrap/seed peers configured — this node can still be \
             announced to via POST /nodes/announce, but won't announce itself anywhere"
        );
    } else if config.own_base_url.is_none() {
        tracing::warn!(
            "node-announce: {} peer(s) configured but AVALON_NODE_URL is unset — cannot \
             announce without this node's own reachable base URL",
            config.peers.len()
        );
    } else {
        tracing::info!(
            "node-announce: announcing to {} peer(s) every {:?}: {}",
            config.peers.len(),
            config.interval,
            config.peers.join(", ")
        );
    }

    let client = reqwest::Client::new();
    let roles = node_roles();
    let network_id = chain.network_id().to_string();

    loop {
        if let Some(own_base_url) = &config.own_base_url {
            for peer in &config.peers {
                match announce_to(
                    &client,
                    peer,
                    own_base_url,
                    &roles,
                    &network_id,
                    dht_identity.as_ref(),
                )
                .await
                {
                    Ok(discovered) => {
                        for info in discovered {
                            if info.network_id == network_id {
                                peers.admit_if_supported(info);
                            }
                        }
                    }
                    Err(err) => tracing::error!("node-announce: {peer}: {err}"),
                }
            }
        }

        let cutoff = OffsetDateTime::now_utc() - config.interval * PRUNE_INTERVAL_MULTIPLE;
        peers.prune_older_than(cutoff);

        tokio::time::sleep(config.interval).await;
    }
}

async fn announce_to(
    client: &reqwest::Client,
    peer_base_url: &str,
    own_base_url: &str,
    roles: &[String],
    network_id: &str,
    dht_identity: Option<&DhtIdentity>,
) -> Result<Vec<PeerInfo>, String> {
    let response = client
        .post(format!("{peer_base_url}/nodes/announce"))
        .json(&AnnounceRequest {
            base_url: own_base_url.to_string(),
            roles: roles.to_vec(),
            protocol_version: crate::version::PROTOCOL_VERSION.to_string(),
            network_id: network_id.to_string(),
            libp2p_peer_id: dht_identity.map(|d| d.peer_id.clone()),
            libp2p_listen_addrs: dht_identity
                .map(|d| d.listen_addrs.clone())
                .unwrap_or_default(),
        })
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }

    let body: AnnounceResponse = response.json().await.map_err(|e| e.to_string())?;
    Ok(body.peers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(base_url: &str, announced_at: OffsetDateTime) -> PeerInfo {
        PeerInfo {
            base_url: base_url.to_string(),
            roles: vec!["combined".to_string()],
            protocol_version: "0.1.0".to_string(),
            network_id: "avalon-dev-local".to_string(),
            last_announced_at: announced_at,
            libp2p_peer_id: None,
            libp2p_listen_addrs: Vec::new(),
        }
    }

    #[test]
    fn upsert_then_list_excluding_omits_the_named_peer() {
        let table = PeerTable::new();
        table.upsert(peer("http://a", OffsetDateTime::now_utc()));
        table.upsert(peer("http://b", OffsetDateTime::now_utc()));

        let seen_by_a = table.list_excluding("http://a");
        assert_eq!(seen_by_a.len(), 1);
        assert_eq!(seen_by_a[0].base_url, "http://b");
    }

    #[test]
    fn upsert_twice_for_the_same_peer_refreshes_not_duplicates() {
        let table = PeerTable::new();
        table.upsert(peer("http://a", OffsetDateTime::now_utc()));
        table.upsert(peer(
            "http://a",
            OffsetDateTime::now_utc() + time::Duration::seconds(1),
        ));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn prune_removes_only_stale_entries() {
        let table = PeerTable::new();
        let now = OffsetDateTime::now_utc();
        table.upsert(peer("http://fresh", now));
        table.upsert(peer("http://stale", now - time::Duration::minutes(10)));

        table.prune_older_than(now - time::Duration::minutes(1));

        let remaining = table.list_all();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].base_url, "http://fresh");
    }

    fn peer_with_version(base_url: &str, protocol_version: &str) -> PeerInfo {
        PeerInfo {
            base_url: base_url.to_string(),
            roles: vec!["combined".to_string()],
            protocol_version: protocol_version.to_string(),
            network_id: "avalon-dev-local".to_string(),
            last_announced_at: OffsetDateTime::now_utc(),
            libp2p_peer_id: None,
            libp2p_listen_addrs: Vec::new(),
        }
    }

    #[test]
    fn a_peer_below_the_effective_floor_is_excluded_from_the_peer_table() {
        let table = PeerTable::new();
        let admitted = table.admit_if_supported(peer_with_version("http://old-peer", "0.0.1"));
        assert!(!admitted);
        assert!(table.list_all().is_empty());
    }

    #[test]
    fn a_peer_at_or_above_the_effective_floor_is_included() {
        let table = PeerTable::new();
        let admitted = table.admit_if_supported(peer_with_version(
            "http://current-peer",
            crate::version::PROTOCOL_VERSION,
        ));
        assert!(admitted);
        assert_eq!(table.list_all().len(), 1);
    }

    #[test]
    fn a_peer_that_later_reports_an_upgraded_version_is_re_admitted() {
        let table = PeerTable::new();
        assert!(!table.admit_if_supported(peer_with_version("http://upgrading-peer", "0.0.1")));
        assert!(table.list_all().is_empty());

        assert!(table.admit_if_supported(peer_with_version(
            "http://upgrading-peer",
            crate::version::PROTOCOL_VERSION,
        )));
        let remaining = table.list_all();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].base_url, "http://upgrading-peer");
    }

    fn anchor(network_id: &str, seed_nodes: Vec<String>) -> avalon_sdk::network::TrustAnchorEntry {
        avalon_sdk::network::TrustAnchorEntry {
            label: network_id.to_string(),
            network_id: network_id.to_string(),
            verify_key: "ab".repeat(32),
            signing_key_id: "test-key".to_string(),
            server_url: None,
            environment: avalon_sdk::network::NetworkEnvironment::LocalDev,
            seed_nodes,
            notes: None,
        }
    }

    #[test]
    fn explicit_bootstrap_env_wins_over_any_seed_list() {
        let anchors = vec![anchor("avalon-dev-local", vec!["http://seed".to_string()])];
        let peers = resolve_bootstrap_peers(
            Some("http://explicit-a, http://explicit-b/"),
            "avalon-dev-local",
            &anchors,
        );
        assert_eq!(peers, vec!["http://explicit-a", "http://explicit-b"]);
    }

    #[test]
    fn unset_bootstrap_env_falls_back_to_this_networks_seed_list() {
        let anchors = vec![
            anchor("avalon-dev-local", vec!["http://seed-a".to_string()]),
            anchor("some-other-network", vec!["http://irrelevant".to_string()]),
        ];
        let peers = resolve_bootstrap_peers(None, "avalon-dev-local", &anchors);
        assert_eq!(peers, vec!["http://seed-a"]);
    }

    #[test]
    fn blank_bootstrap_env_also_falls_back_to_the_seed_list() {
        let anchors = vec![anchor("avalon-dev-local", vec!["http://seed".to_string()])];
        let peers = resolve_bootstrap_peers(Some("   "), "avalon-dev-local", &anchors);
        assert_eq!(peers, vec!["http://seed"]);
    }

    #[test]
    fn an_anchor_node_with_no_seed_list_resolves_to_an_empty_peer_list() {
        let anchors = vec![anchor("avalon-dev-local", Vec::new())];
        let peers = resolve_bootstrap_peers(None, "avalon-dev-local", &anchors);
        assert!(peers.is_empty());
    }

    #[test]
    fn an_unknown_network_id_resolves_to_an_empty_peer_list_not_an_error() {
        let anchors = vec![anchor(
            "some-other-network",
            vec!["http://seed".to_string()],
        )];
        let peers = resolve_bootstrap_peers(None, "avalon-dev-local", &anchors);
        assert!(peers.is_empty());
    }

    #[test]
    fn node_roles_defaults_to_combined_when_unset() {
        // SAFETY-of-intent note: `std::env::remove_var`/`set_var` are
        // process-global; no other test in this crate touches
        // `AVALON_NODE_ROLES`, matching the posture
        // `crates/indexer/src/registry.rs`'s own env-var test already takes.
        unsafe {
            std::env::remove_var("AVALON_NODE_ROLES");
        }
        assert_eq!(node_roles(), vec!["combined".to_string()]);

        unsafe {
            std::env::set_var("AVALON_NODE_ROLES", "settlement,indexer");
        }
        assert_eq!(
            node_roles(),
            vec!["settlement".to_string(), "indexer".to_string()]
        );

        unsafe {
            std::env::remove_var("AVALON_NODE_ROLES");
        }
    }

    /// Issue #517: `NodeStatusResponse` must still be valid JSON when every
    /// resource metric is unavailable (stubbed here as an unrefreshed
    /// sampler, the same shape a platform this crate can't read any
    /// `sysinfo` stat on would produce) — the endpoint itself never fails
    /// just because a host metric couldn't be read.
    #[test]
    fn node_status_response_serializes_with_stubbed_resource_metrics() {
        let response = NodeStatusResponse {
            protocol_version: crate::version::PROTOCOL_VERSION.to_string(),
            network_id: "avalon-dev-local".to_string(),
            stale: false,
            newest_known_peer_version: None,
            resources: crate::resources::NodeResourceMetrics::default(),
        };
        let json = serde_json::to_value(&response).expect("must serialize even when empty");
        assert!(json.get("resources").is_some());
        assert_eq!(
            json["resources"]["cpu"]["usage_percent"],
            serde_json::Value::Null
        );
        assert_eq!(
            json["resources"]["db_pool"]["size"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn peer_info_deserializes_without_libp2p_fields_for_backward_compat() {
        // Issue #582: a peer running a pre-#582 binary never sends these
        // fields at all — `#[serde(default)]` must make that a `None`/
        // empty deserialize, not a hard failure.
        let json = r#"{
            "base_url": "http://old-peer",
            "roles": ["combined"],
            "protocol_version": "0.1.0",
            "network_id": "avalon-dev-local",
            "last_announced_at": "2026-01-01T00:00:00Z"
        }"#;
        let info: PeerInfo =
            serde_json::from_str(json).expect("should deserialize without the new fields");
        assert_eq!(info.libp2p_peer_id, None);
        assert!(info.libp2p_listen_addrs.is_empty());
    }

    #[test]
    fn announce_request_deserializes_without_libp2p_fields_for_backward_compat() {
        let json = r#"{
            "base_url": "http://old-peer",
            "roles": ["combined"],
            "protocol_version": "0.1.0",
            "network_id": "avalon-dev-local"
        }"#;
        let req: AnnounceRequest =
            serde_json::from_str(json).expect("should deserialize without the new fields");
        assert_eq!(req.libp2p_peer_id, None);
        assert!(req.libp2p_listen_addrs.is_empty());
    }
}
