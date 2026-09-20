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
//!
//! **Issue #599, Layer 1: the active announce/exchange set grows past the
//! bootstrap list.** Before this, [`run_worker`] only ever re-announced to
//! `AnnounceConfig::peers` (the originally-configured bootstrap/seed list)
//! on every tick — a peer discovered through one of those bootstrap peers
//! was folded into the local [`PeerTable`] (so it showed up in
//! `GET /nodes/peers`) but was never itself promoted to an ongoing announce
//! target, so propagation stopped one hop past the bootstrap set. Now
//! `run_worker` keeps its own growing `active_peers` list, seeded from the
//! bootstrap set (which is never evicted — it's still how this node first
//! reaches the mesh at all) and extended, capped by
//! `AVALON_NODE_MAX_PEERS`, with peers discovered via announce responses —
//! the same bounded-fan-out/full-eventual-reach property Kademlia's
//! k-bucket maintenance and gossip-membership protocols (SWIM, HyParView)
//! rely on. A peer that later drops out of the peer table (pruned for not
//! re-announcing) is dropped from `active_peers` too on the next tick,
//! making room for others rather than permanently pinning a dead slot.
//!
//! **Issue #599, Layer 2: shard-existence gossip rides on the same
//! mechanism**, the same way DHT identity already rides along announce
//! (#582). [`ShardAnnouncement`]/[`ShardRegistry`] are this node's
//! anti-entropy view of "every shard I currently know exists, and a URL
//! that claims to serve it" — gossiped bidirectionally on every announce
//! exchange (both the request and the response now carry a
//! `known_shards` snapshot), not looked up via a single fixed key. See
//! [`ShardRegistry`]'s own doc comment for the merge/decay policy, and
//! `crate::cross_shard`/`crate::mirror_watcher` for how a discovered shard
//! is consumed once learned — discovering it never implies trusting it;
//! #543's key-resolution/verification step is unconditional and unchanged.

use std::collections::{BTreeSet, HashMap, HashSet};
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

/// One shard this node currently believes exists, and a URL that claims to
/// be able to serve it (`GET /ledger/sth/latest?shard_id=...`, the same
/// read `crate::cross_shard`/`crate::mirror_watcher` already use). Not
/// itself a trust claim — see [`ShardRegistry`]'s own doc comment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardAnnouncement {
    pub shard_id: String,
    pub url: String,
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen_at: OffsetDateTime,
}

/// This node's anti-entropy view of "every shard I currently know exists,
/// and a URL claiming to serve it" — issue #599, Layer 2. Gossiped
/// bidirectionally on every announce exchange (see [`AnnounceRequest`]/
/// [`AnnounceResponse`]'s own `known_shards` fields): a node merges what it
/// learns from a peer, and reports back everything it itself knows,
/// exactly the epidemic/anti-entropy shape membership-gossip protocols use
/// to reconcile "here's what I know" between neighbors.
///
/// Unlike [`PeerTable`], there is deliberately no cap here — a node's
/// direct-connection *count* is bounded (see `AnnounceConfig::max_peers`),
/// but the *set of shards that exist* is an enumeration problem, and the
/// whole point of this ticket is that every node eventually learns the
/// full set, not a bounded sample of it.
///
/// More than one URL can be on record for the same `shard_id` at once
/// (e.g. during a URL migration, or a stale/incorrect claim from a
/// misbehaving peer) — this registry does not attempt to pick a single
/// "correct" one; every consumer ([`crate::cross_shard`],
/// [`crate::mirror_watcher`]) independently verifies whatever URL it tries
/// against #543's real trust mechanism before using it for anything, so an
/// unverifiable or stale claim is simply never acted on, never merged away
/// silently.
#[derive(Clone, Default)]
pub struct ShardRegistry {
    // shard_id -> (url -> last_seen_at)
    shards: Arc<RwLock<HashMap<String, HashMap<String, OffsetDateTime>>>>,
    /// Issue #629: the earliest `last_seen_at` this node has ever recorded
    /// for a given `shard_id`, across every merge (including this node's
    /// own `record_own` calls) — "how old is this shard," used as the
    /// #629 registration-eligibility grace period's input
    /// (`crate::replication::within_grace_period`). Deliberately tracked
    /// separately from `shards` above, which only ever keeps the *newest*
    /// `last_seen_at` per URL (it answers "is this still around," not
    /// "how long has it been around") — and deliberately never advanced
    /// forward once set, only ever backward if a later merge reveals an
    /// even earlier observation (e.g. gossip from a peer that had already
    /// known about this shard for longer than this node had). Same
    /// in-process, non-durable posture as `shards` — see this struct's own
    /// doc comment and `crate::replication`'s module doc comment for the
    /// restart caveat that follows from that.
    first_seen: Arc<RwLock<HashMap<String, OffsetDateTime>>>,
}

impl ShardRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records this node's own authoritative claim (see
    /// `run_worker`'s call site for what "authoritative" means here) —
    /// same upsert-by-key shape [`PeerTable::upsert`] uses, refreshing
    /// `last_seen_at` so this node's own entry never decays out of its own
    /// gossip snapshot while it keeps running.
    pub fn record_own(&self, shard_id: &str, url: &str, now: OffsetDateTime) {
        self.merge(&[ShardAnnouncement {
            shard_id: shard_id.to_string(),
            url: url.to_string(),
            last_seen_at: now,
        }]);
    }

    /// Merges `incoming` (another node's gossip snapshot, or a freshly
    /// self-recorded entry) into this registry, keeping the newest
    /// `last_seen_at` on record for each `(shard_id, url)` pair. Returns
    /// the `shard_id`s that were genuinely new to this node (not
    /// previously known at all, under any URL) — purely for logging at the
    /// call site, never used to gate anything.
    pub fn merge(&self, incoming: &[ShardAnnouncement]) -> Vec<String> {
        let mut newly_learned = Vec::new();
        let mut shards = self.shards.write().expect("shard registry lock poisoned");
        let mut first_seen = self
            .first_seen
            .write()
            .expect("shard registry lock poisoned");
        for entry in incoming {
            let urls = shards.entry(entry.shard_id.clone()).or_insert_with(|| {
                newly_learned.push(entry.shard_id.clone());
                HashMap::new()
            });
            let refresh = urls
                .get(&entry.url)
                .is_none_or(|existing| entry.last_seen_at > *existing);
            if refresh {
                urls.insert(entry.url.clone(), entry.last_seen_at);
            }

            // Issue #629: keep the *earliest* observation on record, never
            // the latest — see `first_seen`'s own doc comment.
            first_seen
                .entry(entry.shard_id.clone())
                .and_modify(|existing| {
                    if entry.last_seen_at < *existing {
                        *existing = entry.last_seen_at;
                    }
                })
                .or_insert(entry.last_seen_at);
        }
        newly_learned
    }

    /// Drops any `(shard_id, url)` entry not refreshed since `cutoff` —
    /// same decay-not-forever posture [`PeerTable::prune_older_than`]
    /// already takes, so a shard operator that genuinely goes away (or
    /// rotates URLs) eventually falls out rather than being remembered
    /// forever on a stale address.
    pub fn prune_older_than(&self, cutoff: OffsetDateTime) {
        let mut shards = self.shards.write().expect("shard registry lock poisoned");
        let mut fully_removed = Vec::new();
        shards.retain(|shard_id, urls| {
            urls.retain(|_, last_seen_at| *last_seen_at >= cutoff);
            let keep = !urls.is_empty();
            if !keep {
                fully_removed.push(shard_id.clone());
            }
            keep
        });
        if !fully_removed.is_empty() {
            // Issue #629: a shard with no URLs left on record at all is
            // treated as genuinely gone — its recorded age goes with it,
            // so a later re-discovery starts its grace period fresh
            // rather than inheriting a stale, possibly very old
            // `first_seen_at`.
            let mut first_seen = self
                .first_seen
                .write()
                .expect("shard registry lock poisoned");
            for shard_id in fully_removed {
                first_seen.remove(&shard_id);
            }
        }
    }

    /// The earliest observation this node has ever recorded for
    /// `shard_id` — see `first_seen`'s own doc comment. `None` when this
    /// node has never merged an announcement naming this shard at all
    /// (including one it authored itself).
    pub fn first_seen_at(&self, shard_id: &str) -> Option<OffsetDateTime> {
        self.first_seen
            .read()
            .expect("shard registry lock poisoned")
            .get(shard_id)
            .copied()
    }

    /// Every `(shard_id, url, last_seen_at)` this node currently knows —
    /// what gets attached to an outbound announce request/response as this
    /// node's own gossip snapshot.
    pub fn snapshot(&self) -> Vec<ShardAnnouncement> {
        self.shards
            .read()
            .expect("shard registry lock poisoned")
            .iter()
            .flat_map(|(shard_id, urls)| {
                urls.iter()
                    .map(move |(url, last_seen_at)| ShardAnnouncement {
                        shard_id: shard_id.clone(),
                        url: url.clone(),
                        last_seen_at: *last_seen_at,
                    })
            })
            .collect()
    }

    /// Every `shard_id` currently on record, under any URL — what
    /// `crate::cross_shard` unions with its own static config.
    pub fn known_shard_ids(&self) -> BTreeSet<String> {
        self.shards
            .read()
            .expect("shard registry lock poisoned")
            .keys()
            .cloned()
            .collect()
    }

    /// The most-recently-seen URL on record for `shard_id`, if any — a
    /// reasonable single candidate for a caller that just needs one URL to
    /// try (still independently verified before being trusted for
    /// anything). A caller that wants every candidate uses [`Self::snapshot`]
    /// directly.
    pub fn best_url(&self, shard_id: &str) -> Option<String> {
        self.shards
            .read()
            .expect("shard registry lock poisoned")
            .get(shard_id)
            .and_then(|urls| urls.iter().max_by_key(|(_, ts)| **ts))
            .map(|(url, _)| url.clone())
    }

    #[cfg(test)]
    fn shard_count(&self) -> usize {
        self.shards
            .read()
            .expect("shard registry lock poisoned")
            .len()
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
    /// Issue #599, Layer 2: this node's own current [`ShardRegistry`]
    /// snapshot — gossiped to the callee on every announce, merged into
    /// its own registry the same way [`AnnounceResponse::known_shards`] is
    /// merged back into this node's. `#[serde(default)]` so an older
    /// peer's announce (pre-#599) still decodes, just with nothing to
    /// merge.
    #[serde(default)]
    pub known_shards: Vec<ShardAnnouncement>,
}

/// `Deserialize` too: this is also the shape `run_worker` parses back out
/// of a peer's response.
#[derive(Debug, Serialize, Deserialize)]
pub struct AnnounceResponse {
    pub peers: Vec<PeerInfo>,
    /// Issue #599, Layer 2 — see [`AnnounceRequest::known_shards`]'s own
    /// doc comment; this is the same exchange in the other direction.
    #[serde(default)]
    pub known_shards: Vec<ShardAnnouncement>,
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

    // Issue #599, Layer 2: anti-entropy shard-gossip merge, both
    // directions, regardless of protocol_version — same posture #368
    // already takes for the peer table itself: a shard claim is not a
    // security gate, it's discovery data a caller can't spoof its way
    // around trusting (#543's verification is unconditional downstream).
    let newly_learned = state.shard_registry.merge(&body.known_shards);
    for shard_id in &newly_learned {
        tracing::info!(
            event = "shard_discovered",
            shard_id = %shard_id,
            from_peer = %caller_base_url,
            "learned of a new shard via peer-announce gossip",
        );
    }

    Ok(Json(AnnounceResponse {
        peers: state.peers.list_excluding(&caller_base_url),
        known_shards: state.shard_registry.snapshot(),
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
    /// Issue #629: this node's own authored shard's current replication
    /// status — the visible fact the #629 ticket asked for, so an
    /// operator (and eventually an end user choosing where to register)
    /// can see how durable a shard actually is *before* trusting it with
    /// anything, not just after a registration attempt is rejected.
    pub own_shard_replication: ShardReplicationStatus,
}

/// Issue #629: how many distinct peers currently confirm mirroring a
/// shard, whether it's still within its bootstrap grace period, and
/// whether it's currently eligible to accept a *new* identity
/// registration under the #629 gate (`crate::replication::registration_eligible`,
/// enforced at `crate::handlers::register_start`). Scoped to this node's
/// own `own_shard_id` only — deliberate v1 simplification, same posture
/// `crate::replication`'s own module doc comment takes for its single
/// uniform `min_confirmations`: every other shard this node merely knows
/// *about* (via gossip) is a different operator's own durability
/// question, not this node's to surface authoritatively.
#[derive(Debug, Serialize)]
pub struct ShardReplicationStatus {
    pub shard_id: String,
    pub confirmed_mirror_count: usize,
    pub min_confirmations_required: usize,
    #[serde(with = "time::serde::rfc3339::option")]
    pub first_seen_at: Option<OffsetDateTime>,
    pub within_grace_period: bool,
    pub eligible_for_new_registrations: bool,
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

    let now = OffsetDateTime::now_utc();
    let first_seen_at = state.shard_registry.first_seen_at(&state.own_shard_id);
    let within_grace_period = crate::replication::within_grace_period(
        first_seen_at,
        now,
        state.replication_gate.grace_period,
    );
    let confirmed_mirror_count = state.mirror_confirmations.confirmed_count(
        &state.own_shard_id,
        now - crate::replication::CONFIRMATION_FRESHNESS_WINDOW,
    );
    let own_shard_replication = ShardReplicationStatus {
        shard_id: state.own_shard_id.clone(),
        confirmed_mirror_count,
        min_confirmations_required: state.replication_gate.min_confirmations,
        first_seen_at,
        within_grace_period,
        eligible_for_new_registrations: within_grace_period
            || confirmed_mirror_count >= state.replication_gate.min_confirmations,
    };

    Json(NodeStatusResponse {
        protocol_version: crate::version::PROTOCOL_VERSION.to_string(),
        network_id: state.chain.network_id().to_string(),
        stale,
        newest_known_peer_version: newest_known_peer_version.map(|v| v.to_string()),
        resources,
        own_shard_replication,
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
    /// `AVALON_NODE_MAX_PEERS` (issue #599, Layer 1) — the cap on this
    /// node's *active* announce/exchange set (bootstrap peers plus
    /// peers promoted from what's been discovered through them). Bounds
    /// this node's own direct-connection count regardless of how large the
    /// network as a whole grows, the same role Kademlia's k-bucket size or
    /// a gossip-membership protocol's fanout limit plays. Bootstrap peers
    /// are never evicted to make room — see [`run_worker`]'s own doc
    /// comment.
    pub max_peers: usize,
}

const DEFAULT_ANNOUNCE_INTERVAL_SECS: u64 = 180;
/// A peer not re-announced within this many multiples of the announce
/// interval is pruned — generous enough that one or two missed ticks
/// (a transient network blip) never evicts a genuinely live peer.
const PRUNE_INTERVAL_MULTIPLE: u32 = 3;
/// Default for `AVALON_NODE_MAX_PEERS` — generous enough that a real
/// small-to-mid-size deployment never bumps into it in practice, but still
/// a real, enforced bound rather than "unlimited" (issue #599's own stated
/// invariant: a node's direct-connection count must stay bounded
/// regardless of network size).
const DEFAULT_MAX_PEERS: usize = 50;

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

        let max_peers = std::env::var("AVALON_NODE_MAX_PEERS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_MAX_PEERS);

        Self {
            peers,
            interval,
            own_base_url,
            max_peers,
        }
    }
}

/// `AVALON_NODE_ROLES` — comma-separated (matching `docs/architecture/nodes.md`'s
/// `Settlement`/`Indexer`/`Realtime`/`Gateway` capability names), defaulting
/// to `combined` — milestone 1's "one `avalon-server` process" reality, per
/// that doc's own capability table.
///
/// This used to be purely advisory (peer-table bookkeeping and #539's
/// realtime relay routing only) — `main.rs` now also reads it to decide
/// what actually gets wired up at startup: whether to run local presence/
/// WebSocket handling at all (issue #663, see [`role_included`]/
/// [`realtime_mode_from_env`]), whether to construct a local
/// `PostgresIndexer` or a `RemoteIndexer` (issue #662, see
/// [`indexer_role_is_local`]), and whether this process is a genuinely
/// standalone Settlement node (issue #664, see [`is_settlement_only`]).
/// `pub` for all three reasons, and so `main.rs` doesn't reimplement the
/// same env-var parsing.
pub fn node_roles() -> Vec<String> {
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

/// Whether `roles` (as returned by [`node_roles`]) includes `role` —
/// case-insensitively, and treating `combined` as implying every named
/// capability (milestone 1's default: one process, every role). Pure and
/// unit-testable independent of the environment, same "pure function
/// behind the real env-reading one" split this module already establishes
/// for `resolve_bootstrap_peers`/`promote_discovered_peers`. This is the
/// same membership rule `crate::realtime_relay::advertises_realtime_relay_role`
/// encodes for its own narrower `realtime`/`gateway`/`combined` set — kept
/// as a separate function rather than reused directly, since that one is
/// specifically "eligible relay target", not "this role is configured",
/// and the two questions are allowed to diverge (a node can be a valid
/// relay target for `gateway` without ever being asked whether it holds
/// the `realtime` role itself).
pub fn role_included(roles: &[String], role: &str) -> bool {
    roles
        .iter()
        .any(|r| r.eq_ignore_ascii_case("combined") || r.eq_ignore_ascii_case(role))
}

/// Issue #663: resolves this process's own `AppState::realtime_remote_url`
/// from `AVALON_NODE_ROLES`/`AVALON_REALTIME_URL`, called once by `main.rs`
/// at startup. `Ok(None)` means this process holds the `realtime` role
/// itself (the default — `role_included`'s `combined` fallback included)
/// and serves `/ws/presence`/`/ws/messages` locally, exactly as before
/// this issue. `Ok(Some(url))` means it does not, and every WebSocket
/// connection is proxied through to `url` instead (already validated as a
/// well-formed URL here, trailing slash stripped, so
/// `crate::realtime_proxy`'s own URL-building can treat a parse failure
/// there as unreachable in practice — see that module's own doc comment).
///
/// `Err` — never a silent fallback to serving sockets locally anyway —
/// when `realtime` is excluded but `AVALON_REALTIME_URL` is unset, blank,
/// or not a well-formed URL. Same "refusing to start" precedent
/// `PostgresSettlementProvider::connect`/`retention::RetentionConfig::from_env`
/// already establish for `main.rs`'s other startup-time checks.
pub fn realtime_mode_from_env(roles: &[String]) -> Result<Option<String>, String> {
    if role_included(roles, "realtime") {
        return Ok(None);
    }
    let raw = std::env::var("AVALON_REALTIME_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            "AVALON_NODE_ROLES excludes \"realtime\" but AVALON_REALTIME_URL is unset — this \
             node has no local Realtime role and no remote one configured to proxy WebSocket \
             connections to"
                .to_string()
        })?;
    let trimmed = raw.trim().trim_end_matches('/').to_string();
    url::Url::parse(&trimmed)
        .map_err(|e| format!("AVALON_REALTIME_URL is not a valid URL: {e}"))?;
    Ok(Some(trimmed))
}

/// Issue #662: does this process's configured `AVALON_NODE_ROLES` include
/// the Indexer role — i.e. should it run its own local `PostgresIndexer`,
/// or route indexer reads/writes to a remote one instead?
///
/// Pure/unit-testable, same "pure function behind the real config read"
/// pattern [`promote_discovered_peers`]/[`retain_reachable_active_peers`]
/// already establish in this module — takes the already-parsed roles list
/// rather than reading the env var itself, so it's trivially testable
/// without touching process-global state.
///
/// `combined` means "every role" (the table in `docs/architecture/nodes.md`'s
/// "Capabilities" section — Settlement/Indexer/Realtime/Gateway — describes
/// `combined` as running all four, not as a fifth, distinct role name), so
/// it counts as including `indexer` here exactly as it already implicitly
/// has for every other role check this codebase makes.
pub fn indexer_role_is_local(roles: &[String]) -> bool {
    roles.iter().any(|r| r == "combined" || r == "indexer")
}

/// Issue #664: true only when `roles` is *exactly* `["settlement"]` — a
/// genuinely standalone Settlement node, not `combined` (which still runs
/// everything, including Settlement) and not some other future combination
/// #662/#663 may introduce (`["settlement", "indexer"]`, say) that this
/// ticket deliberately doesn't try to have an opinion on. This is the one
/// predicate `main.rs`/`lib.rs::router_settlement_only` gate on: every other
/// roles configuration gets today's full router and worker set, unchanged.
///
/// The mirror case — Gateway pointed at a *remote* Settlement authority —
/// doesn't need a new predicate here at all: #313's `AVALON_SETTLEMENT_REMOTE_URL(S)`
/// already exists and is checked independently by `crate::outbox`, so a
/// node can already run `combined` (or any roles list) while forwarding its
/// own outbox writes elsewhere. See `docs/architecture/nodes.md`'s "Today in
/// the repo" section for why these are one config surface, not two.
pub fn is_settlement_only(roles: &[String]) -> bool {
    roles.len() == 1 && roles[0] == "settlement"
}

/// Bounded promotion of newly-discovered peers into the active
/// announce/exchange set — issue #599, Layer 1's actual fix, split out
/// pure/unit-testable from [`run_worker`]'s async plumbing (same "pure
/// function behind the real worker" pattern `crate::dht::new_dht_peer`
/// already establishes). `active_peers` is mutated in place (preserving
/// insertion order — bootstrap peers first, since they were seeded into it
/// before this is ever called); a discovered peer already present, or
/// naming this node's own `own_base_url`, is skipped; anything beyond
/// `max_peers` is left undiscovered for now rather than promoted — it's
/// still in the passive [`PeerTable`] (via the caller's own
/// `admit_if_supported` call), just not an active announce target yet.
/// Returns whichever base URLs were newly promoted this call, purely for
/// logging at the call site.
fn promote_discovered_peers(
    active_peers: &mut Vec<String>,
    discovered: &[PeerInfo],
    own_base_url: &str,
    max_peers: usize,
) -> Vec<String> {
    let mut promoted = Vec::new();
    for info in discovered {
        if info.base_url == own_base_url {
            continue;
        }
        if active_peers.len() >= max_peers {
            break;
        }
        if active_peers.iter().any(|p| p == &info.base_url) {
            continue;
        }
        active_peers.push(info.base_url.clone());
        promoted.push(info.base_url.clone());
    }
    promoted
}

/// Drops any `active_peers` entry that's no longer known to `peers` at
/// all — i.e. it was pruned from the [`PeerTable`] this tick for not
/// re-announcing — *unless* it's one of `bootstrap_peers`, which are never
/// evicted (they're still how this node reaches the mesh at all on a cold
/// start, even if temporarily unreachable). Freeing a dead slot here is
/// what lets [`promote_discovered_peers`] keep making room for genuinely
/// live peers over time rather than permanently pinning a stale one.
fn retain_reachable_active_peers(
    active_peers: &mut Vec<String>,
    bootstrap_peers: &[String],
    known_base_urls: &HashSet<String>,
) {
    active_peers.retain(|p| bootstrap_peers.iter().any(|b| b == p) || known_base_urls.contains(p));
}

/// Spawned unconditionally at startup (see `main.rs`) — unlike the
/// mirror-watcher, this always runs: even a network's anchor node (with an
/// empty resolved peer list) still needs to serve `announce`/`list_peers`
/// requests from everyone else, and an operator can always add
/// `AVALON_BOOTSTRAP_PEERS` later without a restart-time config check
/// gating whether this task exists at all. Never returns.
///
/// Issue #599, Layer 1: unlike before, the set of peers actually announced
/// to each tick (`active_peers`) is not simply `config.peers` — it starts
/// there, but grows (capped at `config.max_peers`) as peers are discovered
/// through those bootstrap peers' own announce responses, via
/// [`promote_discovered_peers`]. A peer that stops re-announcing is pruned
/// from the passive [`PeerTable`] as before, and (via
/// [`retain_reachable_active_peers`]) from `active_peers` too, unless it's
/// a bootstrap peer — freeing room for the network's fan-out to keep
/// growing this node's active set even as individual peers within it come
/// and go.
///
/// Issue #599, Layer 2: `shard_registry` is gossiped bidirectionally on
/// every announce exchange, over the exact same `active_peers` set — a
/// shard's existence propagates over strictly more edges, over enough
/// ticks, than this node's own bounded direct-connection count, which is
/// the entire point (see this module's own doc comment).
pub async fn run_worker(
    chain: PostgresSettlementProvider,
    peers: PeerTable,
    shard_registry: ShardRegistry,
    own_shard_id: String,
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
            "node-announce: announcing to {} bootstrap peer(s) every {:?} (max active peer \
             set size {}): {}",
            config.peers.len(),
            config.interval,
            config.max_peers,
            config.peers.join(", ")
        );
    }

    let client = reqwest::Client::new();
    let roles = node_roles();
    let network_id = chain.network_id().to_string();
    let mut active_peers: Vec<String> = config.peers.clone();

    loop {
        // Issue #599, Layer 2: before announcing, refresh this node's own
        // authoritative claim (if it has one) so it's part of the
        // snapshot gossiped out this tick. "Authoritative" here means this
        // node actually has local, signed settlement history for
        // `own_shard_id` — a pure mirror with no local writes of its own
        // has nothing to claim authority over and gossips only what it's
        // learned from others.
        if let Some(own_base_url) = &config.own_base_url {
            if let Ok(Some(_)) = chain.latest_signed_tree_head().await {
                shard_registry.record_own(&own_shard_id, own_base_url, OffsetDateTime::now_utc());
            }
        }

        if let Some(own_base_url) = &config.own_base_url {
            let targets = active_peers.clone();
            for peer in &targets {
                match announce_to(
                    &client,
                    peer,
                    own_base_url,
                    &roles,
                    &network_id,
                    dht_identity.as_ref(),
                    &shard_registry.snapshot(),
                )
                .await
                {
                    Ok(discovered) => {
                        let mut admitted = Vec::new();
                        for info in discovered.peers {
                            if info.network_id == network_id
                                && peers.admit_if_supported(info.clone())
                            {
                                admitted.push(info);
                            }
                        }
                        let promoted = promote_discovered_peers(
                            &mut active_peers,
                            &admitted,
                            own_base_url,
                            config.max_peers,
                        );
                        for base_url in &promoted {
                            tracing::info!(
                                event = "peer_promoted_to_active_set",
                                peer = %base_url,
                                via = %peer,
                                active_peer_count = active_peers.len(),
                                "promoted a peer discovered via gossip into the active \
                                 announce/exchange set",
                            );
                        }

                        let newly_learned = shard_registry.merge(&discovered.known_shards);
                        for shard_id in &newly_learned {
                            tracing::info!(
                                event = "shard_discovered",
                                shard_id = %shard_id,
                                from_peer = %peer,
                                "learned of a new shard via peer-announce gossip",
                            );
                        }
                    }
                    Err(err) => tracing::error!("node-announce: {peer}: {err}"),
                }
            }
        }

        let cutoff = OffsetDateTime::now_utc() - config.interval * PRUNE_INTERVAL_MULTIPLE;
        peers.prune_older_than(cutoff);
        shard_registry.prune_older_than(cutoff);

        let known_base_urls: HashSet<String> =
            peers.list_all().into_iter().map(|p| p.base_url).collect();
        retain_reachable_active_peers(&mut active_peers, &config.peers, &known_base_urls);

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
    known_shards: &[ShardAnnouncement],
) -> Result<AnnounceResponse, String> {
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
            known_shards: known_shards.to_vec(),
        })
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }

    response
        .json::<AnnounceResponse>()
        .await
        .map_err(|e| e.to_string())
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

    #[test]
    fn role_included_matches_combined_regardless_of_which_role_is_asked_about() {
        let roles = vec!["combined".to_string()];
        assert!(role_included(&roles, "realtime"));
        assert!(role_included(&roles, "settlement"));
        assert!(role_included(&roles, "anything"));
    }

    #[test]
    fn role_included_matches_an_explicit_role_case_insensitively() {
        let roles = vec!["Realtime".to_string(), "Gateway".to_string()];
        assert!(role_included(&roles, "realtime"));
        assert!(role_included(&roles, "REALTIME"));
        assert!(role_included(&roles, "gateway"));
    }

    #[test]
    fn role_included_is_false_for_an_explicit_list_missing_the_role() {
        let roles = vec!["gateway".to_string(), "indexer".to_string()];
        assert!(!role_included(&roles, "realtime"));
    }

    #[test]
    fn role_included_is_false_for_an_empty_role_list() {
        assert!(!role_included(&[], "realtime"));
    }

    #[test]
    fn realtime_mode_is_local_when_the_role_list_includes_realtime() {
        assert_eq!(realtime_mode_from_env(&["realtime".to_string()]), Ok(None));
        assert_eq!(realtime_mode_from_env(&["combined".to_string()]), Ok(None));
        assert_eq!(
            realtime_mode_from_env(&["gateway".to_string(), "realtime".to_string()]),
            Ok(None)
        );
    }

    /// One test, not three — `AVALON_REALTIME_URL` mutation is
    /// process-global, same "no other test in this crate touches this env
    /// var" posture `node_roles_defaults_to_combined_when_unset` already
    /// takes for `AVALON_NODE_ROLES`; sequencing every case through one
    /// test function avoids a parallel-test race on the same var.
    #[test]
    fn realtime_mode_from_env_covers_the_remote_and_error_cases() {
        unsafe {
            std::env::remove_var("AVALON_REALTIME_URL");
        }
        assert!(
            realtime_mode_from_env(&["gateway".to_string()]).is_err(),
            "realtime excluded with no AVALON_REALTIME_URL must fail loudly"
        );

        unsafe {
            std::env::set_var("AVALON_REALTIME_URL", "not a url");
        }
        assert!(
            realtime_mode_from_env(&["gateway".to_string()]).is_err(),
            "a malformed AVALON_REALTIME_URL must fail loudly, not silently fall back to local"
        );

        unsafe {
            std::env::set_var("AVALON_REALTIME_URL", "http://127.0.0.1:9090/");
        }
        assert_eq!(
            realtime_mode_from_env(&["gateway".to_string()]),
            Ok(Some("http://127.0.0.1:9090".to_string())),
            "a trailing slash on AVALON_REALTIME_URL must not be preserved"
        );

        unsafe {
            std::env::remove_var("AVALON_REALTIME_URL");
        }
    }

    #[test]
    fn indexer_role_is_local_treats_combined_as_every_role() {
        assert!(indexer_role_is_local(&["combined".to_string()]));
    }

    #[test]
    fn indexer_role_is_local_true_when_indexer_explicitly_listed() {
        assert!(indexer_role_is_local(&[
            "settlement".to_string(),
            "indexer".to_string()
        ]));
    }

    #[test]
    fn indexer_role_is_local_false_when_indexer_excluded() {
        assert!(!indexer_role_is_local(&[
            "gateway".to_string(),
            "realtime".to_string()
        ]));
    }

    #[test]
    fn indexer_role_is_local_false_for_an_empty_list() {
        assert!(!indexer_role_is_local(&[]));
    }

    /// Issue #664: pure function, no env involved — `is_settlement_only`
    /// only ever gates on the resolved roles list, never reads
    /// `AVALON_NODE_ROLES` itself.
    #[test]
    fn is_settlement_only_requires_exactly_one_settlement_role() {
        assert!(is_settlement_only(&["settlement".to_string()]));
        assert!(!is_settlement_only(&["combined".to_string()]));
        assert!(!is_settlement_only(&[]));
        assert!(!is_settlement_only(&[
            "settlement".to_string(),
            "gateway".to_string()
        ]));
        assert!(!is_settlement_only(&[
            "settlement".to_string(),
            "indexer".to_string()
        ]));
        assert!(!is_settlement_only(&["gateway".to_string()]));
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
            own_shard_replication: ShardReplicationStatus {
                shard_id: "core".to_string(),
                confirmed_mirror_count: 0,
                min_confirmations_required: 1,
                first_seen_at: None,
                within_grace_period: true,
                eligible_for_new_registrations: true,
            },
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
        assert_eq!(json["own_shard_replication"]["shard_id"], "core");
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

    // Issue #599, Layer 1: `promote_discovered_peers` unit tests.

    fn discovered_peer(base_url: &str) -> PeerInfo {
        peer(base_url, OffsetDateTime::now_utc())
    }

    #[test]
    fn a_newly_discovered_peer_is_promoted_when_under_the_cap() {
        let mut active = vec!["http://bootstrap".to_string()];
        let discovered = vec![discovered_peer("http://new-peer")];
        let promoted = promote_discovered_peers(&mut active, &discovered, "http://self", 10);
        assert_eq!(promoted, vec!["http://new-peer".to_string()]);
        assert_eq!(
            active,
            vec![
                "http://bootstrap".to_string(),
                "http://new-peer".to_string()
            ]
        );
    }

    #[test]
    fn already_active_peers_are_never_promoted_twice() {
        let mut active = vec![
            "http://bootstrap".to_string(),
            "http://new-peer".to_string(),
        ];
        let discovered = vec![discovered_peer("http://new-peer")];
        let promoted = promote_discovered_peers(&mut active, &discovered, "http://self", 10);
        assert!(promoted.is_empty());
        assert_eq!(active.len(), 2);
    }

    #[test]
    fn a_peer_naming_this_nodes_own_base_url_is_never_promoted() {
        let mut active = vec!["http://bootstrap".to_string()];
        let discovered = vec![discovered_peer("http://self")];
        let promoted = promote_discovered_peers(&mut active, &discovered, "http://self", 10);
        assert!(promoted.is_empty());
        assert_eq!(active.len(), 1);
    }

    #[test]
    fn promotion_stops_once_the_cap_is_reached() {
        let mut active = vec![
            "http://bootstrap-a".to_string(),
            "http://bootstrap-b".to_string(),
        ];
        let discovered = vec![
            discovered_peer("http://new-1"),
            discovered_peer("http://new-2"),
            discovered_peer("http://new-3"),
        ];
        // Cap of 3: room for exactly one more beyond the two bootstrap
        // peers already active.
        let promoted = promote_discovered_peers(&mut active, &discovered, "http://self", 3);
        assert_eq!(promoted, vec!["http://new-1".to_string()]);
        assert_eq!(active.len(), 3);
    }

    #[test]
    fn a_full_active_set_promotes_nothing_regardless_of_bootstrap_membership() {
        let mut active = vec!["http://bootstrap".to_string()];
        let discovered = vec![discovered_peer("http://new-peer")];
        let promoted = promote_discovered_peers(&mut active, &discovered, "http://self", 1);
        assert!(promoted.is_empty());
        assert_eq!(active, vec!["http://bootstrap".to_string()]);
    }

    // Issue #599, Layer 1: `retain_reachable_active_peers` unit tests.

    #[test]
    fn a_pruned_non_bootstrap_peer_is_dropped_from_the_active_set() {
        let mut active = vec!["http://bootstrap".to_string(), "http://gone".to_string()];
        let bootstrap = vec!["http://bootstrap".to_string()];
        let known: HashSet<String> = ["http://bootstrap".to_string()].into_iter().collect();
        retain_reachable_active_peers(&mut active, &bootstrap, &known);
        assert_eq!(active, vec!["http://bootstrap".to_string()]);
    }

    #[test]
    fn a_bootstrap_peer_is_retained_even_if_temporarily_unreachable() {
        let mut active = vec!["http://bootstrap".to_string()];
        let bootstrap = vec!["http://bootstrap".to_string()];
        let known: HashSet<String> = HashSet::new();
        retain_reachable_active_peers(&mut active, &bootstrap, &known);
        assert_eq!(
            active,
            vec!["http://bootstrap".to_string()],
            "a bootstrap/seed peer must never be evicted just because it's momentarily out of \
             the peer table"
        );
    }

    // Issue #599, Layer 2: `ShardRegistry` unit tests.

    fn shard_announcement(shard_id: &str, url: &str, at: OffsetDateTime) -> ShardAnnouncement {
        ShardAnnouncement {
            shard_id: shard_id.to_string(),
            url: url.to_string(),
            last_seen_at: at,
        }
    }

    #[test]
    fn merging_a_new_shard_reports_it_as_newly_learned() {
        let registry = ShardRegistry::new();
        let newly_learned = registry.merge(&[shard_announcement(
            "game:ashen-realms",
            "http://shard-a",
            OffsetDateTime::now_utc(),
        )]);
        assert_eq!(newly_learned, vec!["game:ashen-realms".to_string()]);
        assert_eq!(registry.shard_count(), 1);
        assert!(registry.known_shard_ids().contains("game:ashen-realms"));
    }

    #[test]
    fn merging_an_already_known_shard_again_is_not_reported_as_newly_learned() {
        let registry = ShardRegistry::new();
        let now = OffsetDateTime::now_utc();
        registry.merge(&[shard_announcement(
            "game:ashen-realms",
            "http://shard-a",
            now,
        )]);
        let newly_learned = registry.merge(&[shard_announcement(
            "game:ashen-realms",
            "http://shard-a",
            now + time::Duration::seconds(1),
        )]);
        assert!(newly_learned.is_empty());
    }

    #[test]
    fn best_url_picks_the_most_recently_seen_url_for_a_shard() {
        let registry = ShardRegistry::new();
        let now = OffsetDateTime::now_utc();
        registry.merge(&[
            shard_announcement("game:ashen-realms", "http://old-url", now),
            shard_announcement(
                "game:ashen-realms",
                "http://new-url",
                now + time::Duration::minutes(5),
            ),
        ]);
        assert_eq!(
            registry.best_url("game:ashen-realms"),
            Some("http://new-url".to_string())
        );
    }

    #[test]
    fn prune_older_than_drops_stale_shard_entries_but_keeps_fresh_ones() {
        let registry = ShardRegistry::new();
        let now = OffsetDateTime::now_utc();
        registry.merge(&[
            shard_announcement(
                "game:stale-shard",
                "http://stale",
                now - time::Duration::hours(1),
            ),
            shard_announcement("game:fresh-shard", "http://fresh", now),
        ]);

        registry.prune_older_than(now - time::Duration::minutes(1));

        let known = registry.known_shard_ids();
        assert!(!known.contains("game:stale-shard"));
        assert!(known.contains("game:fresh-shard"));
    }

    // Issue #629: `first_seen_at` unit tests.

    #[test]
    fn first_seen_at_is_none_for_an_unknown_shard() {
        let registry = ShardRegistry::new();
        assert_eq!(registry.first_seen_at("game:never-seen"), None);
    }

    #[test]
    fn first_seen_at_records_the_first_observation() {
        let registry = ShardRegistry::new();
        let now = OffsetDateTime::now_utc();
        registry.merge(&[shard_announcement("core", "http://a", now)]);
        assert_eq!(registry.first_seen_at("core"), Some(now));
    }

    #[test]
    fn first_seen_at_never_advances_on_a_later_observation() {
        let registry = ShardRegistry::new();
        let now = OffsetDateTime::now_utc();
        registry.merge(&[shard_announcement("core", "http://a", now)]);
        registry.merge(&[shard_announcement(
            "core",
            "http://a",
            now + time::Duration::hours(1),
        )]);
        assert_eq!(
            registry.first_seen_at("core"),
            Some(now),
            "a later merge must never make a shard look younger"
        );
    }

    #[test]
    fn first_seen_at_moves_earlier_if_an_earlier_observation_is_learned() {
        let registry = ShardRegistry::new();
        let now = OffsetDateTime::now_utc();
        registry.merge(&[shard_announcement("core", "http://a", now)]);
        registry.merge(&[shard_announcement(
            "core",
            "http://b",
            now - time::Duration::hours(1),
        )]);
        assert_eq!(
            registry.first_seen_at("core"),
            Some(now - time::Duration::hours(1))
        );
    }

    #[test]
    fn pruning_every_url_for_a_shard_also_clears_its_recorded_age() {
        let registry = ShardRegistry::new();
        let now = OffsetDateTime::now_utc();
        registry.merge(&[shard_announcement(
            "game:stale-shard",
            "http://stale",
            now - time::Duration::hours(1),
        )]);

        registry.prune_older_than(now - time::Duration::minutes(1));

        assert_eq!(registry.first_seen_at("game:stale-shard"), None);
    }

    #[test]
    fn record_own_refreshes_this_nodes_own_authoritative_entry() {
        let registry = ShardRegistry::new();
        let now = OffsetDateTime::now_utc();
        registry.record_own("core", "http://self", now);
        assert_eq!(registry.best_url("core"), Some("http://self".to_string()));

        registry.record_own("core", "http://self", now + time::Duration::minutes(1));
        assert_eq!(
            registry.shard_count(),
            1,
            "re-recording must refresh, never duplicate"
        );
    }
}
