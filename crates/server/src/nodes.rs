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
//! **Layer 2: shard-existence gossip rides on the same
//! mechanism**, the same way DHT identity already rides along announce.
//! [`ShardAnnouncement`]/[`ShardRegistry`] are this node's
//! anti-entropy view of "every shard I currently know exists, and a URL
//! that claims to serve it" — gossiped bidirectionally on every announce
//! exchange (both the request and the response now carry a
//! `known_shards` snapshot), not looked up via a single fixed key. See
//! [`ShardRegistry`]'s own doc comment for the merge/decay policy, and
//! `crate::cross_shard`/`crate::mirror_watcher` for how a discovered shard
//! is consumed once learned — discovering it never implies trusting it;
//! #543's key-resolution/verification step is unconditional and unchanged.
//!
//! **Layer 3: bounded head-summary gossip.**
//! [`HeadSummary`]/[`HeadGossipTracker`] ride the same announce exchange
//! (`known_shards`'s sibling field, `head_summaries`) but are deliberately
//! their own small, bounded structure — see [`HeadGossipTracker`]'s own doc
//! comment for why it never touches [`PeerTable`] or [`ShardRegistry`]. A
//! conflicting pair of summaries is only a *signal*; confirming it as a
//! real equivocation (fetching full cosignature detail, checking majority
//! intersection) is `crate::equivocation::confirm_and_record`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use avalon_chain::PostgresSettlementProvider;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Extension;
use axum::Json;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::AppError;
use crate::network_coordinates::Coordinate;
use crate::peer_admission::{admission, sanitize_libp2p_addrs, AdmitError, PeerAdmission};
use crate::state::AppState;
use crate::topology_limits::{client_ip, TopologyError};

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
    /// This peer's witness key advertisement. Only ever holds an advert whose
    /// proof of possession verified for this `base_url`; unproven adverts are
    /// dropped before an entry is stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness: Option<WitnessAdvert>,
    /// Self-reported reachability hint; never used for trust decisions. `None` from old peers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connectivity: Option<avalon_protocol::connectivity::Connectivity>,
    /// Set only when this entry's libp2p id, addresses and connectivity were vouched for by the
    /// URL's own server, never by gossip or a third party. Only bound entries are routed by id.
    #[serde(skip)]
    pub identity_bound: bool,
}

/// A witness key advertisement: the hex Ed25519 verifying key a node
/// cosigns under plus a signature by that key over
/// `(base_url, key_id, announced_at)`, so the key cannot be claimed by a
/// party that cannot sign with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WitnessAdvert {
    pub key_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub announced_at: OffsetDateTime,
    pub proof: String,
    /// Set only when verified from the announce response of this exact
    /// `base_url`, i.e. the URL's own endpoint vouches for the key. Never
    /// serialized, so relayed or inbound adverts always start `false`.
    #[serde(skip)]
    pub direct: bool,
}

/// This node's own witness signing identity, used to build [`WitnessAdvert`]s.
#[derive(Clone)]
pub struct WitnessSigner {
    signing_key: ed25519_dalek::SigningKey,
    key_id: String,
}

impl WitnessSigner {
    /// `None` unless `key_id` is the hex verifying key of `signing_key`: an
    /// overridden, non-key id cannot be proven and is never advertised.
    pub fn new(signing_key: ed25519_dalek::SigningKey, key_id: String) -> Option<Self> {
        (hex::encode(signing_key.verifying_key().to_bytes()) == key_id).then_some(Self {
            signing_key,
            key_id,
        })
    }

    /// A fresh advert binding this key to `base_url`.
    pub fn advert(&self, base_url: &str, now: OffsetDateTime) -> WitnessAdvert {
        let base_url = normalized_base_url(base_url);
        WitnessAdvert {
            key_id: self.key_id.clone(),
            announced_at: now,
            proof: avalon_protocol::witness::sign_witness_announce(
                &self.signing_key,
                &base_url,
                &self.key_id,
                now,
            ),
            direct: false,
        }
    }
}

/// The form of `raw` peer entries are stored and proven under.
pub fn normalized_base_url(raw: &str) -> String {
    admission()
        .check_shape(raw)
        .unwrap_or_else(|_| raw.trim_end_matches('/').to_string())
}

/// Returns `advert` only if its proof verifies for `base_url` (already
/// normalized) and is fresh; a forged, mismatched or stale advert yields
/// `None` so the peer is still admitted, just without a witness key.
pub fn verified_advert(
    base_url: &str,
    advert: Option<WitnessAdvert>,
    now: OffsetDateTime,
) -> Option<WitnessAdvert> {
    let advert = advert?;
    if avalon_protocol::witness::verify_witness_announce(
        base_url,
        &advert.key_id,
        advert.announced_at,
        &advert.proof,
        now,
    ) {
        Some(advert)
    } else {
        tracing::warn!(
            event = "witness_advert_rejected",
            peer = %base_url,
            "ignored a witness key advertisement with an invalid or stale proof",
        );
        None
    }
}

/// Whether `new` may replace `old`: a direct advert is never displaced by a
/// non-direct one; otherwise the newer advert wins.
fn advert_replaces(old: &WitnessAdvert, new: &WitnessAdvert) -> bool {
    match (old.direct, new.direct) {
        (true, false) => false,
        (false, true) => true,
        _ => new.announced_at >= old.announced_at,
    }
}

/// Applies the advert-replacement rule when `info` is stored over an existing
/// entry; an entry without an advert never erases a held one.
fn carry_witness(existing: Option<&PeerInfo>, info: &mut PeerInfo) {
    let Some(old) = existing.and_then(|e| e.witness.as_ref()) else {
        return;
    };
    match &info.witness {
        Some(new) if advert_replaces(old, new) => {}
        _ => info.witness = Some(old.clone()),
    }
}

/// Keeps a bound entry's identity when a write does not carry its own proof of binding: the
/// same id refreshes addresses and connectivity, a different id changes nothing.
fn carry_identity(existing: Option<&PeerInfo>, info: &mut PeerInfo) {
    let Some(old) = existing.filter(|e| e.identity_bound) else {
        return;
    };
    if info.identity_bound {
        return;
    }
    if info.libp2p_peer_id == old.libp2p_peer_id {
        info.identity_bound = true;
    } else {
        info.libp2p_peer_id = old.libp2p_peer_id.clone();
        info.libp2p_listen_addrs = old.libp2p_listen_addrs.clone();
        info.connectivity = old.connectivity;
        info.identity_bound = true;
    }
}

pub(crate) fn is_p2p_url(url: &str) -> bool {
    url.starts_with("p2p://")
}

/// Most `p2p://` entries the main table holds, so free peer ids cannot fill it.
const MAX_P2P_ENTRIES: usize = 64;
/// Most `p2p://` entries the unverified pool holds.
const MAX_P2P_UNVERIFIED: usize = 32;
/// Most `p2p://` shard URLs the shard registry holds in total.
const MAX_P2P_SHARD_URLS_TOTAL: usize = 32;
/// Most unseen `p2p://` shard URLs accepted from one gossip exchange.
const MAX_P2P_SHARD_URLS_PER_EXCHANGE: usize = 8;

/// Turns a pooled `p2p://<id>` entry into what a libp2p contact proves: only the id. Roles,
/// version and addresses are placeholders until the peer announces itself. Other entries are
/// left as they were; returns whether `info` was rewritten.
fn bind_if_p2p_url_names_peer(info: &mut PeerInfo, peer_id: &str) -> bool {
    let names_peer = crate::node_http::parse_p2p_base(&info.base_url)
        .is_some_and(|id| id.to_string() == peer_id)
        && info.libp2p_peer_id.as_deref() == Some(peer_id);
    if names_peer {
        info.identity_bound = true;
        info.roles = Vec::new();
        info.protocol_version = "0.0.0".to_string();
        info.libp2p_listen_addrs = Vec::new();
        info.connectivity = None;
        info.witness = None;
    }
    names_peer
}

/// The peer table is at its cap and every entry is active or a bootstrap peer.
#[derive(Debug, PartialEq, Eq)]
pub struct TableFull;

/// Bound on the unverified pool, independent of `AVALON_NODE_MAX_KNOWN_PEERS`
/// and not itself hoster-configurable: gossip relay alone must never be able
/// to grow to main-table scale.
const MAX_UNVERIFIED_PEERS: usize = 256;

/// `Arc<RwLock<_>>` around a plain map, cheap to clone into [`AppState`] —
/// same shape `crate::presence::PresenceStore` already establishes for
/// in-process, non-durable state. Keyed by `base_url`: a peer is uniquely
/// identified by where it's reachable, not by any self-reported id.
///
/// `unverified` holds entries relayed by gossip only — never contacted, never
/// self-announced. An entry moves into `peers` only via a direct announce
/// (`admit_new_announcer`) or a successful outbound contact by this node
/// (`run_worker`'s announce, or `/nodes/probe`); gossip alone never writes
/// into `peers`. Both maps share the same `last_announced_at` expiry rule.
#[derive(Clone, Default)]
pub struct PeerTable {
    peers: Arc<RwLock<HashMap<String, PeerInfo>>>,
    unverified: Arc<RwLock<HashMap<String, PeerInfo>>>,
    neighbors: crate::neighbors::NeighborTable,
}

impl PeerTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// The announce worker's active set and per-neighbor round-trip stats.
    pub fn neighbors(&self) -> &crate::neighbors::NeighborTable {
        &self.neighbors
    }

    /// Inserts or refreshes `info`, keyed by its own `base_url`.
    pub fn upsert(&self, mut info: PeerInfo) {
        let mut peers = self.peers.write().expect("peer table lock poisoned");
        carry_witness(peers.get(&info.base_url), &mut info);
        carry_identity(peers.get(&info.base_url), &mut info);
        peers.insert(info.base_url.clone(), info);
    }

    /// Records a verified witness advert on an existing entry (main table or
    /// unverified pool) under the replacement rule; a no-op for an unknown
    /// peer.
    pub fn attach_witness(&self, base_url: &str, advert: WitnessAdvert) {
        let mut peers = self.peers.write().expect("peer table lock poisoned");
        if let Some(p) = peers.get_mut(base_url) {
            if p.witness
                .as_ref()
                .is_none_or(|old| advert_replaces(old, &advert))
            {
                p.witness = Some(advert);
            }
            return;
        }
        drop(peers);
        let mut pool = self
            .unverified
            .write()
            .expect("unverified pool lock poisoned");
        if let Some(p) = pool.get_mut(base_url) {
            if p.witness
                .as_ref()
                .is_none_or(|old| advert_replaces(old, &advert))
            {
                p.witness = Some(advert);
            }
        }
    }

    pub fn contains(&self, base_url: &str) -> bool {
        self.peers
            .read()
            .expect("peer table lock poisoned")
            .contains_key(base_url)
    }

    /// Inserts or refreshes `info`, holding the table to `max` entries. A new
    /// entry into a full table evicts the entry with the oldest
    /// `last_announced_at` that is neither active nor a bootstrap peer, and
    /// returns its base URL; with nothing evictable the newcomer is refused.
    pub fn insert_bounded(
        &self,
        mut info: PeerInfo,
        max: usize,
    ) -> Result<Option<String>, TableFull> {
        let protected = self.neighbors.protected_urls();
        let mut peers = self.peers.write().expect("peer table lock poisoned");
        carry_witness(peers.get(&info.base_url), &mut info);
        carry_identity(peers.get(&info.base_url), &mut info);
        let mut evicted = None;
        let is_new = !peers.contains_key(&info.base_url);
        let oldest = |peers: &HashMap<String, PeerInfo>, only_p2p: bool| {
            peers
                .values()
                .filter(|p| !protected.contains(&p.base_url))
                .filter(|p| !only_p2p || is_p2p_url(&p.base_url))
                .min_by(|a, b| {
                    // `p2p://` entries go first: they cost nothing to create.
                    is_p2p_url(&b.base_url)
                        .cmp(&is_p2p_url(&a.base_url))
                        .then_with(|| a.last_announced_at.cmp(&b.last_announced_at))
                        .then_with(|| a.base_url.cmp(&b.base_url))
                })
                .map(|p| p.base_url.clone())
        };
        let p2p_count = peers.keys().filter(|u| is_p2p_url(u)).count();
        if is_new && is_p2p_url(&info.base_url) && p2p_count >= MAX_P2P_ENTRIES.min(max) {
            let victim = oldest(&peers, true).ok_or(TableFull)?;
            peers.remove(&victim);
            evicted = Some(victim);
        }
        if is_new && peers.len() >= max {
            // A p2p newcomer never displaces an http peer.
            let victim = oldest(&peers, is_p2p_url(&info.base_url)).ok_or(TableFull)?;
            peers.remove(&victim);
            evicted = Some(victim);
        }
        peers.insert(info.base_url.clone(), info);
        Ok(evicted)
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

    /// The URL requests to the known peer `base_url` should use: `p2p://` when it is not
    /// reachable by URL, else `base_url` itself (see [`crate::node_http::NodeClient::url_for`]).
    pub fn transport_url(&self, base_url: &str) -> String {
        let peers = self.peers.read().expect("peer table lock poisoned");
        peers
            .get(base_url)
            .map(crate::node_http::NodeClient::url_for)
            .unwrap_or_else(|| base_url.to_string())
    }

    /// `p2p://<id>` for the known peer `base_url` when its id is bound (or the URL is already
    /// `p2p://`), for callers that must reach it over an authenticated stream.
    pub fn stream_url(&self, base_url: &str) -> Option<String> {
        if let Some(id) = crate::node_http::parse_p2p_base(base_url) {
            return Some(crate::node_http::p2p_base_url(&id));
        }
        let peers = self.peers.read().expect("peer table lock poisoned");
        let info = peers.get(base_url).filter(|p| p.identity_bound)?;
        let id: libp2p::PeerId = info.libp2p_peer_id.as_ref()?.parse().ok()?;
        Some(crate::node_http::p2p_base_url(&id))
    }

    /// The libp2p id of the known peer whose `base_url` `url` is under, for the stream fallback.
    pub fn libp2p_peer_for_url(&self, url: &str) -> Option<libp2p::PeerId> {
        let peers = self.peers.read().expect("peer table lock poisoned");
        peers
            .values()
            .filter(|p| {
                url.strip_prefix(p.base_url.trim_end_matches('/'))
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '?']))
            })
            .filter(|p| p.identity_bound)
            .find_map(|p| p.libp2p_peer_id.as_ref()?.parse().ok())
    }

    /// Whether `peer` is the libp2p id of a bound entry whose http URL answered `/nodes/status`
    /// with that id. A self-announced `p2p://` entry only proves a key pair, so it never counts.
    pub fn is_bound_libp2p_peer(&self, peer: &libp2p::PeerId) -> bool {
        let id = peer.to_string();
        self.peers
            .read()
            .expect("peer table lock poisoned")
            .values()
            .any(|p| {
                p.identity_bound
                    && !is_p2p_url(&p.base_url)
                    && p.libp2p_peer_id.as_deref() == Some(id.as_str())
            })
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

    /// Inserts or refreshes `info` in the unverified pool, keyed by its own
    /// `base_url`. A new entry into a full pool evicts the entry with the
    /// oldest `last_announced_at` — unlike [`Self::insert_bounded`], nothing
    /// here is protected, so this never refuses a newcomer.
    pub fn insert_unverified(&self, mut info: PeerInfo) {
        let mut pool = self
            .unverified
            .write()
            .expect("unverified pool lock poisoned");
        carry_witness(pool.get(&info.base_url), &mut info);
        let new_p2p = is_p2p_url(&info.base_url) && !pool.contains_key(&info.base_url);
        let oldest = |pool: &HashMap<String, PeerInfo>, only_p2p: bool| {
            pool.values()
                .filter(|p| !only_p2p || is_p2p_url(&p.base_url))
                .min_by(|a, b| {
                    is_p2p_url(&b.base_url)
                        .cmp(&is_p2p_url(&a.base_url))
                        .then_with(|| a.last_announced_at.cmp(&b.last_announced_at))
                        .then_with(|| a.base_url.cmp(&b.base_url))
                })
                .map(|p| p.base_url.clone())
        };
        if new_p2p && pool.keys().filter(|u| is_p2p_url(u)).count() >= MAX_P2P_UNVERIFIED {
            if let Some(victim) = oldest(&pool, true) {
                pool.remove(&victim);
            }
        }
        if !pool.contains_key(&info.base_url) && pool.len() >= MAX_UNVERIFIED_PEERS {
            // A p2p newcomer only displaces p2p entries, and is dropped if there are none.
            match oldest(&pool, new_p2p) {
                Some(victim) => {
                    pool.remove(&victim);
                }
                None => return,
            }
        }
        pool.insert(info.base_url.clone(), info);
    }

    /// Every entry currently in the unverified pool.
    pub fn list_unverified(&self) -> Vec<PeerInfo> {
        self.unverified
            .read()
            .expect("unverified pool lock poisoned")
            .values()
            .cloned()
            .collect()
    }

    /// Removes `base_url` from the unverified pool and returns it, if
    /// present — the promotion step: the caller is expected to insert the
    /// returned entry into the main table right after, having just confirmed
    /// it via a direct announce or a successful outbound contact.
    pub fn take_unverified(&self, base_url: &str) -> Option<PeerInfo> {
        self.unverified
            .write()
            .expect("unverified pool lock poisoned")
            .remove(base_url)
    }

    /// Moves the unverified entries naming `libp2p_peer_id` into the main table. The libp2p
    /// handshake authenticates the peer id, so an outbound connection to that id is the same
    /// kind of confirmation a successful outbound HTTP contact is. Returns the promoted URLs.
    pub(crate) fn promote_unverified_by_libp2p_peer(
        &self,
        libp2p_peer_id: &str,
        max_known_peers: usize,
    ) -> Vec<String> {
        let urls: Vec<String> = self
            .unverified
            .read()
            .expect("unverified pool lock poisoned")
            .values()
            .filter(|p| p.libp2p_peer_id.as_deref() == Some(libp2p_peer_id))
            .map(|p| p.base_url.clone())
            .collect();
        let mut promoted = Vec::new();
        for url in urls {
            if let Some(mut info) = self.take_unverified(&url) {
                bind_if_p2p_url_names_peer(&mut info, libp2p_peer_id);
                if is_p2p_url(&url) && self.contains(&url) {
                    continue;
                }
                if self.insert_bounded(info, max_known_peers).is_ok() {
                    promoted.push(url);
                }
            }
        }
        promoted
    }

    /// Same expiry rule as [`Self::prune_older_than`], applied to the unverified pool: an entry that never gets promoted ages out.
    pub fn prune_unverified_older_than(&self, cutoff: OffsetDateTime) {
        self.unverified
            .write()
            .expect("unverified pool lock poisoned")
            .retain(|_, info| info.last_announced_at >= cutoff);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.peers.read().expect("peer table lock poisoned").len()
    }

    #[cfg(test)]
    fn unverified_len(&self) -> usize {
        self.unverified
            .read()
            .expect("unverified pool lock poisoned")
            .len()
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
        // Past the cap the oldest `p2p://` URLs go; they cost nothing to claim.
        let mut p2p: Vec<(OffsetDateTime, String, String)> = shards
            .iter()
            .flat_map(|(id, urls)| {
                urls.iter()
                    .filter(|(u, _)| is_p2p_url(u))
                    .map(move |(u, t)| (*t, id.clone(), u.clone()))
            })
            .collect();
        if p2p.len() > MAX_P2P_SHARD_URLS_TOTAL {
            p2p.sort();
            for (_, id, url) in &p2p[..p2p.len() - MAX_P2P_SHARD_URLS_TOTAL] {
                if let Some(urls) = shards.get_mut(id) {
                    urls.remove(url);
                    if urls.is_empty() {
                        shards.remove(id);
                    }
                }
            }
        }
        newly_learned
    }

    /// Whether `(shard_id, url)` is already on record.
    pub fn has_url(&self, shard_id: &str, url: &str) -> bool {
        self.shards
            .read()
            .expect("shard registry lock poisoned")
            .get(shard_id)
            .is_some_and(|urls| urls.contains_key(url))
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

/// Bound on head-summary gossip per exchange — the design doc's "Head
/// gossip" section (`docs/projects/backend-server/architecture/witness-cosigning.md`):
/// small and fixed, matching the `MAX_UNVERIFIED_PEERS` size discipline
/// already established for node-coordination gossip, not
/// hoster-configurable.
const MAX_HEAD_SUMMARIES_PER_EXCHANGE: usize = 5;

/// Bound on the total number of distinct `(shard_id, tree_size, root_hash)`
/// combinations [`HeadGossipTracker`] holds at once, independent of the
/// per-exchange cap above — a peer spread across many exchanges must not be
/// able to grow this without bound either.
const MAX_TRACKED_HEAD_SUMMARIES: usize = 4096;

/// One node's most-recently-observed cosigned-head summary for a shard it
/// tracks — gossip payload. Deliberately not the full
/// cosignature set: only a count. A node that wants the underlying
/// signatures fetches them directly (`crate::settlement::latest_sth`/
/// `sth_at_tree_size` with `?witnesses=1`), never receives them over this
/// gossip path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeadSummary {
    pub shard_id: String,
    pub tree_size: i64,
    pub root_hash: String,
    pub cosignature_count: usize,
}

fn head_summary_well_formed(adm: &PeerAdmission, summary: &HeadSummary) -> bool {
    adm.shard_id_ok(&summary.shard_id)
        && summary.tree_size >= 0
        && hex::decode(&summary.root_hash).is_ok_and(|bytes| bytes.len() == 32)
}

/// Two different roots reported for the same `(shard_id, tree_size)` — the
/// fork signal the design doc calls out ("Fork detection is a side effect
/// of this gossip, not a separate mechanism"). Carries which peer reported
/// each side, so a caller knows where to fetch full cosignature detail
/// from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadConflict {
    pub shard_id: String,
    pub tree_size: i64,
    pub root_hash_a: String,
    pub source_a: String,
    pub root_hash_b: String,
    pub source_b: String,
}

#[derive(Clone)]
struct TrackedHeadSummary {
    summary: HeadSummary,
    reported_by: String,
    last_seen_at: OffsetDateTime,
}

/// `(shard_id, tree_size, root_hash)` — [`HeadGossipTracker`]'s key.
type TrackedHeadKey = (String, i64, String);

/// This node's bounded, in-memory view of "what root has each peer most
/// recently claimed for this shard at this tree_size".
///
/// **Deliberately its own structure, never folded into [`PeerTable`] or
/// [`ShardRegistry`].** Those exist for peer/shard *discovery* — admitting
/// something here never places an entry into either of them, and merging
/// gossip into either of them never touches this. A head summary is log
/// data, not an address to dial or a peer to trust.
#[derive(Clone, Default)]
pub struct HeadGossipTracker {
    seen: Arc<RwLock<HashMap<TrackedHeadKey, TrackedHeadSummary>>>,
    /// `shard_id`s a confirmed equivocation has been recorded for — the
    /// gate `crate::witness_cosign::decide_and_cosign` consults before
    /// cosigning anything for this shard.
    equivocating_shards: Arc<RwLock<HashSet<String>>>,
}

impl HeadGossipTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Evicts the least-recently-seen tracked entry if `seen` is already at
    /// [`MAX_TRACKED_HEAD_SUMMARIES`] and `key` isn't already present —
    /// same bounded-with-LRU-eviction shape [`PeerTable::insert_unverified`]
    /// already establishes.
    fn evict_if_full(seen: &mut HashMap<TrackedHeadKey, TrackedHeadSummary>, key: &TrackedHeadKey) {
        if seen.contains_key(key) || seen.len() < MAX_TRACKED_HEAD_SUMMARIES {
            return;
        }
        if let Some(victim) = seen
            .iter()
            .min_by(|a, b| {
                a.1.last_seen_at
                    .cmp(&b.1.last_seen_at)
                    .then_with(|| a.0.cmp(b.0))
            })
            .map(|(key, _)| key.clone())
        {
            seen.remove(&victim);
        }
    }

    /// Merges `incoming` (a gossip exchange from `from_peer`, or this
    /// node's own current head via [`Self::record_own`]) — validates each
    /// entry before ever admitting it (never relayed or stored unchecked),
    /// examines at most [`MAX_HEAD_SUMMARIES_PER_EXCHANGE`] of them
    /// regardless of how many `incoming` actually holds (defense in depth:
    /// a well-behaved peer already caps its own [`Self::snapshot`] to this
    /// same bound, but a malicious one might not), and returns what was
    /// admitted, any new conflicts this merge surfaced, and how many
    /// entries were rejected outright (malformed, or past the per-exchange
    /// cap).
    ///
    /// A conflict already confirmed as an equivocation
    /// (`shard_id` in `equivocating_shards`) is not re-reported — the
    /// network-wide "stop trusting this log" fact is already on record; a
    /// fresh gossip round repeating the same two roots has nothing new to
    /// prove.
    pub fn merge(
        &self,
        from_peer: &str,
        incoming: &[HeadSummary],
        now: OffsetDateTime,
    ) -> (Vec<HeadSummary>, Vec<HeadConflict>, usize) {
        let adm = admission();
        let mut admitted = Vec::new();
        let mut conflicts = Vec::new();
        let mut rejected = incoming
            .len()
            .saturating_sub(MAX_HEAD_SUMMARIES_PER_EXCHANGE);

        for summary in incoming.iter().take(MAX_HEAD_SUMMARIES_PER_EXCHANGE) {
            if !head_summary_well_formed(adm, summary) {
                rejected += 1;
                continue;
            }

            let already_equivocating = self
                .equivocating_shards
                .read()
                .expect("head gossip tracker lock poisoned")
                .contains(&summary.shard_id);

            let key = (
                summary.shard_id.clone(),
                summary.tree_size,
                summary.root_hash.clone(),
            );
            {
                let mut seen = self
                    .seen
                    .write()
                    .expect("head gossip tracker lock poisoned");
                Self::evict_if_full(&mut seen, &key);
                seen.insert(
                    key,
                    TrackedHeadSummary {
                        summary: summary.clone(),
                        reported_by: from_peer.to_string(),
                        last_seen_at: now,
                    },
                );

                if !already_equivocating {
                    for ((other_shard, other_size, other_root), tracked) in seen.iter() {
                        if other_shard == &summary.shard_id
                            && *other_size == summary.tree_size
                            && other_root != &summary.root_hash
                        {
                            conflicts.push(HeadConflict {
                                shard_id: summary.shard_id.clone(),
                                tree_size: summary.tree_size,
                                root_hash_a: summary.root_hash.clone(),
                                source_a: from_peer.to_string(),
                                root_hash_b: other_root.clone(),
                                source_b: tracked.reported_by.clone(),
                            });
                        }
                    }
                }
            }
            admitted.push(summary.clone());
        }

        (admitted, conflicts, rejected)
    }

    /// This node's own current head, recorded the same way a peer's gossip
    /// would be — feeds into [`Self::snapshot`] so this node's own view is
    /// part of what it gossips out, matching [`ShardRegistry::record_own`]'s
    /// precedent.
    pub fn record_own(&self, summary: HeadSummary, own_identity: &str, now: OffsetDateTime) {
        self.merge(own_identity, &[summary], now);
    }

    /// The at-most-[`MAX_HEAD_SUMMARIES_PER_EXCHANGE`] most-recently-seen
    /// summaries this node currently holds — what gets attached to an
    /// outbound announce request/response, matching the design doc's "not
    /// full cosignature bytes on every exchange" bound exactly.
    pub fn snapshot(&self) -> Vec<HeadSummary> {
        let seen = self.seen.read().expect("head gossip tracker lock poisoned");
        let mut entries: Vec<&TrackedHeadSummary> = seen.values().collect();
        entries.sort_by(|a, b| {
            b.last_seen_at
                .cmp(&a.last_seen_at)
                .then_with(|| a.summary.shard_id.cmp(&b.summary.shard_id))
        });
        entries
            .into_iter()
            .take(MAX_HEAD_SUMMARIES_PER_EXCHANGE)
            .map(|t| t.summary.clone())
            .collect()
    }

    /// Records `shard_id` as having a confirmed equivocation — called once
    /// a caller has independently fetched full cosignature detail for both
    /// conflicting sides of a [`HeadConflict`] and confirmed it with
    /// `avalon_protocol::cosigned_sth::find_equivocating_witnesses`
    /// (see `crate::equivocation::confirm_and_record`). Idempotent.
    pub fn mark_equivocating(&self, shard_id: &str) {
        self.equivocating_shards
            .write()
            .expect("head gossip tracker lock poisoned")
            .insert(shard_id.to_string());
    }

    /// Whether `shard_id` has a confirmed equivocation on record — checked
    /// by `crate::witness_cosign::decide_and_cosign` before it cosigns
    /// anything for this shard.
    pub fn is_equivocating(&self, shard_id: &str) -> bool {
        self.equivocating_shards
            .read()
            .expect("head gossip tracker lock poisoned")
            .contains(shard_id)
    }

    #[cfg(test)]
    fn tracked_len(&self) -> usize {
        self.seen
            .read()
            .expect("head gossip tracker lock poisoned")
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
    /// Layer 2: this node's own current [`ShardRegistry`]
    /// snapshot — gossiped to the callee on every announce, merged into
    /// its own registry the same way [`AnnounceResponse::known_shards`] is
    /// merged back into this node's. `#[serde(default)]` so an older
    /// peer's announce still decodes, just with nothing to
    /// merge.
    #[serde(default)]
    pub known_shards: Vec<ShardAnnouncement>,
    /// This node's own current [`HeadGossipTracker::snapshot`]
    /// — at most [`MAX_HEAD_SUMMARIES_PER_EXCHANGE`] entries, gossiped
    /// alongside `known_shards` on every exchange. `#[serde(default)]` so
    /// an older peer's announce still decodes with nothing to merge.
    #[serde(default)]
    pub head_summaries: Vec<HeadSummary>,
    /// The sender's witness key advertisement, when it cosigns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness: Option<WitnessAdvert>,
    /// The sender's own network coordinate.
    pub coordinate: Coordinate,
    /// Self-reported reachability hint; see [`PeerInfo::connectivity`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connectivity: Option<avalon_protocol::connectivity::Connectivity>,
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
    /// See [`AnnounceRequest::head_summaries`]'s own doc
    /// comment; the same exchange in the other direction.
    #[serde(default)]
    pub head_summaries: Vec<HeadSummary>,
    /// The responder's own witness key advertisement, when it cosigns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness: Option<WitnessAdvert>,
    /// The responder's own network coordinate.
    pub coordinate: Coordinate,
    /// The responder's own entry, so a caller that only knows its HTTP URL learns its libp2p
    /// identity. Only set when the responder has an own base URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<PeerInfo>,
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
///
/// A caller announcing `p2p://<peer id>` (no URL of its own) is admitted only over a libp2p
/// stream authenticated as that peer id; over plain HTTP it gets the response and nothing
/// it sent is stored.
pub async fn announce(
    State(state): State<AppState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    remote: Option<Extension<crate::node_http::RemotePeer>>,
    Json(body): Json<AnnounceRequest>,
) -> Result<Json<AnnounceResponse>, AnnounceError> {
    if body.network_id != state.chain.network_id() {
        return Err(AppError::PeerNetworkMismatch.into());
    }

    let adm = admission();
    let caller_base_url = body.base_url.clone();
    let p2p = classify_p2p_announce(
        &body.base_url,
        body.libp2p_peer_id.as_deref(),
        remote.map(|Extension(r)| r.0),
    )?;
    if p2p == P2pAnnounce::Unauthenticated {
        // Nothing the caller sent is stored or gossiped: no connection proves it is that peer.
        return Ok(Json(announce_response(&state, &caller_base_url)));
    }
    let mut info = PeerInfo {
        base_url: body.base_url,
        roles: body.roles,
        protocol_version: body.protocol_version,
        network_id: body.network_id,
        last_announced_at: OffsetDateTime::now_utc(),
        libp2p_listen_addrs: sanitize_libp2p_addrs(
            body.libp2p_peer_id.as_deref(),
            &body.libp2p_listen_addrs,
            &adm.policy,
        ),
        libp2p_peer_id: body.libp2p_peer_id,
        witness: None,
        connectivity: body.connectivity,
        identity_bound: false,
    };
    if crate::version::is_supported(&info.protocol_version) {
        if let P2pAnnounce::Authenticated(id) = p2p {
            info.base_url = crate::node_http::p2p_base_url(&id);
            info.witness = verified_advert(&info.base_url, body.witness, info.last_announced_at);
            store_authenticated_p2p_announcer(
                &state.peers,
                adm,
                client_ip(source, &headers),
                info,
            )?;
        } else {
            info.base_url = adm
                .check_shape(&info.base_url)
                .map_err(TopologyError::from)?;
            info.witness = verified_advert(&info.base_url, body.witness, info.last_announced_at);
            if state.peers.contains(&info.base_url) {
                let info = rebind_existing(
                    &state.peers,
                    state.chain.network_id(),
                    adm,
                    client_ip(source, &headers),
                    info,
                )
                .await;
                state.peers.upsert(info);
            } else {
                admit_new_announcer(&state, adm, client_ip(source, &headers), info).await?;
            }
        }
    } else {
        state.peers.admit_if_supported(info);
    }

    // Shard claims are discovery data whatever the caller's protocol version;
    // downstream key verification is unconditional.
    let (shards, rejected_shards) =
        validated_shards(adm, &state.shard_registry, &body.known_shards).await;
    if rejected_shards > 0 {
        tracing::warn!(
            event = "shard_gossip_entries_rejected",
            from_peer = %caller_base_url,
            rejected = rejected_shards,
            "skipped shard entries that failed address validation or limits",
        );
    }
    let newly_learned = state.shard_registry.merge(&shards);
    for shard_id in &newly_learned {
        tracing::info!(
            event = "shard_discovered",
            shard_id = %shard_id,
            from_peer = %caller_base_url,
            "learned of a new shard via peer-announce gossip",
        );
    }

    // Head-summary gossip rides the same exchange, bounded and
    // validated the same way shard gossip is above — see
    // `HeadGossipTracker::merge`'s own doc comment.
    let (_admitted_heads, conflicts, rejected_heads) = state.head_gossip.merge(
        &caller_base_url,
        &body.head_summaries,
        OffsetDateTime::now_utc(),
    );
    if rejected_heads > 0 {
        tracing::warn!(
            event = "head_summary_gossip_entries_rejected",
            from_peer = %caller_base_url,
            rejected = rejected_heads,
            "skipped head-summary entries that failed validation or exceeded the per-exchange \
             limit",
        );
    }
    for conflict in conflicts {
        tracing::warn!(
            event = "head_summary_conflict_detected",
            shard_id = %conflict.shard_id,
            tree_size = conflict.tree_size,
            root_hash_a = %conflict.root_hash_a,
            root_hash_b = %conflict.root_hash_b,
            "two different roots reported for the same shard/tree_size via gossip — fetching \
             full cosignature detail to confirm",
        );
        let chain = state.chain.clone();
        let head_gossip = state.head_gossip.clone();
        let peers = state.peers.clone();
        let known_list = state.known_list.clone();
        tokio::spawn(async move {
            crate::equivocation::confirm_and_record(
                &chain,
                &head_gossip,
                &peers,
                &known_list,
                conflict,
            )
            .await;
        });
    }

    Ok(Json(announce_response(&state, &caller_base_url)))
}

/// This node's answer to an announce from `caller_base_url`.
fn announce_response(state: &AppState, caller_base_url: &str) -> AnnounceResponse {
    let own_witness = state
        .own_witness
        .as_ref()
        .zip(state.own_base_url.as_deref())
        .map(|(w, url)| w.advert(url, OffsetDateTime::now_utc()));
    AnnounceResponse {
        witness: own_witness,
        peers: state.peers.list_excluding(caller_base_url),
        known_shards: state.shard_registry.snapshot(),
        head_summaries: state.head_gossip.snapshot(),
        coordinate: state.peers.neighbors().own_coordinate(),
        node: state.own_base_url.as_deref().map(|base_url| {
            own_peer_info(
                base_url,
                state.own_libp2p_peer_id.clone(),
                &state.reachability,
                state.chain.network_id(),
                OffsetDateTime::now_utc(),
            )
        }),
    }
}

/// How an announce's base URL relates to the connection it arrived on.
#[derive(Debug, PartialEq, Eq)]
enum P2pAnnounce {
    /// An http(s) base URL, admitted by the address and reachability checks.
    NotP2p,
    /// `p2p://<id>` on a stream the libp2p handshake authenticated as `<id>`.
    Authenticated(libp2p::PeerId),
    /// `p2p://<id>` with nothing proving the caller holds that id (plain HTTP).
    Unauthenticated,
}

/// A `p2p://` base URL is only honoured for a caller authenticated as that peer id, and must
/// name the same id in `libp2p_peer_id`.
fn classify_p2p_announce(
    base_url: &str,
    claimed_id: Option<&str>,
    remote: Option<libp2p::PeerId>,
) -> Result<P2pAnnounce, TopologyError> {
    if !base_url.trim().starts_with("p2p://") {
        return Ok(P2pAnnounce::NotP2p);
    }
    let mismatch =
        |status, message: &str| Err(TopologyError::new(status, "p2p_identity_mismatch", message));
    let Some(id) = crate::node_http::parse_p2p_base(base_url) else {
        return mismatch(
            StatusCode::BAD_REQUEST,
            "base_url is not a valid p2p:// URL",
        );
    };
    if claimed_id != Some(id.to_string().as_str()) {
        return mismatch(
            StatusCode::BAD_REQUEST,
            "a p2p:// base_url must name the same libp2p_peer_id",
        );
    }
    match remote {
        Some(r) if r == id => Ok(P2pAnnounce::Authenticated(id)),
        Some(_) => mismatch(
            StatusCode::FORBIDDEN,
            "the connection is authenticated as a different libp2p peer",
        ),
        None => Ok(P2pAnnounce::Unauthenticated),
    }
}

/// Error type of [`announce`]: the network mismatch keeps its own status and
/// code; every admission refusal carries a machine-readable code.
#[derive(Debug)]
pub enum AnnounceError {
    App(AppError),
    Rejected(TopologyError),
}

impl From<AppError> for AnnounceError {
    fn from(e: AppError) -> Self {
        Self::App(e)
    }
}

impl From<TopologyError> for AnnounceError {
    fn from(e: TopologyError) -> Self {
        Self::Rejected(e)
    }
}

impl axum::response::IntoResponse for AnnounceError {
    fn into_response(self) -> axum::response::Response {
        match self {
            Self::App(e) => e.into_response(),
            Self::Rejected(e) => e.into_response(),
        }
    }
}

/// Admission path for a base URL not yet in the table: per-source budget,
/// address policy, reachability, then the bounded insert.
async fn admit_new_announcer(
    state: &AppState,
    adm: &PeerAdmission,
    source: IpAddr,
    info: PeerInfo,
) -> Result<(), TopologyError> {
    adm.admit_new_url_from(source)?;
    let _permit = adm.enter_check()?;
    let checked = adm.check_address(&info.base_url).await?;
    let mut info = info;
    if adm.cfg.verify_reachability {
        let reported = adm
            .verify_reachable(&checked, state.chain.network_id())
            .await?;
        bind_or_blank(&mut info, reported);
    }
    let base_url = info.base_url.clone();
    match admit_promoted(&state.peers, info, adm.cfg.max_known_peers) {
        Ok(Some(evicted)) => {
            tracing::info!(
                event = "peer_evicted_for_capacity",
                evicted = %evicted,
                admitted = %base_url,
                "peer table full: evicted the oldest inactive entry",
            );
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(TableFull) => {
            tracing::warn!(
                event = "peer_table_full",
                peer = %base_url,
                "peer table full and every entry is active or bootstrap; announce refused",
            );
            Err(AdmitError::TableFull.into())
        }
    }
}

/// Stores an announcer whose `p2p://<id>` URL the stream authenticated, bound to that id. The
/// handshake stands in for the address and reachability checks: there is no address to check
/// and the peer id was proven by the connection. A new URL still counts against the source
/// budget and the table cap.
fn store_authenticated_p2p_announcer(
    peers: &PeerTable,
    adm: &PeerAdmission,
    source: IpAddr,
    mut info: PeerInfo,
) -> Result<(), TopologyError> {
    if info.base_url.len() > adm.cfg.max_url_len {
        return Err(AdmitError::TooLong.into());
    }
    info.identity_bound = true;
    if peers.contains(&info.base_url) {
        peers.upsert(info);
        return Ok(());
    }
    adm.admit_new_url_from(source)?;
    let base_url = info.base_url.clone();
    match admit_promoted(peers, info, adm.cfg.max_known_peers) {
        Ok(_) => Ok(()),
        Err(TableFull) => {
            tracing::warn!(
                event = "peer_table_full",
                peer = %base_url,
                "peer table full and every entry is active or bootstrap; announce refused",
            );
            Err(AdmitError::TableFull.into())
        }
    }
}

/// Keeps the announced libp2p identity only when the URL's own server reported the same id
/// (and marks it bound); otherwise blanks it, since the announcer cannot vouch for another id.
fn bind_or_blank(info: &mut PeerInfo, reported: Option<String>) {
    if info.libp2p_peer_id.is_some() && info.libp2p_peer_id == reported {
        info.identity_bound = true;
        return;
    }
    info.identity_bound = false;
    info.libp2p_peer_id = None;
    info.libp2p_listen_addrs = Vec::new();
    info.connectivity = None;
}

/// An announce for a known URL naming a libp2p id that differs from the stored one is checked
/// against the URL's own status answer; on any failure the stored identity stays (the table
/// keeps it when a write is unbound).
async fn rebind_existing(
    peers: &PeerTable,
    network_id: &str,
    adm: &PeerAdmission,
    source: IpAddr,
    mut info: PeerInfo,
) -> PeerInfo {
    let stored = peers
        .list_all()
        .into_iter()
        .find(|p| p.base_url == info.base_url);
    let differs = match (&stored, &info.libp2p_peer_id) {
        (Some(s), Some(id)) => s.identity_bound && s.libp2p_peer_id.as_ref() != Some(id),
        _ => false,
    };
    if !differs || !adm.cfg.verify_reachability {
        return info;
    }
    let verified = async {
        adm.admit_new_url_from(source).ok()?;
        let _permit = adm.enter_check().ok()?;
        let checked = adm.check_address(&info.base_url).await.ok()?;
        adm.verify_reachable(&checked, network_id).await.ok()
    }
    .await;
    match verified {
        Some(reported) if reported.is_some() && reported == info.libp2p_peer_id => {
            info.identity_bound = true;
        }
        _ => {}
    }
    info
}

/// Inserts `info` into the main table and, on success, clears the same base
/// URL from the unverified pool — a peer that announces itself directly is
/// confirmed, whether or not gossip had already relayed it into the pool.
fn admit_promoted(
    peers: &PeerTable,
    info: PeerInfo,
    max: usize,
) -> Result<Option<String>, TableFull> {
    let base_url = info.base_url.clone();
    let result = peers.insert_bounded(info, max);
    if result.is_ok() {
        peers.take_unverified(&base_url);
    }
    result
}

/// Promotes `contacted` from the unverified pool into the main table after a
/// successful outbound contact — the pool holds only entries this node has
/// never confirmed itself, so a successful request to one is exactly that
/// confirmation. A no-op when `contacted` isn't (or is no longer) in the
/// pool, including when it was already promoted.
pub(crate) fn promote_on_contact(peers: &PeerTable, adm: &PeerAdmission, contacted: &str) {
    let Some(mut info) = peers.take_unverified(contacted) else {
        return;
    };
    // Reaching `p2p://<id>` means the handshake authenticated that id.
    if let Some(id) = crate::node_http::parse_p2p_base(contacted) {
        bind_if_p2p_url_names_peer(&mut info, &id.to_string());
        if peers.contains(contacted) {
            return;
        }
    }
    match peers.insert_bounded(info, adm.cfg.max_known_peers) {
        Ok(evicted) => {
            if let Some(evicted) = evicted {
                tracing::info!(
                    event = "peer_evicted_for_capacity",
                    evicted = %evicted,
                    admitted = %contacted,
                    "peer table full: evicted the oldest inactive entry",
                );
            }
            tracing::info!(
                event = "peer_promoted_from_unverified_pool",
                peer = %contacted,
                "promoted a gossip-learned peer into the peer table after a successful \
                 outbound contact",
            );
        }
        Err(TableFull) => {
            tracing::warn!(
                event = "peer_table_full",
                peer = %contacted,
                "peer table full; could not promote a contacted gossip-learned peer",
            );
        }
    }
}

/// This node's own peer table entry, as returned in an announce response.
fn own_peer_info(
    base_url: &str,
    libp2p_peer_id: Option<String>,
    reachability: &crate::reachability::ReachabilityHandle,
    network_id: &str,
    now: OffsetDateTime,
) -> PeerInfo {
    let libp2p_listen_addrs = if libp2p_peer_id.is_some() {
        reachability.advertised_addrs()
    } else {
        Vec::new()
    };
    PeerInfo {
        base_url: base_url.to_string(),
        roles: node_roles(),
        protocol_version: crate::version::PROTOCOL_VERSION.to_string(),
        network_id: network_id.to_string(),
        last_announced_at: now,
        libp2p_peer_id,
        libp2p_listen_addrs,
        witness: None,
        connectivity: crate::reachability::connectivity_for(&reachability.snapshot()),
        identity_bound: false,
    }
}

/// Stores the responder's own entry from an announce response, only if it names `called_url`
/// itself on this network at a supported version. Goes through `insert_bounded`, so the table
/// cap and its protected entries hold. Returns whether an entry was stored.
fn admit_responder_node(
    peers: &PeerTable,
    adm: &PeerAdmission,
    network_id: &str,
    called_url: &str,
    mut node: PeerInfo,
) -> bool {
    let called = normalized_base_url(called_url);
    let shape_ok = if called.starts_with("p2p://") {
        self_consistent_p2p_base(&node.base_url, node.libp2p_peer_id.as_deref()).as_deref()
            == Some(called.as_str())
    } else {
        normalized_base_url(&node.base_url) == called && adm.check_shape(&node.base_url).is_ok()
    };
    if node.network_id != network_id
        || !shape_ok
        || !crate::version::is_supported(&node.protocol_version)
    {
        tracing::warn!(
            event = "responder_identity_rejected",
            peer = %called,
            "ignored a responder's own entry that did not match the URL called",
        );
        return false;
    }
    node.base_url = called;
    node.libp2p_listen_addrs = sanitize_libp2p_addrs(
        node.libp2p_peer_id.as_deref(),
        &node.libp2p_listen_addrs,
        &adm.policy,
    );
    node.last_announced_at = OffsetDateTime::now_utc();
    node.witness = None;
    // The entry came from the URL's own server over the connection we made to it.
    node.identity_bound = node.libp2p_peer_id.is_some();
    match peers.insert_bounded(node, adm.cfg.max_known_peers) {
        Ok(_) => true,
        Err(TableFull) => {
            tracing::warn!(
                event = "peer_table_full",
                peer = %called_url,
                "peer table full; could not store a responder's own entry",
            );
            false
        }
    }
}

/// The normalized `p2p://<id>` base URL when `base_url` is one and `libp2p_peer_id` names the
/// same id; `None` for anything else, since an entry cannot vouch for an id other than its own.
fn self_consistent_p2p_base(base_url: &str, libp2p_peer_id: Option<&str>) -> Option<String> {
    let id = crate::node_http::parse_p2p_base(base_url)?;
    (libp2p_peer_id == Some(id.to_string().as_str())).then(|| crate::node_http::p2p_base_url(&id))
}

/// Counts of gossip entries skipped by [`merge_gossip`].
#[derive(Debug, Default, PartialEq, Eq)]
struct GossipSkipped {
    rejected: usize,
    over_limit: usize,
}

/// Merges the peers a neighbor returned. Entries already in the main table
/// are refreshed there; unknown ones are shape- and address-checked (no
/// reachability contact) and, at most `max_new_per_exchange` of them, land in
/// the unverified pool, never the main table — gossip alone never admits a
/// peer this node hasn't confirmed itself. Returns the entries admitted
/// (refreshed in the table, or newly placed in the pool) plus the skip
/// counts.
async fn merge_gossip(
    peers: &PeerTable,
    adm: &PeerAdmission,
    network_id: &str,
    incoming: Vec<PeerInfo>,
) -> (Vec<PeerInfo>, GossipSkipped) {
    let now = OffsetDateTime::now_utc();
    let cap = adm.cfg.max_new_per_exchange;
    let examine_limit = cap.saturating_mul(4);
    let (mut examined, mut accepted) = (0usize, 0usize);
    let mut admitted = Vec::new();
    let mut skipped = GossipSkipped::default();
    for mut info in incoming {
        if info.network_id != network_id {
            skipped.rejected += 1;
            continue;
        }
        let is_p2p = info.base_url.trim().starts_with("p2p://");
        if is_p2p {
            let Some(base) =
                self_consistent_p2p_base(&info.base_url, info.libp2p_peer_id.as_deref())
            else {
                skipped.rejected += 1;
                continue;
            };
            info.base_url = base;
        } else {
            let Ok(base) = adm.check_shape(&info.base_url) else {
                skipped.rejected += 1;
                continue;
            };
            info.base_url = base;
        }
        info.libp2p_listen_addrs = sanitize_libp2p_addrs(
            info.libp2p_peer_id.as_deref(),
            &info.libp2p_listen_addrs,
            &adm.policy,
        );
        info.last_announced_at = info.last_announced_at.min(now);
        info.identity_bound = false;
        info.witness = verified_advert(&info.base_url, info.witness.take(), now);
        if !crate::version::is_supported(&info.protocol_version) {
            peers.admit_if_supported(info);
            continue;
        }
        if peers.contains(&info.base_url) {
            // Only the peer's own announce may change a `p2p://` entry.
            if !is_p2p {
                peers.upsert(info.clone());
            }
            admitted.push(info);
            continue;
        }
        if accepted >= cap || examined >= examine_limit {
            skipped.over_limit += 1;
            continue;
        }
        examined += 1;
        // A p2p:// entry has no address to check; a libp2p contact promotes and binds it.
        if !is_p2p && adm.check_address(&info.base_url).await.is_err() {
            skipped.rejected += 1;
            continue;
        }
        peers.insert_unverified(info.clone());
        accepted += 1;
        admitted.push(info);
    }
    (admitted, skipped)
}

/// Keeps the shard entries fit for the registry: known `(shard, url)` pairs
/// as they are, unseen URLs only after the shape and address checks, with at
/// most `max_new_shard_urls_per_exchange` unseen URLs examined. Returns the
/// entries to merge and how many were refused.
async fn validated_shards(
    adm: &PeerAdmission,
    registry: &ShardRegistry,
    incoming: &[ShardAnnouncement],
) -> (Vec<ShardAnnouncement>, usize) {
    let now = OffsetDateTime::now_utc();
    let mut kept = Vec::new();
    let (mut examined, mut refused, mut p2p_new) = (0usize, 0usize, 0usize);
    for entry in incoming {
        let mut entry = entry.clone();
        entry.last_seen_at = entry.last_seen_at.min(now);
        if !adm.shard_id_ok(&entry.shard_id) {
            refused += 1;
            continue;
        }
        if registry.has_url(&entry.shard_id, &entry.url) {
            kept.push(entry);
            continue;
        }
        if examined >= adm.cfg.max_new_shard_urls_per_exchange {
            refused += 1;
            continue;
        }
        examined += 1;
        if let Some(id) = crate::node_http::parse_p2p_base(&entry.url) {
            p2p_new += 1;
            if p2p_new > MAX_P2P_SHARD_URLS_PER_EXCHANGE {
                refused += 1;
                continue;
            }
            entry.url = crate::node_http::p2p_base_url(&id);
            kept.push(entry);
            continue;
        }
        let Ok(base) = adm.check_shape(&entry.url) else {
            refused += 1;
            continue;
        };
        if adm.check_address(&base).await.is_err() {
            refused += 1;
            continue;
        }
        entry.url = base;
        kept.push(entry);
    }
    (kept, refused)
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
    /// This node's own libp2p peer id, so a caller can bind it to this URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub libp2p_peer_id: Option<String>,
    pub protocol_version: String,
    pub network_id: String,
    /// This node's own `AVALON_NODE_ROLES` ([`node_roles`]) — `combined`
    /// reported as-is, not expanded, matching
    /// [`AnnounceRequest::roles`]/[`PeerInfo::roles`].
    pub roles: Vec<String>,
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
    /// `Some(true)` when this node authors `core` with its network's pinned
    /// settlement key, `Some(false)` when it authors `core` with a different
    /// key (tolerated only for a lone `local-dev` node), `None` when it does
    /// not author `core` or its network has no pinned key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub core_author_pinned: Option<bool>,
    /// How this node is reachable, derived from `reachability`; omitted while it is `unknown`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connectivity: Option<avalon_protocol::connectivity::Connectivity>,
    /// What AutoNAT detected about whether peers can dial this node.
    pub reachability: crate::reachability::Reachability,
    /// Addresses a peer confirmed by dialing them back.
    pub confirmed_external_addrs: Vec<String>,
    /// Accepted relay reservations this node holds while `private`.
    pub relay_reservations: Vec<crate::reachability::RelayReservation>,
    /// Addresses peers can dial to reach this node through its relays.
    pub relayed_listen_addrs: Vec<String>,
    /// Recent hole-punch attempts, oldest first; a failed one leaves the relayed connection in use.
    pub hole_punches: Vec<crate::reachability::HolePunchOutcome>,
    /// Peers this node holds a hole-punched direct connection to right now.
    pub punched_peers: Vec<String>,
    /// Limits and live usage of the relay server role; omitted when this node does not relay.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_server: Option<crate::relay::RelayServerStatus>,
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
    Json(build_status(&state))
}

/// Shared by `status` and `discover` so both build the exact same
/// `NodeStatusResponse` from the same `AppState`.
pub(crate) fn build_status(state: &AppState) -> NodeStatusResponse {
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
        eligible_for_new_registrations: !crate::replica::is_replica_only()
            && (within_grace_period
                || confirmed_mirror_count >= state.replication_gate.min_confirmations),
    };

    let detected = state.reachability.snapshot();
    NodeStatusResponse {
        libp2p_peer_id: state.own_libp2p_peer_id.clone(),
        protocol_version: crate::version::PROTOCOL_VERSION.to_string(),
        network_id: state.chain.network_id().to_string(),
        roles: node_roles(),
        stale,
        newest_known_peer_version: newest_known_peer_version.map(|v| v.to_string()),
        resources,
        own_shard_replication,
        core_author_pinned: crate::core_author_guard::recorded_outcome(),
        connectivity: crate::reachability::connectivity_for(&detected),
        reachability: detected.reachability,
        confirmed_external_addrs: detected.confirmed_addrs,
        relayed_listen_addrs: detected
            .relay_reservations
            .iter()
            .map(|r| r.relayed_addr.clone())
            .collect(),
        relay_reservations: detected.relay_reservations,
        hole_punches: detected.hole_punches,
        punched_peers: detected.punched_peers,
        relay_server: state.reachability.relay_server_status(),
    }
}

/// Response shape for `GET /nodes/discover` — a verified node's own status
/// alongside its full peer table, so an SDK that has reached exactly one
/// node can expand its candidate pool in a single round trip instead of a
/// separate `GET /nodes/peers` call.
#[derive(Debug, Serialize)]
pub struct DiscoverResponse {
    pub self_status: NodeStatusResponse,
    pub peers: Vec<PeerInfo>,
}

/// `GET /nodes/discover` — read-only, same public posture as `list_peers`/
/// `status`. Combines both into one response for a caller that only needs
/// a single request to go from "one verified node" to "a full candidate
/// pool." Not ranked by latency or health — just this node's current view
/// of its own status and peer table.
pub async fn discover(State(state): State<AppState>) -> Json<DiscoverResponse> {
    Json(DiscoverResponse {
        self_status: build_status(&state),
        peers: state.peers.list_all(),
    })
}

/// This node's own libp2p DHT identity, computed once at
/// startup by `crate::dht::start` — `None` when `AVALON_DHT_ENABLED` isn't
/// set, in which case this node announces exactly as it did before DHT
/// identity existed. Threaded into [`run_worker`] so every outbound announce also
/// tells peers how to find this node in the DHT, the same way `roles`/
/// `protocol_version` already do for the HTTP peer table.
#[derive(Debug, Clone)]
pub struct DhtIdentity {
    pub peer_id: String,
    /// Source of the addresses announced with this identity.
    pub reachability: crate::reachability::ReachabilityHandle,
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
    /// This node's own announced identity: `AVALON_NODE_URL`, else `p2p://<peer id>` when libp2p
    /// runs ([`Self::with_p2p_fallback`]). `None` means this node can still be announced TO, it
    /// just can't announce itself anywhere.
    pub own_base_url: Option<String>,
    /// `AVALON_NODE_MAX_PEERS` (Layer 1) — the cap on this
    /// node's *active* announce/exchange set (bootstrap peers plus
    /// peers promoted from what's been discovered through them). Bounds
    /// this node's own direct-connection count regardless of how large the
    /// network as a whole grows, the same role Kademlia's k-bucket size or
    /// a gossip-membership protocol's fanout limit plays. Bootstrap peers
    /// are never evicted to make room — see [`run_worker`]'s own doc
    /// comment.
    pub max_peers: usize,
    /// This node's witness identity, when it cosigns; advertised in announces.
    pub witness: Option<WitnessSigner>,
}

/// Upper bound on one announce round trip; a slower peer counts as loss.
const ANNOUNCE_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_ANNOUNCE_INTERVAL_SECS: u64 = 180;
/// A peer not re-announced within this many multiples of the announce
/// interval is pruned — generous enough that one or two missed ticks
/// (a transient network blip) never evicts a genuinely live peer.
const PRUNE_INTERVAL_MULTIPLE: u32 = 3;
/// Default for `AVALON_NODE_MAX_PEERS` — generous enough that a real
/// small-to-mid-size deployment never bumps into it in practice, but still
/// a real, enforced bound rather than "unlimited" — a node's direct-connection
/// count must stay bounded regardless of network size.
const DEFAULT_MAX_PEERS: usize = 50;

/// Pure resolution logic, split out for direct unit testing (same "pure
/// function behind the env-reading wrapper" pattern `registry::coarsen`
/// already uses in this repo) — no real `bundled_trust_anchors()` call, so
/// a test can feed
/// it a controlled anchor list instead of depending on
/// `docs/trusted-networks.json`'s actual (currently empty) `seed_nodes`.
///
/// `pub`: `avalon-cli`'s `discover-mirror-peers` command
/// reuses this exact same resolution — `AVALON_BOOTSTRAP_PEERS` verbatim,
/// or this network's seed nodes — rather than re-implementing it, so the
/// two never drift apart on what "the configured bootstrap peers" means.
pub fn resolve_bootstrap_peers(
    bootstrap_env: Option<&str>,
    network_id: &str,
    anchors: &[avalon_protocol::network_trust::TrustAnchorEntry],
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
            avalon_protocol::network_trust::bundled_trust_anchors(),
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
            witness: None,
        }
    }
}

impl AnnounceConfig {
    /// The http(s) URL only: `None` when this node announces as `p2p://<id>`.
    pub fn own_http_base_url(&self) -> Option<String> {
        self.own_base_url
            .clone()
            .filter(|u| !u.starts_with("p2p://"))
    }

    /// Without `AVALON_NODE_URL`, a node with a libp2p identity announces itself as
    /// `p2p://<peer id>`, reachable only over an authenticated stream.
    pub fn with_p2p_fallback(mut self, libp2p_peer_id: Option<&str>) -> Self {
        if self.own_base_url.is_none() {
            self.own_base_url = libp2p_peer_id
                .and_then(|id| id.parse::<libp2p::PeerId>().ok())
                .map(|id| crate::node_http::p2p_base_url(&id));
        }
        self
    }
}

/// `AVALON_NODE_ROLES` — comma-separated (matching `docs/projects/backend-server/architecture/nodes.md`'s
/// `Settlement`/`Indexer`/`Realtime`/`Gateway` capability names), defaulting
/// to `combined` — milestone 1's "one `avalon-server` process" reality, per
/// that doc's own capability table.
///
/// This used to be purely advisory (peer-table bookkeeping and the
/// realtime relay routing only) — `main.rs` now also reads it to decide
/// what actually gets wired up at startup: whether to run local presence/
/// WebSocket handling at all (see [`role_included`]/
/// [`realtime_mode_from_env`]), whether to construct a local
/// `PostgresIndexer` or a `RemoteIndexer` (see
/// [`indexer_role_is_local`]), and whether this process is a genuinely
/// standalone Settlement node (see [`is_settlement_only`]).
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
    // Issue #665: shared normalization/validation, same helper
    // `internal_role::RemoteIndexer::from_env` now uses for
    // `AVALON_INDEXER_REMOTE_URL` — see `crate::backing_services`'s own
    // module doc comment for why this part (trim, strip trailing slash,
    // well-formed check) is unified across all three backing-service vars
    // while the required-vs-optional question above stays this function's
    // own.
    crate::backing_services::normalize_and_validate_url("AVALON_REALTIME_URL", &raw).map(Some)
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
/// `combined` means "every role" (the table in `docs/projects/backend-server/architecture/nodes.md`'s
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
/// own outbox writes elsewhere. See `docs/projects/backend-server/architecture/nodes.md`'s "Today in
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
#[allow(clippy::too_many_arguments)]
pub async fn run_worker(
    chain: PostgresSettlementProvider,
    peers: PeerTable,
    shard_registry: ShardRegistry,
    head_gossip: HeadGossipTracker,
    known_list: crate::known_list::KnownListHandle,
    own_shard_id: String,
    config: AnnounceConfig,
    dht_identity: Option<DhtIdentity>,
) {
    let witness_signer = config.witness.clone();
    if config.peers.is_empty() {
        tracing::info!(
            "node-announce: no bootstrap/seed peers configured — this node can still be \
             announced to via POST /nodes/announce, but won't announce itself anywhere"
        );
    } else if config.own_base_url.is_none() {
        tracing::warn!(
            "node-announce: {} peer(s) configured but neither AVALON_NODE_URL nor a libp2p \
             identity (AVALON_DHT_ENABLED) is set — this node has nothing to announce itself as",
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

    let client = crate::node_http::NodeClient::from(
        reqwest::Client::builder()
            .timeout(ANNOUNCE_TIMEOUT)
            .build()
            .unwrap_or_default(),
    )
    .with_timeout(ANNOUNCE_TIMEOUT);
    let roles = node_roles();
    let network_id = chain.network_id().to_string();
    let mut active_peers: Vec<String> = config.peers.clone();

    let neighbors = peers.neighbors().clone();
    neighbors.set_own_libp2p_peer_id(dht_identity.as_ref().map(|d| d.peer_id.clone()));

    let mut vouch_cursor = 0usize;
    loop {
        neighbors.set_active(&active_peers, &config.peers);
        // Issue #599, Layer 2: before announcing, refresh this node's own
        // authoritative claim (if it has one) so it's part of the
        // snapshot gossiped out this tick. "Authoritative" here means this
        // node actually has local, signed settlement history for
        // `own_shard_id` — a pure mirror with no local writes of its own
        // has nothing to claim authority over and gossips only what it's
        // learned from others.
        if let Some(own_base_url) = &config.own_base_url {
            if let Ok(Some(sth)) = chain.latest_signed_tree_head().await {
                shard_registry.record_own(&own_shard_id, own_base_url, OffsetDateTime::now_utc());
                // This node's own current head joins the
                // snapshot gossiped out this tick too — see
                // `HeadGossipTracker::record_own`'s own doc comment.
                let cosignature_count = chain
                    .list_witness_cosignatures(&sth.network_id, &own_shard_id, sth.tree_size)
                    .await
                    .map(|c| c.len())
                    .unwrap_or(0);
                head_gossip.record_own(
                    HeadSummary {
                        shard_id: own_shard_id.clone(),
                        tree_size: sth.tree_size,
                        root_hash: sth.root_hash,
                        cosignature_count,
                    },
                    own_base_url,
                    OffsetDateTime::now_utc(),
                );
            }
        }

        if let Some(own_base_url) = &config.own_base_url {
            let targets = active_peers.clone();
            for peer in &targets {
                match measured(
                    &neighbors,
                    peer,
                    announce_to_peer(
                        &client,
                        &peers,
                        peer,
                        own_base_url,
                        &announce_request(
                            own_base_url,
                            &roles,
                            &network_id,
                            dht_identity.as_ref(),
                            &shard_registry.snapshot(),
                            &head_gossip.snapshot(),
                            witness_signer
                                .as_ref()
                                .map(|w| w.advert(own_base_url, OffsetDateTime::now_utc())),
                            neighbors.own_coordinate(),
                        ),
                    ),
                )
                .await
                {
                    Ok(((discovered, hello), rtt)) => {
                        let adm = admission();
                        record_contact(&neighbors, &peers, adm, peer, &discovered, rtt, hello);
                        if let Some(node) = discovered.node {
                            admit_responder_node(&peers, adm, &network_id, peer, node);
                        }
                        if let Some(advert) = verified_advert(
                            &normalized_base_url(peer),
                            discovered.witness.clone().filter(|_| !hello),
                            OffsetDateTime::now_utc(),
                        ) {
                            let advert = WitnessAdvert {
                                direct: true,
                                ..advert
                            };
                            peers.attach_witness(&normalized_base_url(peer), advert);
                        }
                        let (admitted, skipped) =
                            merge_gossip(&peers, adm, &network_id, discovered.peers).await;
                        if skipped != GossipSkipped::default() {
                            tracing::warn!(
                                event = "gossip_peers_skipped",
                                via = %peer,
                                rejected = skipped.rejected,
                                over_limit = skipped.over_limit,
                                "skipped gossiped peers that failed validation or exceeded \
                                 the per-exchange limit",
                            );
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

                        let (shards, rejected_shards) =
                            validated_shards(adm, &shard_registry, &discovered.known_shards).await;
                        if rejected_shards > 0 {
                            tracing::warn!(
                                event = "shard_gossip_entries_rejected",
                                via = %peer,
                                rejected = rejected_shards,
                                "skipped shard entries that failed address validation or limits",
                            );
                        }
                        let newly_learned = shard_registry.merge(&shards);
                        for shard_id in &newly_learned {
                            tracing::info!(
                                event = "shard_discovered",
                                shard_id = %shard_id,
                                from_peer = %peer,
                                "learned of a new shard via peer-announce gossip",
                            );
                        }

                        // Head-summary gossip, same bounded/
                        // validated merge the announce handler runs for an
                        // inbound exchange — see `HeadGossipTracker::merge`.
                        let (_admitted_heads, conflicts, rejected_heads) = head_gossip.merge(
                            peer,
                            &discovered.head_summaries,
                            OffsetDateTime::now_utc(),
                        );
                        if rejected_heads > 0 {
                            tracing::warn!(
                                event = "head_summary_gossip_entries_rejected",
                                via = %peer,
                                rejected = rejected_heads,
                                "skipped head-summary entries that failed validation or \
                                 exceeded the per-exchange limit",
                            );
                        }
                        for conflict in conflicts {
                            tracing::warn!(
                                event = "head_summary_conflict_detected",
                                shard_id = %conflict.shard_id,
                                tree_size = conflict.tree_size,
                                root_hash_a = %conflict.root_hash_a,
                                root_hash_b = %conflict.root_hash_b,
                                "two different roots reported for the same shard/tree_size via \
                                 gossip — fetching full cosignature detail to confirm",
                            );
                            let chain = chain.clone();
                            let head_gossip = head_gossip.clone();
                            let peers = peers.clone();
                            let known_list = known_list.clone();
                            tokio::spawn(async move {
                                crate::equivocation::confirm_and_record(
                                    &chain,
                                    &head_gossip,
                                    &peers,
                                    &known_list,
                                    conflict,
                                )
                                .await;
                            });
                        }
                    }
                    Err(err) => tracing::error!("node-announce: {peer}: {err}"),
                }
            }
        }

        if let Some(own_base_url) = &config.own_base_url {
            let targets =
                pick_vouch_targets(&peers, &active_peers, vouch_cursor, VOUCH_CONTACTS_PER_TICK);
            vouch_cursor = vouch_cursor.wrapping_add(VOUCH_CONTACTS_PER_TICK);
            if !targets.is_empty() {
                let request = announce_request(
                    own_base_url,
                    &roles,
                    &network_id,
                    dht_identity.as_ref(),
                    &shard_registry.snapshot(),
                    &head_gossip.snapshot(),
                    witness_signer
                        .as_ref()
                        .map(|w| w.advert(own_base_url, OffsetDateTime::now_utc())),
                    neighbors.own_coordinate(),
                );
                for peer in &targets {
                    let target = announce_target(&peers, peer, own_base_url);
                    vouch_contact(&client, &peers, admission(), peer, &target, &request).await;
                }
            }
        }

        let cutoff = OffsetDateTime::now_utc() - config.interval * PRUNE_INTERVAL_MULTIPLE;
        peers.prune_older_than(cutoff);
        peers.prune_unverified_older_than(cutoff);
        shard_registry.prune_older_than(cutoff);

        // Includes the unverified pool: a peer only just gossiped in has to
        // survive at least one more announce cycle to get the outbound
        // contact that would promote it, or it would never get the chance.
        let known_base_urls: HashSet<String> = peers
            .list_all()
            .into_iter()
            .chain(peers.list_unverified())
            .map(|p| p.base_url)
            .collect();
        retain_reachable_active_peers(&mut active_peers, &config.peers, &known_base_urls);
        neighbors.set_active(&active_peers, &config.peers);

        tokio::time::sleep(config.interval).await;
    }
}

/// Where to send `peer`'s announce. A node announcing as `p2p://<id>` must reach a peer over a
/// stream, the only way the peer can authenticate it; until `peer`'s libp2p id is known this
/// falls back to HTTP, where a `p2p://` announce is answered but never stored.
fn announce_target(peers: &PeerTable, peer: &str, own_base_url: &str) -> String {
    if crate::node_http::parse_p2p_base(own_base_url).is_some() {
        if let Some(url) = peers.stream_url(peer) {
            return url;
        }
    }
    peers.transport_url(peer)
}

/// Announces to `peer` at [`announce_target`]. A `p2p://` announcer whose stream attempt fails
/// retries over the peer's own URL, which keeps the peer's id and addresses fresh. The flag is
/// true when the announce went over HTTP and so admitted nothing.
async fn announce_to_peer(
    client: &crate::node_http::NodeClient,
    peers: &PeerTable,
    peer: &str,
    own_base_url: &str,
    request: &AnnounceRequest,
) -> Result<(AnnounceResponse, bool), String> {
    let target = announce_target(peers, peer, own_base_url);
    let own_is_p2p = crate::node_http::parse_p2p_base(own_base_url).is_some();
    // Over HTTP a `p2p://` announce is only answered, never admitted.
    let hello = own_is_p2p && !target.starts_with("p2p://");
    let first = announce_to(client, &target, request).await;
    if first.is_ok() || !own_is_p2p || target == peer {
        return first.map(|r| (r, hello));
    }
    announce_to(client, peer, request).await.map(|r| (r, true))
}

/// What a successful announce to `peer` counts for: only one the peer admitted (not an HTTP
/// `p2p://` hello) feeds the coordinate and promotes a pooled entry.
fn record_contact(
    neighbors: &crate::neighbors::NeighborTable,
    peers: &PeerTable,
    adm: &PeerAdmission,
    peer: &str,
    response: &AnnounceResponse,
    rtt: Duration,
    hello: bool,
) {
    if hello {
        return;
    }
    neighbors.observe_coordinate(peer, &response.coordinate, rtt);
    promote_on_contact(peers, adm, peer);
}

/// Peers with only a non-direct witness advert contacted per worker tick.
const VOUCH_CONTACTS_PER_TICK: usize = 5;

/// Up to `limit` peers holding a non-direct advert and no direct one, not in
/// `skip`, taken in base-URL order starting at `cursor` (wrapping) so every
/// such peer gets a turn.
fn pick_vouch_targets(
    peers: &PeerTable,
    skip: &[String],
    cursor: usize,
    limit: usize,
) -> Vec<String> {
    let mut urls: Vec<String> = peers
        .list_all()
        .into_iter()
        .filter(|p| p.witness.as_ref().is_some_and(|w| !w.direct) && !skip.contains(&p.base_url))
        .map(|p| p.base_url)
        .collect();
    urls.sort();
    if urls.is_empty() {
        return urls;
    }
    let shift = cursor % urls.len();
    urls.rotate_left(shift);
    urls.truncate(limit);
    urls
}

/// Announces to `peer` only to obtain its own witness advert from the
/// response; the peer is not added to the active set or the neighbors table
/// and nothing else in the response is merged. Only the URL's own response
/// can create a direct advert.
async fn vouch_contact(
    client: &crate::node_http::NodeClient,
    peers: &PeerTable,
    adm: &PeerAdmission,
    peer: &str,
    target: &str,
    request: &AnnounceRequest,
) {
    if !peer.starts_with("p2p://") && adm.check_address(peer).await.is_err() {
        return;
    }
    // A `p2p://` announce over HTTP admits nothing, so its answer vouches for nothing.
    if request.base_url.starts_with("p2p://") && !target.starts_with("p2p://") {
        return;
    }
    let Ok(response) = announce_to(client, target, request).await else {
        return;
    };
    if let Some(advert) = verified_advert(
        &normalized_base_url(peer),
        response.witness,
        OffsetDateTime::now_utc(),
    ) {
        peers.attach_witness(
            &normalized_base_url(peer),
            WitnessAdvert {
                direct: true,
                ..advert
            },
        );
    }
}

/// Awaits one announce and records its round trip (request to parsed
/// response) on success or a loss on failure; returns the round trip with the value.
async fn measured<T>(
    neighbors: &crate::neighbors::NeighborTable,
    peer: &str,
    announce: impl std::future::Future<Output = Result<T, String>>,
) -> Result<(T, Duration), String> {
    let started = std::time::Instant::now();
    let result = announce.await;
    let rtt = started.elapsed();
    match result {
        Ok(value) => {
            neighbors.record_success(peer, rtt);
            Ok((value, rtt))
        }
        Err(err) => {
            neighbors.record_failure(peer);
            Err(err)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn announce_request(
    own_base_url: &str,
    roles: &[String],
    network_id: &str,
    dht_identity: Option<&DhtIdentity>,
    known_shards: &[ShardAnnouncement],
    head_summaries: &[HeadSummary],
    witness: Option<WitnessAdvert>,
    coordinate: Coordinate,
) -> AnnounceRequest {
    AnnounceRequest {
        base_url: own_base_url.to_string(),
        roles: roles.to_vec(),
        protocol_version: crate::version::PROTOCOL_VERSION.to_string(),
        network_id: network_id.to_string(),
        libp2p_peer_id: dht_identity.map(|d| d.peer_id.clone()),
        libp2p_listen_addrs: dht_identity
            .map(|d| d.reachability.advertised_addrs())
            .unwrap_or_default(),
        known_shards: known_shards.to_vec(),
        head_summaries: head_summaries.to_vec(),
        witness,
        coordinate,
        connectivity: dht_identity
            .and_then(|d| crate::reachability::connectivity_for(&d.reachability.snapshot())),
    }
}

async fn announce_to(
    client: &crate::node_http::NodeClient,
    peer_base_url: &str,
    request: &AnnounceRequest,
) -> Result<AnnounceResponse, String> {
    let response = client
        .post(format!("{peer_base_url}/nodes/announce"))
        .json(request)
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
    use libp2p::PeerId;

    fn peer(base_url: &str, announced_at: OffsetDateTime) -> PeerInfo {
        PeerInfo {
            identity_bound: false,
            base_url: base_url.to_string(),
            roles: vec!["combined".to_string()],
            protocol_version: "0.1.0".to_string(),
            network_id: "avalon-dev-local".to_string(),
            last_announced_at: announced_at,
            libp2p_peer_id: None,
            libp2p_listen_addrs: Vec::new(),
            connectivity: None,
            witness: None,
        }
    }

    #[tokio::test]
    async fn announce_records_round_trip_and_loss_separately() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/nodes/announce"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(30))
                    .set_body_json(
                        serde_json::json!({"peers": [], "known_shards": [], "coordinate": Coordinate::default()}),
                    ),
            )
            .mount(&server)
            .await;
        let up = server.uri();
        let down = "http://127.0.0.1:1".to_string();

        let neighbors = crate::neighbors::NeighborTable::new();
        neighbors.set_active(&[up.clone(), down.clone()], &[]);
        let client = crate::node_http::NodeClient::new();
        for peer in [&up, &down] {
            let _ = measured(
                &neighbors,
                peer,
                announce_to(
                    &client,
                    peer,
                    &announce_request(
                        "http://me",
                        &[],
                        "n",
                        None,
                        &[],
                        &[],
                        None,
                        Coordinate::default(),
                    ),
                ),
            )
            .await;
        }

        let snap = neighbors.snapshot();
        let up_stats = snap[0].round_trip.clone().unwrap();
        assert!(up_stats.last_ms.unwrap() >= 30.0);
        assert_eq!(up_stats.samples, 1);
        assert_eq!(up_stats.failed_recent, 0);
        let down_stats = snap[1].round_trip.clone().unwrap();
        assert_eq!(down_stats.samples, 0);
        assert!(down_stats.last_ms.is_none());
        assert_eq!(down_stats.loss_ratio, 1.0);
    }

    #[tokio::test]
    async fn announce_exchanges_coordinates_and_moves_the_local_one() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let remote = Coordinate {
            vector: [10.0, 0.0, 0.0],
            height: 1.0,
            error: 0.5,
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/nodes/announce"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"peers": [], "known_shards": [], "coordinate": remote}),
            ))
            .mount(&server)
            .await;
        let peer = server.uri();
        let neighbors = crate::neighbors::NeighborTable::new();
        neighbors.set_active(std::slice::from_ref(&peer), &[]);
        let client = crate::node_http::NodeClient::new();
        let start = neighbors.own_coordinate();

        for _ in 0..2 {
            let (response, rtt) = measured(
                &neighbors,
                &peer,
                announce_to(
                    &client,
                    &peer,
                    &announce_request(
                        "http://me",
                        &[],
                        "n",
                        None,
                        &[],
                        &[],
                        None,
                        neighbors.own_coordinate(),
                    ),
                ),
            )
            .await
            .unwrap();
            assert_eq!(response.coordinate, remote);
            neighbors.observe_coordinate(&peer, &response.coordinate, rtt);
        }

        assert_ne!(neighbors.own_coordinate(), start);
        assert_eq!(neighbors.snapshot()[0].coordinate, Some(remote));
        let sent: Vec<AnnounceRequest> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect();
        assert_eq!(sent[0].coordinate, start);
        assert_ne!(sent[1].coordinate, start);
    }

    #[test]
    fn announce_bodies_without_a_coordinate_do_not_decode() {
        let req =
            r#"{"base_url":"http://a","roles":[],"protocol_version":"0.1.0","network_id":"n"}"#;
        assert!(serde_json::from_str::<AnnounceRequest>(req).is_err());
        assert!(serde_json::from_str::<AnnounceResponse>(r#"{"peers":[]}"#).is_err());
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
            identity_bound: false,
            base_url: base_url.to_string(),
            roles: vec!["combined".to_string()],
            protocol_version: protocol_version.to_string(),
            network_id: "avalon-dev-local".to_string(),
            last_announced_at: OffsetDateTime::now_utc(),
            libp2p_peer_id: None,
            libp2p_listen_addrs: Vec::new(),
            connectivity: None,
            witness: None,
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

    fn anchor(
        network_id: &str,
        seed_nodes: Vec<String>,
    ) -> avalon_protocol::network_trust::TrustAnchorEntry {
        avalon_protocol::network_trust::TrustAnchorEntry {
            label: network_id.to_string(),
            network_id: network_id.to_string(),
            verify_key: "ab".repeat(32),
            signing_key_id: "test-key".to_string(),
            server_url: None,
            environment: avalon_protocol::network_trust::NetworkEnvironment::LocalDev,
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
        let _env = crate::test_env::guard();
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
        let _env = crate::test_env::guard();
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
            libp2p_peer_id: None,
            protocol_version: crate::version::PROTOCOL_VERSION.to_string(),
            network_id: "avalon-dev-local".to_string(),
            roles: vec!["combined".to_string()],
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
            core_author_pinned: Some(true),
            connectivity: Some(avalon_protocol::connectivity::Connectivity::NatTraversed),
            reachability: crate::reachability::Reachability::Public,
            confirmed_external_addrs: vec!["/ip4/203.0.113.7/tcp/4001".to_string()],
            relay_reservations: Vec::new(),
            relayed_listen_addrs: Vec::new(),
            hole_punches: Vec::new(),
            punched_peers: Vec::new(),
            relay_server: None,
        };
        let json = serde_json::to_value(&response).expect("must serialize even when empty");
        assert_eq!(json["connectivity"], "nat_traversed");
        assert_eq!(json["reachability"], "public");
        assert_eq!(
            json["confirmed_external_addrs"][0],
            "/ip4/203.0.113.7/tcp/4001"
        );
        assert_eq!(json["core_author_pinned"], true);
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
            "network_id": "avalon-dev-local",
            "coordinate": {"vector": [0.0, 0.0, 0.0], "height": 0.01, "error": 1.0}
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

    fn supported(base_url: &str, announced_at: OffsetDateTime) -> PeerInfo {
        let mut p = peer(base_url, announced_at);
        p.protocol_version = crate::version::PROTOCOL_VERSION.to_string();
        p
    }

    fn admission_for_tests(
        allow_private: bool,
        tweak: impl FnOnce(&mut crate::peer_admission::AdmissionConfig),
    ) -> PeerAdmission {
        let mut cfg = crate::peer_admission::AdmissionConfig::default();
        tweak(&mut cfg);
        PeerAdmission::new(
            cfg,
            crate::outbound_policy::OutboundPolicy::new(allow_private),
        )
    }

    #[test]
    fn a_full_table_evicts_the_oldest_entry_that_is_not_active_or_bootstrap() {
        let table = PeerTable::new();
        let now = OffsetDateTime::now_utc();
        let age = |m| now - time::Duration::minutes(m);
        for (url, m) in [
            ("http://bootstrap", 100),
            ("http://active", 90),
            ("http://old", 50),
            ("http://newer", 10),
        ] {
            table.upsert(supported(url, age(m)));
        }
        table.neighbors().set_active(
            &["http://bootstrap".to_string(), "http://active".to_string()],
            &["http://bootstrap".to_string()],
        );

        let evicted = table.insert_bounded(supported("http://new-1", now), 4);
        assert_eq!(evicted, Ok(Some("http://old".to_string())));
        let evicted = table.insert_bounded(supported("http://new-2", now), 4);
        assert_eq!(evicted, Ok(Some("http://newer".to_string())));
        assert_eq!(table.len(), 4);
        assert!(table.contains("http://bootstrap") && table.contains("http://active"));
    }

    #[test]
    fn a_refresh_never_evicts_and_a_table_of_only_protected_entries_refuses_newcomers() {
        let table = PeerTable::new();
        let now = OffsetDateTime::now_utc();
        table.upsert(supported("http://a", now));
        table.upsert(supported("http://b", now));
        table
            .neighbors()
            .set_active(&["http://a".to_string()], &["http://b".to_string()]);
        assert_eq!(
            table.insert_bounded(supported("http://a", now), 2),
            Ok(None)
        );
        assert_eq!(
            table.insert_bounded(supported("http://c", now), 2),
            Err(TableFull)
        );
        assert_eq!(table.len(), 2);
        assert!(!table.contains("http://c"));
    }

    #[tokio::test]
    async fn gossip_accepts_at_most_the_per_exchange_cap_of_new_entries_into_the_unverified_pool() {
        let table = PeerTable::new();
        let adm = admission_for_tests(true, |c| c.max_new_per_exchange = 3);
        let now = OffsetDateTime::now_utc();
        table.upsert(supported("http://127.0.0.1:9000", now));
        let mut incoming = vec![supported("http://127.0.0.1:9000", now)];
        for i in 0..10 {
            incoming.push(supported(&format!("http://127.0.0.1:{}", 9100 + i), now));
        }
        let (admitted, skipped) = merge_gossip(&table, &adm, "avalon-dev-local", incoming).await;
        // The one already-known entry is refreshed in the main table; the
        // three newly admitted ones land only in the unverified pool.
        assert_eq!(table.len(), 1);
        assert_eq!(table.unverified_len(), 3);
        assert_eq!(admitted.len(), 4);
        assert_eq!(skipped.over_limit, 7);
    }

    #[tokio::test]
    async fn gossip_skips_invalid_and_forbidden_entries_and_counts_them() {
        let table = PeerTable::new();
        let adm = admission_for_tests(false, |c| c.max_url_len = 60);
        let now = OffsetDateTime::now_utc();
        let mut other_network = supported("http://8.8.8.8:80", now);
        other_network.network_id = "other".to_string();
        let incoming = vec![
            supported("http://127.0.0.1:9000", now),
            supported("http://10.1.2.3", now),
            supported("http://169.254.169.254", now),
            supported("ftp://8.8.4.4", now),
            supported("http://user:pw@8.8.4.4", now),
            supported(&format!("http://8.8.4.4/{}", "a".repeat(80)), now),
            other_network,
            supported("http://8.8.4.4:8080/", now),
        ];
        let (admitted, skipped) = merge_gossip(&table, &adm, "avalon-dev-local", incoming).await;
        assert_eq!(skipped.rejected, 7);
        assert_eq!(admitted.len(), 1);
        assert_eq!(admitted[0].base_url, "http://8.8.4.4:8080");
        assert_eq!(
            table.len(),
            0,
            "gossip alone must never place an entry into the main table"
        );
        assert_eq!(table.unverified_len(), 1);
    }

    /// A hostile neighbor relaying fabricated entries
    /// can never touch the main table at all, so it can never evict a real
    /// bootstrap/active peer, however full the unverified pool gets.
    #[tokio::test]
    async fn gossip_never_evicts_from_the_main_table_even_when_it_would_have_been_admitted() {
        let table = PeerTable::new();
        let adm = admission_for_tests(true, |c| c.max_known_peers = 2);
        let now = OffsetDateTime::now_utc();
        table.upsert(supported(
            "http://127.0.0.1:1",
            now - time::Duration::minutes(5),
        ));
        table.upsert(supported(
            "http://127.0.0.1:2",
            now - time::Duration::minutes(1),
        ));
        table
            .neighbors()
            .set_active(&["http://127.0.0.1:2".to_string()], &[]);
        merge_gossip(
            &table,
            &adm,
            "avalon-dev-local",
            vec![supported("http://127.0.0.1:3", now)],
        )
        .await;
        assert!(
            table.contains("http://127.0.0.1:1"),
            "gossip must not evict a main-table entry"
        );
        assert!(table.contains("http://127.0.0.1:2"));
        assert!(!table.contains("http://127.0.0.1:3"));
        assert!(table.unverified_len() == 1 && !table.list_unverified().is_empty());
    }

    #[tokio::test]
    async fn future_dated_gossip_timestamps_are_clamped_to_now() {
        let table = PeerTable::new();
        let adm = admission_for_tests(true, |_| {});
        let future = OffsetDateTime::now_utc() + time::Duration::days(365);
        merge_gossip(
            &table,
            &adm,
            "avalon-dev-local",
            vec![supported("http://127.0.0.1:9", future)],
        )
        .await;
        let stored = table.list_unverified().pop().unwrap();
        assert!(stored.last_announced_at < OffsetDateTime::now_utc() + time::Duration::minutes(1));
    }

    // --- witness key adverts ---------------------------------------

    #[tokio::test]
    async fn gossip_admits_a_proven_witness_key_and_strips_forged_mismatched_or_stale_ones() {
        let table = PeerTable::new();
        let adm = admission_for_tests(true, |_| {});
        let now = OffsetDateTime::now_utc();
        let key = ed25519_dalek::SigningKey::generate(&mut rand::rng());
        let id = hex::encode(key.verifying_key().to_bytes());
        let signer = WitnessSigner::new(key, id.clone()).unwrap();

        let mut good = supported("http://127.0.0.1:9601", now);
        good.witness = Some(signer.advert("http://127.0.0.1:9601", now));

        // Proof made for a different URL.
        let mut mismatched = supported("http://127.0.0.1:9602", now);
        mismatched.witness = Some(signer.advert("http://127.0.0.1:9601", now));

        // Someone else's key id with this signer's proof.
        let other = ed25519_dalek::SigningKey::generate(&mut rand::rng());
        let mut forged = supported("http://127.0.0.1:9603", now);
        let mut advert = signer.advert("http://127.0.0.1:9603", now);
        advert.key_id = hex::encode(other.verifying_key().to_bytes());
        forged.witness = Some(advert);

        let mut stale = supported("http://127.0.0.1:9604", now);
        stale.witness = Some(signer.advert(
            "http://127.0.0.1:9604",
            now - avalon_protocol::witness::WITNESS_ANNOUNCE_MAX_SKEW - time::Duration::minutes(1),
        ));

        merge_gossip(
            &table,
            &adm,
            "avalon-dev-local",
            vec![good, mismatched, forged, stale],
        )
        .await;
        let pool = table.list_unverified();
        assert_eq!(pool.len(), 4, "no peer is denied participation");
        for p in &pool {
            if p.base_url.ends_with(":9601") {
                assert_eq!(
                    p.witness.as_ref().map(|w| w.key_id.as_str()),
                    Some(id.as_str())
                );
            } else {
                assert!(p.witness.is_none(), "{} kept an unproven key", p.base_url);
            }
        }
    }

    #[test]
    fn a_relayed_entry_without_a_key_does_not_erase_a_proven_one() {
        let table = PeerTable::new();
        let now = OffsetDateTime::now_utc();
        let key = ed25519_dalek::SigningKey::generate(&mut rand::rng());
        let id = hex::encode(key.verifying_key().to_bytes());
        let signer = WitnessSigner::new(key, id).unwrap();
        let mut with_key = supported("http://127.0.0.1:9605", now);
        with_key.witness = Some(signer.advert("http://127.0.0.1:9605", now));
        table.upsert(with_key);
        table.upsert(supported("http://127.0.0.1:9605", now));
        assert!(table.list_all()[0].witness.is_some());
    }

    #[test]
    fn a_direct_advert_is_never_displaced_by_a_gossiped_one_with_another_key() {
        let table = PeerTable::new();
        let now = OffsetDateTime::now_utc();
        let url = "http://127.0.0.1:9606";
        let mk = |direct: bool, at: OffsetDateTime| {
            let key = ed25519_dalek::SigningKey::generate(&mut rand::rng());
            let id = hex::encode(key.verifying_key().to_bytes());
            let mut a = WitnessSigner::new(key, id).unwrap().advert(url, at);
            a.direct = direct;
            a
        };
        let direct = mk(true, now - time::Duration::minutes(5));
        table.upsert(supported(url, now));
        table.attach_witness(url, direct.clone());

        let mut gossiped = supported(url, now);
        gossiped.witness = Some(mk(false, now));
        table.upsert(gossiped);
        assert_eq!(table.list_all()[0].witness, Some(direct.clone()));

        table.upsert(supported(url, now));
        assert_eq!(table.list_all()[0].witness, Some(direct));

        let newer = mk(true, now);
        table.attach_witness(url, newer.clone());
        assert_eq!(table.list_all()[0].witness, Some(newer));
    }

    #[tokio::test]
    async fn an_inbound_only_peer_gets_a_direct_advert_only_from_its_own_response() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let now = OffsetDateTime::now_utc();
        let server = MockServer::start().await;
        let url = server.uri();
        let key = ed25519_dalek::SigningKey::generate(&mut rand::rng());
        let id = hex::encode(key.verifying_key().to_bytes());
        let signer = WitnessSigner::new(key, id).unwrap();
        let body = serde_json::json!({
            "peers": [],
            "witness": signer.advert(&url, now),
            "coordinate": {"vector": [0.0, 0.0, 0.0], "height": 0.01, "error": 1.0},
        });
        Mock::given(method("POST"))
            .and(path("/nodes/announce"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let table = PeerTable::new();
        let silent = "http://127.0.0.1:9";
        for u in [url.as_str(), silent] {
            let mut p = supported(u, now);
            p.witness = Some(signer.advert(u, now));
            table.upsert(p);
        }
        let targets = pick_vouch_targets(&table, &[], 0, VOUCH_CONTACTS_PER_TICK);
        assert_eq!(targets.len(), 2);

        let adm = admission_for_tests(true, |_| {});
        let client = crate::node_http::NodeClient::from(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
        );
        let request = announce_request(
            "http://me",
            &[],
            "avalon-dev-local",
            None,
            &[],
            &[],
            None,
            Coordinate::default(),
        );
        for t in &targets {
            vouch_contact(&client, &table, &adm, t, t, &request).await;
        }
        let direct = |u: &str| {
            table
                .list_all()
                .into_iter()
                .find(|p| p.base_url == u)
                .and_then(|p| p.witness)
                .map(|w| w.direct)
        };
        assert_eq!(direct(&url), Some(true));
        assert_eq!(direct(silent), Some(false));
        assert!(table.neighbors().protected_urls().is_empty());
        // Once direct, a peer is no longer a vouch target.
        assert_eq!(
            pick_vouch_targets(&table, &[], 0, 5),
            vec![silent.to_string()]
        );

        let policy = crate::outbound_policy::OutboundPolicy::new(true);
        let candidates =
            crate::known_list::build_candidates(&policy, &table, "avalon-dev-local", now).await;
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn a_non_key_witness_id_is_never_advertised() {
        let key = ed25519_dalek::SigningKey::generate(&mut rand::rng());
        assert!(WitnessSigner::new(key, "custom-label".to_string()).is_none());
    }

    // --- unverified gossip pool ------------------------------------

    #[tokio::test]
    async fn a_direct_announce_promotes_a_gossip_learned_entry_and_clears_the_pool() {
        let table = PeerTable::new();
        let adm = admission_for_tests(true, |_| {});
        let now = OffsetDateTime::now_utc();
        // First heard about only via gossip relay — sits in the pool, not
        // the main table.
        merge_gossip(
            &table,
            &adm,
            "avalon-dev-local",
            vec![supported("http://127.0.0.1:9500", now)],
        )
        .await;
        assert!(!table.contains("http://127.0.0.1:9500"));
        assert!(table
            .list_unverified()
            .iter()
            .any(|p| p.base_url == "http://127.0.0.1:9500"));

        // The same entry now announces itself directly — the path
        // `admit_new_announcer` takes for a brand-new base URL.
        let result = admit_promoted(&table, supported("http://127.0.0.1:9500", now), 50);
        assert_eq!(result, Ok(None));
        assert!(table.contains("http://127.0.0.1:9500"));
        assert!(
            !table
                .list_unverified()
                .iter()
                .any(|p| p.base_url == "http://127.0.0.1:9500"),
            "a promoted entry must not linger in the unverified pool"
        );
    }

    #[tokio::test]
    async fn a_successful_outbound_contact_promotes_a_gossip_learned_entry() {
        let table = PeerTable::new();
        let adm = admission_for_tests(true, |_| {});
        let now = OffsetDateTime::now_utc();
        merge_gossip(
            &table,
            &adm,
            "avalon-dev-local",
            vec![supported("http://127.0.0.1:9600", now)],
        )
        .await;
        assert!(!table.contains("http://127.0.0.1:9600"));

        promote_on_contact(&table, &adm, "http://127.0.0.1:9600");
        assert!(table.contains("http://127.0.0.1:9600"));
        assert!(!table
            .list_unverified()
            .iter()
            .any(|p| p.base_url == "http://127.0.0.1:9600"));

        // A second contact of an already-promoted (or never-unverified) URL
        // is a harmless no-op.
        promote_on_contact(&table, &adm, "http://127.0.0.1:9600");
        assert!(table.contains("http://127.0.0.1:9600"));
        promote_on_contact(&table, &adm, "http://127.0.0.1:9601");
        assert!(!table.contains("http://127.0.0.1:9601"));
    }

    #[tokio::test]
    async fn an_unpromoted_pool_entry_expires_on_the_same_rule_as_the_main_table() {
        let table = PeerTable::new();
        let adm = admission_for_tests(true, |_| {});
        let now = OffsetDateTime::now_utc();
        merge_gossip(
            &table,
            &adm,
            "avalon-dev-local",
            vec![supported(
                "http://127.0.0.1:9700",
                now - time::Duration::minutes(10),
            )],
        )
        .await;
        assert_eq!(table.unverified_len(), 1);

        table.prune_unverified_older_than(now - time::Duration::minutes(5));
        assert_eq!(table.unverified_len(), 0);
        assert!(!table.contains("http://127.0.0.1:9700"));
    }

    #[test]
    fn the_unverified_pool_is_bounded_independently_of_the_main_table() {
        let table = PeerTable::new();
        let now = OffsetDateTime::now_utc();
        for i in 0..(MAX_UNVERIFIED_PEERS + 5) {
            table.insert_unverified(supported(
                &format!("http://127.0.0.1:{}", 20000 + i),
                now - time::Duration::seconds(((MAX_UNVERIFIED_PEERS + 5 - i) * 10) as i64),
            ));
        }
        assert_eq!(table.unverified_len(), MAX_UNVERIFIED_PEERS);
        // The oldest entries were evicted to make room for the newest.
        assert!(table.list_unverified().iter().any(
            |p| p.base_url == format!("http://127.0.0.1:{}", 20000 + MAX_UNVERIFIED_PEERS + 4)
        ));
        assert!(!table
            .list_unverified()
            .iter()
            .any(|p| p.base_url == "http://127.0.0.1:20000"));
    }

    #[tokio::test]
    async fn shard_entries_are_validated_unless_already_known() {
        let registry = ShardRegistry::new();
        let adm = admission_for_tests(false, |c| c.max_new_shard_urls_per_exchange = 2);
        let now = OffsetDateTime::now_utc();
        registry.merge(&[shard_announcement("known", "http://10.0.0.1", now)]);
        let incoming = vec![
            shard_announcement("known", "http://10.0.0.1", now),
            shard_announcement("bad-private", "http://10.0.0.2", now),
            shard_announcement("bad-scheme", "file:///etc/passwd", now),
            shard_announcement("beyond-limit", "http://8.8.8.8", now),
            shard_announcement("", "http://8.8.4.4", now),
        ];
        let (kept, refused) = validated_shards(&adm, &registry, &incoming).await;
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].shard_id, "known");
        assert_eq!(refused, 4);

        let (kept, refused) = validated_shards(
            &adm,
            &registry,
            &[shard_announcement("fresh", "http://8.8.8.8/", now)],
        )
        .await;
        assert_eq!((kept.len(), refused), (1, 0));
        assert_eq!(kept[0].url, "http://8.8.8.8");
    }

    // --- head-summary gossip -------------------------------------

    fn head_summary(shard_id: &str, tree_size: i64, root_byte: u8) -> HeadSummary {
        HeadSummary {
            shard_id: shard_id.to_string(),
            tree_size,
            root_hash: hex::encode([root_byte; 32]),
            cosignature_count: 2,
        }
    }

    #[test]
    fn merge_admits_a_well_formed_summary_and_it_is_reflected_in_the_snapshot() {
        let tracker = HeadGossipTracker::new();
        let now = OffsetDateTime::now_utc();
        let (admitted, conflicts, rejected) =
            tracker.merge("http://peer-a", &[head_summary("core", 5, 1)], now);
        assert_eq!(admitted.len(), 1);
        assert!(conflicts.is_empty());
        assert_eq!(rejected, 0);
        assert_eq!(tracker.snapshot(), vec![head_summary("core", 5, 1)]);
        assert_eq!(tracker.tracked_len(), 1);
    }

    #[test]
    fn merge_rejects_malformed_summaries_and_never_admits_them() {
        let tracker = HeadGossipTracker::new();
        let now = OffsetDateTime::now_utc();
        let bad_shard = HeadSummary {
            shard_id: String::new(),
            ..head_summary("core", 5, 1)
        };
        let bad_root = HeadSummary {
            root_hash: "not-hex".to_string(),
            ..head_summary("core", 6, 1)
        };
        let bad_size = HeadSummary {
            tree_size: -1,
            ..head_summary("core", 7, 1)
        };
        let (admitted, conflicts, rejected) =
            tracker.merge("http://peer-a", &[bad_shard, bad_root, bad_size], now);
        assert!(admitted.is_empty());
        assert!(conflicts.is_empty());
        assert_eq!(rejected, 3);
        assert_eq!(tracker.tracked_len(), 0);
    }

    /// "gossiping more than 5 summaries in one exchange, only 5 are
    /// sent/accepted" — the bounded-volume requirement, checked on both the
    /// receive side (here) and the send side (`snapshot_never_exceeds_the_per_exchange_cap`).
    #[test]
    fn merge_accepts_at_most_the_per_exchange_cap_regardless_of_how_many_arrive() {
        let tracker = HeadGossipTracker::new();
        let now = OffsetDateTime::now_utc();
        let incoming: Vec<HeadSummary> =
            (0..12).map(|i| head_summary("core", i, i as u8)).collect();
        let (admitted, _conflicts, rejected) = tracker.merge("http://peer-a", &incoming, now);
        assert_eq!(admitted.len(), MAX_HEAD_SUMMARIES_PER_EXCHANGE);
        assert_eq!(rejected, 12 - MAX_HEAD_SUMMARIES_PER_EXCHANGE);
        assert_eq!(tracker.tracked_len(), MAX_HEAD_SUMMARIES_PER_EXCHANGE);
    }

    #[test]
    fn snapshot_never_exceeds_the_per_exchange_cap() {
        let tracker = HeadGossipTracker::new();
        let now = OffsetDateTime::now_utc();
        for i in 0..20 {
            tracker.merge(
                "http://peer-a",
                &[head_summary(&format!("shard-{i}"), 1, i as u8)],
                now + time::Duration::seconds(i),
            );
        }
        assert_eq!(tracker.snapshot().len(), MAX_HEAD_SUMMARIES_PER_EXCHANGE);
    }

    /// A log shown two different versions to disjoint witness groups is
    /// detected as soon as the second, conflicting root is gossiped in —
    /// the fork signal that feeds `crate::equivocation::confirm_and_record`.
    #[test]
    fn merge_detects_a_conflicting_root_at_the_same_shard_and_tree_size() {
        let tracker = HeadGossipTracker::new();
        let now = OffsetDateTime::now_utc();
        let (_, conflicts, _) = tracker.merge("http://peer-a", &[head_summary("core", 5, 1)], now);
        assert!(conflicts.is_empty());

        let (_, conflicts, _) = tracker.merge("http://peer-b", &[head_summary("core", 5, 2)], now);
        assert_eq!(conflicts.len(), 1);
        let conflict = &conflicts[0];
        assert_eq!(conflict.shard_id, "core");
        assert_eq!(conflict.tree_size, 5);
        assert_eq!(conflict.source_a, "http://peer-b");
        assert_eq!(conflict.source_b, "http://peer-a");
    }

    #[test]
    fn merge_does_not_re_report_a_conflict_for_a_shard_already_confirmed_equivocating() {
        let tracker = HeadGossipTracker::new();
        let now = OffsetDateTime::now_utc();
        tracker.merge("http://peer-a", &[head_summary("core", 5, 1)], now);
        tracker.mark_equivocating("core");

        let (_, conflicts, _) = tracker.merge("http://peer-b", &[head_summary("core", 5, 2)], now);
        assert!(conflicts.is_empty());
        assert!(tracker.is_equivocating("core"));
    }

    #[test]
    fn same_root_reported_twice_is_not_a_conflict() {
        let tracker = HeadGossipTracker::new();
        let now = OffsetDateTime::now_utc();
        tracker.merge("http://peer-a", &[head_summary("core", 5, 1)], now);
        let (_, conflicts, _) = tracker.merge("http://peer-b", &[head_summary("core", 5, 1)], now);
        assert!(conflicts.is_empty());
    }

    fn responder(base_url: &str, peer_id: &libp2p::PeerId, addrs: &[&str]) -> PeerInfo {
        PeerInfo {
            libp2p_peer_id: Some(peer_id.to_string()),
            libp2p_listen_addrs: addrs.iter().map(|a| a.to_string()).collect(),
            connectivity: Some(avalon_protocol::connectivity::Connectivity::Direct),
            ..supported(base_url, OffsetDateTime::now_utc())
        }
    }

    fn fresh_peer_id() -> libp2p::PeerId {
        libp2p::identity::Keypair::generate_ed25519()
            .public()
            .into()
    }

    async fn announce_response_from(
        server: &wiremock::MockServer,
        node: &PeerInfo,
    ) -> AnnounceResponse {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};
        Mock::given(method("POST"))
            .and(path("/nodes/announce"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "peers": [],
                "coordinate": Coordinate::default(),
                "node": node,
            })))
            .mount(server)
            .await;
        let request = announce_request(
            "http://me",
            &[],
            "avalon-dev-local",
            None,
            &[],
            &[],
            None,
            Coordinate::default(),
        );
        announce_to(
            &crate::node_http::NodeClient::new(),
            &server.uri(),
            &request,
        )
        .await
        .expect("announce succeeds")
    }

    #[tokio::test]
    async fn a_responders_own_entry_is_stored_with_its_libp2p_identity() {
        let server = wiremock::MockServer::start().await;
        let id = fresh_peer_id();
        let node = responder(
            &server.uri(),
            &id,
            &["/ip4/203.0.113.7/tcp/4001", "/dns4/x.example/tcp/1"],
        );
        let response = announce_response_from(&server, &node).await;
        let table = PeerTable::new();
        let adm = admission_for_tests(true, |_| {});
        let node = response.node.expect("response carries the responder");
        assert!(admit_responder_node(
            &table,
            &adm,
            "avalon-dev-local",
            &server.uri(),
            node
        ));
        let stored = &table.list_all()[0];
        assert_eq!(stored.base_url, normalized_base_url(&server.uri()));
        assert_eq!(stored.libp2p_peer_id, Some(id.to_string()));
        assert_eq!(
            stored.libp2p_listen_addrs,
            vec!["/ip4/203.0.113.7/tcp/4001".to_string()]
        );
    }

    fn table_entry(url: &str, id: Option<&PeerId>, bound: bool) -> PeerInfo {
        PeerInfo {
            libp2p_peer_id: id.map(|i| i.to_string()),
            libp2p_listen_addrs: vec!["/ip4/203.0.113.7/tcp/4001".into()],
            connectivity: id.map(|_| avalon_protocol::connectivity::Connectivity::Relayed),
            identity_bound: bound,
            ..peer(url, OffsetDateTime::now_utc())
        }
    }

    fn fresh_libp2p() -> PeerId {
        PeerId::random()
    }

    #[test]
    fn an_unbound_write_never_changes_a_bound_peers_identity_or_routing() {
        let (real, attacker) = (fresh_libp2p(), fresh_libp2p());
        let table = PeerTable::new();
        table.upsert(table_entry("http://v.test", Some(&real), true));
        assert_eq!(
            crate::node_http::NodeClient::url_for(&table.list_all()[0]),
            crate::node_http::p2p_base_url(&real)
        );

        // The announce path, the gossip path and the responder-less bounded path.
        let hostile = table_entry("http://v.test", Some(&attacker), false);
        table.upsert(hostile.clone());
        table.insert_bounded(hostile, 10).unwrap();
        let stored = &table.list_all()[0];
        assert_eq!(stored.libp2p_peer_id, Some(real.to_string()));
        assert!(stored.identity_bound);
        assert_eq!(
            crate::node_http::NodeClient::url_for(stored),
            crate::node_http::p2p_base_url(&real)
        );
        assert_eq!(table.libp2p_peer_for_url("http://v.test/x"), Some(real));
        assert!(!table.is_bound_libp2p_peer(&attacker));
    }

    #[test]
    fn the_same_id_refreshes_a_bound_entry_and_only_bound_entries_route_by_id() {
        let id = fresh_libp2p();
        let table = PeerTable::new();
        table.upsert(table_entry("http://v.test", Some(&id), true));
        let mut refresh = table_entry("http://v.test", Some(&id), false);
        refresh.libp2p_listen_addrs = vec!["/ip4/198.51.100.1/tcp/1".into()];
        table.upsert(refresh);
        let stored = &table.list_all()[0];
        assert!(stored.identity_bound);
        assert_eq!(stored.libp2p_listen_addrs, vec!["/ip4/198.51.100.1/tcp/1"]);

        let unbound = PeerTable::new();
        unbound.upsert(table_entry("http://u.test", Some(&id), false));
        assert_eq!(
            crate::node_http::NodeClient::url_for(&unbound.list_all()[0]),
            "http://u.test"
        );
        assert_eq!(unbound.transport_url("http://u.test"), "http://u.test");
        assert_eq!(unbound.libp2p_peer_for_url("http://u.test/x"), None);
        assert!(!unbound.is_bound_libp2p_peer(&id));
    }

    #[test]
    fn a_status_answer_must_match_the_announced_id_to_keep_it() {
        let (a, b) = (fresh_libp2p(), fresh_libp2p());
        let mut ok = table_entry("http://v.test", Some(&a), false);
        bind_or_blank(&mut ok, Some(a.to_string()));
        assert!(ok.identity_bound);
        assert_eq!(ok.libp2p_peer_id, Some(a.to_string()));
        for reported in [Some(b.to_string()), None] {
            let mut bad = table_entry("http://v.test", Some(&a), false);
            bind_or_blank(&mut bad, reported);
            assert!(!bad.identity_bound);
            assert_eq!(bad.libp2p_peer_id, None);
            assert!(bad.libp2p_listen_addrs.is_empty());
            assert_eq!(bad.connectivity, None);
        }
    }

    async fn status_server(reported: Option<&PeerId>) -> wiremock::MockServer {
        use wiremock::matchers::{method, path};
        let server = wiremock::MockServer::start().await;
        let mut body = serde_json::json!({"network_id": "avalon-dev-local"});
        if let Some(id) = reported {
            body["libp2p_peer_id"] = id.to_string().into();
        }
        wiremock::Mock::given(method("GET"))
            .and(path("/nodes/status"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn an_announce_naming_a_new_id_for_a_bound_url_needs_the_urls_own_confirmation() {
        let (real, new_id, attacker) = (fresh_libp2p(), fresh_libp2p(), fresh_libp2p());
        let adm = admission_for_tests(true, |_| {});
        let source: IpAddr = "203.0.113.9".parse().unwrap();

        // The URL's server says it is `new_id`: a legitimate re-key is accepted and bound.
        let server = status_server(Some(&new_id)).await;
        let url = normalized_base_url(&server.uri());
        let table = PeerTable::new();
        table.upsert(table_entry(&url, Some(&real), true));
        let claim = table_entry(&url, Some(&new_id), false);
        let info = rebind_existing(&table, "avalon-dev-local", &adm, source, claim).await;
        assert!(info.identity_bound);
        table.upsert(info);
        assert_eq!(table.list_all()[0].libp2p_peer_id, Some(new_id.to_string()));

        // A third party naming another id: the URL's server disagrees, the stored id stays.
        let claim = table_entry(&url, Some(&attacker), false);
        let info = rebind_existing(&table, "avalon-dev-local", &adm, source, claim).await;
        assert!(!info.identity_bound);
        table.upsert(info);
        let stored = &table.list_all()[0];
        assert_eq!(stored.libp2p_peer_id, Some(new_id.to_string()));
        assert!(stored.identity_bound);

        // A server that cannot be reached leaves the stored identity alone as well.
        let dead = "http://127.0.0.1:1".to_string();
        table.upsert(table_entry(&dead, Some(&real), true));
        let claim = table_entry(&dead, Some(&attacker), false);
        let info = rebind_existing(&table, "avalon-dev-local", &adm, source, claim).await;
        table.upsert(info);
        let stored = table
            .list_all()
            .into_iter()
            .find(|p| p.base_url == dead)
            .unwrap();
        assert_eq!(stored.libp2p_peer_id, Some(real.to_string()));
    }

    #[tokio::test]
    async fn responder_entries_are_bound_and_gossip_never_binds() {
        let server = wiremock::MockServer::start().await;
        let id = fresh_peer_id();
        let own = responder(&server.uri(), &id, &["/ip4/203.0.113.7/tcp/4001"]);
        let adm = admission_for_tests(true, |_| {});
        let table = PeerTable::new();
        assert!(admit_responder_node(
            &table,
            &adm,
            "avalon-dev-local",
            &server.uri(),
            own
        ));
        assert!(table.list_all()[0].identity_bound);
        assert_eq!(table.list_all()[0].libp2p_peer_id, Some(id.to_string()));

        let url = table.list_all()[0].base_url.clone();
        let hostile = PeerInfo {
            identity_bound: false,
            ..table_entry(&url, Some(&fresh_libp2p()), true)
        };
        merge_gossip(&table, &adm, "avalon-dev-local", vec![hostile]).await;
        assert_eq!(table.list_all()[0].libp2p_peer_id, Some(id.to_string()));

        let pooled = PeerTable::new();
        pooled.insert_unverified(table_entry(
            "http://pooled.test",
            Some(&fresh_libp2p()),
            false,
        ));
        assert!(pooled.list_unverified().iter().all(|p| !p.identity_bound));
    }

    #[tokio::test]
    async fn a_responder_naming_another_url_network_or_old_version_is_ignored() {
        let server = wiremock::MockServer::start().await;
        let id = fresh_peer_id();
        let hostile = responder("http://victim.example", &id, &["/ip4/203.0.113.7/tcp/4001"]);
        let response = announce_response_from(&server, &hostile).await;
        let table = PeerTable::new();
        let adm = admission_for_tests(true, |_| {});
        let called = server.uri();
        assert!(!admit_responder_node(
            &table,
            &adm,
            "avalon-dev-local",
            &called,
            response.node.unwrap()
        ));
        let own = responder(&called, &id, &[]);
        let other_net = PeerInfo {
            network_id: "other".into(),
            ..own.clone()
        };
        assert!(!admit_responder_node(
            &table,
            &adm,
            "avalon-dev-local",
            &called,
            other_net
        ));
        let old = PeerInfo {
            protocol_version: "0.0.0".into(),
            ..own
        };
        assert!(!admit_responder_node(
            &table,
            &adm,
            "avalon-dev-local",
            &called,
            old
        ));
        assert!(table.list_all().is_empty());
    }

    #[test]
    fn a_responder_entry_respects_the_peer_table_cap() {
        let table = PeerTable::new();
        let adm = admission_for_tests(true, |c| c.max_known_peers = 1);
        let protected = "http://127.0.0.1:1";
        table.upsert(supported(protected, OffsetDateTime::now_utc()));
        table.neighbors().set_active(&[protected.to_string()], &[]);
        let node = responder("http://127.0.0.1:2", &fresh_peer_id(), &[]);
        assert!(!admit_responder_node(
            &table,
            &adm,
            "avalon-dev-local",
            "http://127.0.0.1:2",
            node
        ));
        assert_eq!(table.len(), 1);
        assert!(table.contains(protected));
    }

    #[test]
    fn own_peer_info_carries_identity_addresses_and_connectivity() {
        let handle =
            crate::reachability::ReachabilityHandle::new(Some("/ip4/203.0.113.7/tcp/4001".into()));
        handle.set_reachability(crate::reachability::Reachability::Public);
        let id = fresh_peer_id().to_string();
        let info = own_peer_info(
            "http://me",
            Some(id.clone()),
            &handle,
            "n",
            OffsetDateTime::now_utc(),
        );
        assert_eq!(info.libp2p_peer_id, Some(id));
        assert_eq!(
            info.libp2p_listen_addrs,
            vec!["/ip4/203.0.113.7/tcp/4001".to_string()]
        );
        assert_eq!(
            info.connectivity,
            Some(avalon_protocol::connectivity::Connectivity::Direct)
        );
        let no_dht = own_peer_info("http://me", None, &handle, "n", OffsetDateTime::now_utc());
        assert!(no_dht.libp2p_listen_addrs.is_empty());
    }

    #[test]
    fn announce_payloads_without_node_or_connectivity_still_decode() {
        let info: PeerInfo = serde_json::from_str(
            r#"{"base_url":"http://old","roles":[],"protocol_version":"0.1.0","network_id":"n","last_announced_at":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(info.connectivity, None);
        let req: AnnounceRequest = serde_json::from_str(
            r#"{"base_url":"http://old","roles":[],"protocol_version":"0.1.0","network_id":"n","coordinate":{"vector":[0.0,0.0,0.0],"height":0.01,"error":1.0}}"#,
        )
        .unwrap();
        assert_eq!(req.connectivity, None);
        let resp: AnnounceResponse = serde_json::from_str(
            r#"{"peers":[],"coordinate":{"vector":[0.0,0.0,0.0],"height":0.01,"error":1.0}}"#,
        )
        .unwrap();
        assert!(resp.node.is_none());
        let out = serde_json::to_value(&resp).unwrap();
        assert!(out.get("node").is_none());
    }

    #[tokio::test]
    async fn gossip_merge_sanitizes_libp2p_addresses() {
        let table = PeerTable::new();
        let adm = admission_for_tests(true, |_| {});
        let id = fresh_peer_id();
        let other = fresh_peer_id();
        let gossiped = responder(
            "http://127.0.0.1:9",
            &id,
            &[
                "/ip4/203.0.113.7/tcp/4001",
                &format!("/ip4/203.0.113.8/tcp/4001/p2p/{other}"),
            ],
        );
        let (admitted, _) = merge_gossip(&table, &adm, "avalon-dev-local", vec![gossiped]).await;
        assert_eq!(
            admitted[0].libp2p_listen_addrs,
            vec!["/ip4/203.0.113.7/tcp/4001".to_string()]
        );
    }

    #[test]
    fn a_libp2p_confirmation_promotes_only_the_pool_entry_for_that_peer_id() {
        let table = PeerTable::new();
        let (a, b) = (fresh_peer_id(), fresh_peer_id());
        let now = OffsetDateTime::now_utc();
        table.insert_unverified(responder("http://127.0.0.1:1", &a, &[]));
        table.insert_unverified(responder("http://127.0.0.1:2", &b, &[]));
        table.insert_unverified(supported("http://127.0.0.1:3", now));

        let promoted = table.promote_unverified_by_libp2p_peer(&a.to_string(), 10);
        assert_eq!(promoted, vec!["http://127.0.0.1:1".to_string()]);
        assert!(table.contains("http://127.0.0.1:1"));
        assert_eq!(table.unverified_len(), 2, "other entries stay unverified");
        assert!(table
            .promote_unverified_by_libp2p_peer(&fresh_peer_id().to_string(), 10)
            .is_empty());
    }

    #[test]
    fn a_libp2p_promotion_respects_the_peer_table_cap() {
        let table = PeerTable::new();
        let protected = "http://127.0.0.1:1";
        table.upsert(supported(protected, OffsetDateTime::now_utc()));
        table.neighbors().set_active(&[protected.to_string()], &[]);
        let id = fresh_peer_id();
        table.insert_unverified(responder("http://127.0.0.1:2", &id, &[]));
        assert!(table
            .promote_unverified_by_libp2p_peer(&id.to_string(), 1)
            .is_empty());
        assert_eq!(table.len(), 1);
        assert!(table.contains(protected));
    }
    fn p2p_entry(id: &PeerId, bound: bool) -> PeerInfo {
        PeerInfo {
            base_url: crate::node_http::p2p_base_url(id),
            libp2p_peer_id: Some(id.to_string()),
            identity_bound: bound,
            ..responder("", id, &["/ip4/203.0.113.7/tcp/4001"])
        }
    }

    fn source() -> IpAddr {
        "203.0.113.9".parse().unwrap()
    }

    #[test]
    fn a_p2p_announce_is_classified_by_the_stream_that_carried_it() {
        let (me, other) = (fresh_peer_id(), fresh_peer_id());
        let url = crate::node_http::p2p_base_url(&me);
        let id = me.to_string();
        let classify =
            |u: &str, claimed: Option<&str>, remote| classify_p2p_announce(u, claimed, remote);

        assert_eq!(
            classify("http://a.test", None, None).unwrap(),
            P2pAnnounce::NotP2p
        );
        assert_eq!(
            classify("http://a.test", None, Some(other)).unwrap(),
            P2pAnnounce::NotP2p
        );
        assert_eq!(
            classify(&url, Some(&id), Some(me)).unwrap(),
            P2pAnnounce::Authenticated(me)
        );
        assert_eq!(
            classify(&format!("{url}/"), Some(&id), Some(me)).unwrap(),
            P2pAnnounce::Authenticated(me)
        );
        // Plain HTTP carries no proof of the id.
        assert_eq!(
            classify(&url, Some(&id), None).unwrap(),
            P2pAnnounce::Unauthenticated
        );
        // Authenticated as someone else: refused, not downgraded.
        let err = classify(&url, Some(&id), Some(other)).unwrap_err();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        // The URL and the announced libp2p id must agree.
        for claimed in [None, Some(other.to_string())] {
            let err = classify(&url, claimed.as_deref(), Some(me)).unwrap_err();
            assert_eq!(err.status, StatusCode::BAD_REQUEST);
            let err = classify(&url, claimed.as_deref(), None).unwrap_err();
            assert_eq!(err.status, StatusCode::BAD_REQUEST);
        }
        // Not a peer id, or a URL with a path.
        for bad in ["p2p://nope", "p2p://", &format!("{url}/x")] {
            let err = classify(bad, Some(&id), Some(me)).unwrap_err();
            assert_eq!(err.status, StatusCode::BAD_REQUEST, "{bad}");
        }
    }

    #[test]
    fn an_authenticated_p2p_announcer_is_admitted_bound_without_a_reachability_check() {
        // A p2p:// URL has nothing to dial; the default config would reject one that needs it.
        let adm = admission_for_tests(false, |_| {});
        let id = fresh_peer_id();
        let table = PeerTable::new();
        store_authenticated_p2p_announcer(&table, &adm, source(), p2p_entry(&id, false)).unwrap();

        let stored = &table.list_all()[0];
        assert_eq!(stored.base_url, crate::node_http::p2p_base_url(&id));
        assert!(stored.identity_bound);
        assert!(
            !table.is_bound_libp2p_peer(&id),
            "self-bound entries do not unlock write routes"
        );
        assert_eq!(
            crate::node_http::NodeClient::url_for(stored),
            crate::node_http::p2p_base_url(&id)
        );
        assert_eq!(
            table.stream_url(&stored.base_url),
            Some(crate::node_http::p2p_base_url(&id))
        );
    }

    #[test]
    fn a_p2p_announcer_still_counts_against_the_source_budget_table_cap_and_url_length() {
        let adm = admission_for_tests(false, |c| c.new_urls_per_source = 1);
        let table = PeerTable::new();
        let (a, b) = (fresh_peer_id(), fresh_peer_id());
        store_authenticated_p2p_announcer(&table, &adm, source(), p2p_entry(&a, false)).unwrap();
        let err = store_authenticated_p2p_announcer(&table, &adm, source(), p2p_entry(&b, false))
            .unwrap_err();
        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
        // A refresh of a known URL is not a new URL.
        store_authenticated_p2p_announcer(&table, &adm, source(), p2p_entry(&a, false)).unwrap();

        let adm = admission_for_tests(false, |c| c.max_url_len = 20);
        let err = store_authenticated_p2p_announcer(
            &PeerTable::new(),
            &adm,
            source(),
            p2p_entry(&a, false),
        )
        .unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);

        let adm = admission_for_tests(false, |c| c.max_known_peers = 1);
        let full = PeerTable::new();
        full.upsert(supported("http://127.0.0.1:1", OffsetDateTime::now_utc()));
        full.neighbors()
            .set_active(&["http://127.0.0.1:1".to_string()], &[]);
        let err = store_authenticated_p2p_announcer(&full, &adm, source(), p2p_entry(&a, false))
            .unwrap_err();
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn gossip_cannot_overwrite_an_existing_p2p_entry_but_its_own_announce_can() {
        let adm = admission_for_tests(false, |_| {});
        let id = fresh_peer_id();
        let table = PeerTable::new();
        let mut existing = p2p_entry(&id, true);
        existing.connectivity = Some(avalon_protocol::connectivity::Connectivity::Relayed);
        table.upsert(existing);
        // A self-consistent gossip copy with different data.
        let mut gossip = p2p_entry(&id, false);
        gossip.roles = vec!["indexer".into()];
        gossip.protocol_version = "9.9.9".into();
        gossip.libp2p_listen_addrs = vec!["/ip4/198.51.100.1/tcp/1".into()];
        gossip.connectivity = Some(avalon_protocol::connectivity::Connectivity::Direct);
        merge_gossip(&table, &adm, "avalon-dev-local", vec![gossip]).await;
        let stored = table.list_all().remove(0);
        assert_eq!(stored.roles, vec!["combined".to_string()]);
        assert_eq!(
            stored.libp2p_listen_addrs,
            vec!["/ip4/203.0.113.7/tcp/4001"]
        );
        assert_eq!(
            stored.connectivity,
            Some(avalon_protocol::connectivity::Connectivity::Relayed)
        );
        assert_ne!(stored.protocol_version, "9.9.9");

        // The peer's own authenticated announce does update it.
        let mut own = p2p_entry(&id, false);
        own.roles = vec!["indexer".into()];
        store_authenticated_p2p_announcer(&table, &adm, source(), own).unwrap();
        assert_eq!(table.list_all()[0].roles, vec!["indexer".to_string()]);
    }

    #[test]
    fn an_unbound_write_naming_another_id_never_changes_a_p2p_entry() {
        let (real, other) = (fresh_peer_id(), fresh_peer_id());
        let table = PeerTable::new();
        table.upsert(p2p_entry(&real, true));
        let mut hostile = p2p_entry(&real, false);
        hostile.libp2p_peer_id = Some(other.to_string());
        table.upsert(hostile);
        let stored = &table.list_all()[0];
        assert_eq!(stored.libp2p_peer_id, Some(real.to_string()));
        assert!(stored.identity_bound);
    }

    #[tokio::test]
    async fn gossip_puts_only_self_consistent_p2p_entries_in_the_pool_unbound() {
        // A strict policy: nothing dialable by address is allowed, yet p2p:// has no address.
        let adm = admission_for_tests(false, |_| {});
        let table = PeerTable::new();
        let (good, liar, other) = (fresh_peer_id(), fresh_peer_id(), fresh_peer_id());
        let mut wrong_id = p2p_entry(&liar, false);
        wrong_id.libp2p_peer_id = Some(other.to_string());
        let mut no_id = p2p_entry(&other, false);
        no_id.libp2p_peer_id = None;
        let mut bad_url = p2p_entry(&other, false);
        bad_url.base_url = "p2p://not-a-peer-id".into();
        let mut with_path = p2p_entry(&other, false);
        with_path.base_url = format!("{}/x", with_path.base_url);

        let (admitted, skipped) = merge_gossip(
            &table,
            &adm,
            "avalon-dev-local",
            vec![p2p_entry(&good, true), wrong_id, no_id, bad_url, with_path],
        )
        .await;
        assert_eq!(admitted.len(), 1);
        assert_eq!(skipped.rejected, 4);
        assert_eq!(table.len(), 0, "gossip never writes the main table");
        let pooled = table.list_unverified();
        assert_eq!(pooled.len(), 1);
        assert_eq!(pooled[0].base_url, crate::node_http::p2p_base_url(&good));
        assert!(!pooled[0].identity_bound, "gossip never binds");
    }

    #[test]
    fn a_libp2p_contact_binds_a_pooled_p2p_entry_but_not_other_entries_naming_the_id() {
        let table = PeerTable::new();
        let (a, b) = (fresh_peer_id(), fresh_peer_id());
        table.insert_unverified(p2p_entry(&a, false));
        // An http entry merely claiming `a`'s id, and a p2p entry whose id field disagrees.
        table.insert_unverified(responder("http://127.0.0.1:1", &a, &[]));
        let mut mismatched = p2p_entry(&b, false);
        mismatched.libp2p_peer_id = Some(a.to_string());
        table.insert_unverified(mismatched);

        let promoted = table.promote_unverified_by_libp2p_peer(&a.to_string(), 10);
        assert_eq!(promoted.len(), 3);
        let by_url = |u: &str| {
            table
                .list_all()
                .into_iter()
                .find(|p| p.base_url == u)
                .unwrap()
        };
        assert!(by_url(&crate::node_http::p2p_base_url(&a)).identity_bound);
        assert!(!by_url("http://127.0.0.1:1").identity_bound);
        assert!(!by_url(&crate::node_http::p2p_base_url(&b)).identity_bound);
        assert!(
            !table.is_bound_libp2p_peer(&a),
            "a p2p-only entry never unlocks write routes"
        );
        let promoted = by_url(&crate::node_http::p2p_base_url(&a));
        assert!(promoted.roles.is_empty() && promoted.libp2p_listen_addrs.is_empty());
        assert_eq!(promoted.protocol_version, "0.0.0");
    }

    #[test]
    fn contacting_a_pooled_p2p_entry_over_its_stream_binds_it() {
        let adm = admission_for_tests(false, |_| {});
        let (a, b) = (fresh_peer_id(), fresh_peer_id());
        let table = PeerTable::new();
        table.insert_unverified(p2p_entry(&a, false));
        let mut forged = p2p_entry(&b, false);
        forged.libp2p_peer_id = Some(a.to_string());
        table.insert_unverified(forged);
        table.insert_unverified(responder("http://127.0.0.1:1", &a, &[]));

        for url in [
            crate::node_http::p2p_base_url(&a),
            crate::node_http::p2p_base_url(&b),
            "http://127.0.0.1:1".to_string(),
        ] {
            promote_on_contact(&table, &adm, &url);
        }
        let bound: Vec<String> = table
            .list_all()
            .into_iter()
            .filter(|p| p.identity_bound)
            .map(|p| p.base_url)
            .collect();
        assert_eq!(bound, vec![crate::node_http::p2p_base_url(&a)]);
    }

    #[test]
    fn a_node_without_a_url_announces_as_its_peer_id_only_when_libp2p_runs() {
        let config = |url: Option<&str>| AnnounceConfig {
            peers: vec![],
            interval: Duration::from_secs(1),
            own_base_url: url.map(str::to_string),
            max_peers: 1,
            witness: None,
        };
        let id = fresh_peer_id();
        let p2p = crate::node_http::p2p_base_url(&id);
        assert_eq!(
            config(None)
                .with_p2p_fallback(Some(&id.to_string()))
                .own_base_url,
            Some(p2p)
        );
        let announced = config(None).with_p2p_fallback(Some(&id.to_string()));
        assert_eq!(announced.own_http_base_url(), None);
        assert_eq!(
            config(Some("http://a.test")).own_http_base_url(),
            Some("http://a.test".to_string())
        );
        // A configured URL is never replaced.
        assert_eq!(
            config(Some("http://a.test"))
                .with_p2p_fallback(Some(&id.to_string()))
                .own_base_url,
            Some("http://a.test".to_string())
        );
        for id in [None, Some("not-a-peer-id")] {
            assert_eq!(config(None).with_p2p_fallback(id).own_base_url, None);
        }
    }

    #[test]
    fn a_url_less_node_announces_to_a_peer_over_its_stream_once_the_peer_id_is_bound() {
        let table = PeerTable::new();
        let id = fresh_peer_id();
        let own_p2p = crate::node_http::p2p_base_url(&fresh_peer_id());
        let seed = "http://seed.test";
        // The seed's id is not known yet: HTTP, where a p2p:// announce is never stored.
        assert_eq!(announce_target(&table, seed, &own_p2p), seed);
        table.upsert(table_entry(seed, Some(&id), true));
        assert_eq!(
            announce_target(&table, seed, &own_p2p),
            crate::node_http::p2p_base_url(&id)
        );
        // A node with its own URL keeps the HTTP path for a peer it can reach.
        let mut plain = table_entry("http://plain.test", Some(&id), true);
        plain.connectivity = Some(avalon_protocol::connectivity::Connectivity::Direct);
        table.upsert(plain);
        assert_eq!(
            announce_target(&table, "http://plain.test", "http://me.test"),
            "http://plain.test"
        );
        // An unbound id is never trusted to route.
        table.upsert(table_entry("http://unbound.test", Some(&id), false));
        assert_eq!(
            announce_target(&table, "http://unbound.test", &own_p2p),
            "http://unbound.test"
        );
        // A p2p:// peer is always reached over its stream.
        let peer_url = crate::node_http::p2p_base_url(&id);
        assert_eq!(announce_target(&table, &peer_url, &own_p2p), peer_url);
    }

    #[test]
    fn a_responder_entry_for_a_p2p_url_must_name_that_peer() {
        let adm = admission_for_tests(false, |_| {});
        let (a, b) = (fresh_peer_id(), fresh_peer_id());
        let called = crate::node_http::p2p_base_url(&a);
        let table = PeerTable::new();
        let mut wrong = p2p_entry(&a, false);
        wrong.libp2p_peer_id = Some(b.to_string());
        assert!(!admit_responder_node(
            &table,
            &adm,
            "avalon-dev-local",
            &called,
            wrong
        ));
        assert!(!admit_responder_node(
            &table,
            &adm,
            "avalon-dev-local",
            &called,
            p2p_entry(&b, false)
        ));
        assert!(table.list_all().is_empty());
        assert!(admit_responder_node(
            &table,
            &adm,
            "avalon-dev-local",
            &called,
            p2p_entry(&a, false)
        ));
        assert!(table.list_all()[0].identity_bound);
    }

    #[tokio::test]
    async fn shard_gossip_accepts_a_p2p_url_without_an_address_check() {
        let adm = admission_for_tests(false, |_| {});
        let id = fresh_peer_id();
        let entry = |url: String| ShardAnnouncement {
            shard_id: "core".into(),
            url,
            last_seen_at: OffsetDateTime::now_utc(),
        };
        let (kept, refused) = validated_shards(
            &adm,
            &ShardRegistry::new(),
            &[
                entry(crate::node_http::p2p_base_url(&id)),
                entry("http://127.0.0.1:1".into()),
            ],
        )
        .await;
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].url, crate::node_http::p2p_base_url(&id));
        assert_eq!(refused, 1);
    }
    async fn announce_mock(server: &wiremock::MockServer, hits: u64) {
        use wiremock::matchers::{method, path};
        wiremock::Mock::given(method("POST"))
            .and(path("/nodes/announce"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "peers": [],
                    "coordinate": Coordinate::default(),
                })),
            )
            .expect(hits)
            .mount(server)
            .await;
    }

    fn announce_body(own: &str) -> AnnounceRequest {
        announce_request(
            own,
            &[],
            "avalon-dev-local",
            None,
            &[],
            &[],
            None,
            Coordinate::default(),
        )
    }

    #[tokio::test]
    async fn a_url_less_announcer_whose_stream_fails_retries_over_the_peers_url() {
        let server = wiremock::MockServer::start().await;
        announce_mock(&server, 1).await;
        let client = crate::node_http::NodeClient::new();
        let table = PeerTable::new();
        let url = normalized_base_url(&server.uri());
        table.upsert(table_entry(&url, Some(&fresh_libp2p()), true));
        let own = crate::node_http::p2p_base_url(&fresh_libp2p());

        // No stream transport runs here, so the p2p:// attempt fails first.
        let result = announce_to_peer(&client, &table, &url, &own, &announce_body(&own)).await;
        assert!(matches!(result, Ok((_, true))), "{result:?}");
    }

    #[tokio::test]
    async fn an_announcer_with_a_url_does_not_retry_a_failed_stream_over_http() {
        let server = wiremock::MockServer::start().await;
        announce_mock(&server, 0).await;
        let client = crate::node_http::NodeClient::new();
        let table = PeerTable::new();
        let url = normalized_base_url(&server.uri());
        table.upsert(table_entry(&url, Some(&fresh_libp2p()), true));

        let result = announce_to_peer(
            &client,
            &table,
            &url,
            "http://me.test",
            &announce_body("http://me.test"),
        )
        .await;
        assert!(result.is_err());
    }
    #[test]
    fn p2p_entries_have_their_own_cap_and_are_evicted_before_http_entries() {
        let table = PeerTable::new();
        let now = OffsetDateTime::now_utc();
        for i in 0..MAX_P2P_ENTRIES + 5 {
            let mut e = p2p_entry(&fresh_peer_id(), true);
            e.last_announced_at = now + time::Duration::seconds(i as i64);
            table.insert_bounded(e, 10_000).unwrap();
        }
        let p2p = |t: &PeerTable| {
            t.list_all()
                .iter()
                .filter(|p| is_p2p_url(&p.base_url))
                .count()
        };
        assert_eq!(p2p(&table), MAX_P2P_ENTRIES);

        // At the global cap a newcomer evicts a p2p entry even though http ones are older.
        let small = PeerTable::new();
        small
            .insert_bounded(
                supported("http://127.0.0.1:1", now - time::Duration::hours(1)),
                2,
            )
            .unwrap();
        small
            .insert_bounded(p2p_entry(&fresh_peer_id(), true), 2)
            .unwrap();
        let evicted = small
            .insert_bounded(supported("http://127.0.0.1:2", now), 2)
            .unwrap();
        assert!(evicted.unwrap().starts_with("p2p://"));
        assert!(small.contains("http://127.0.0.1:1"));
    }

    #[tokio::test]
    async fn only_a_few_new_p2p_shard_urls_are_accepted_per_exchange() {
        let adm = admission_for_tests(false, |_| {});
        let entries: Vec<ShardAnnouncement> = (0..MAX_P2P_SHARD_URLS_PER_EXCHANGE + 3)
            .map(|i| ShardAnnouncement {
                shard_id: format!("s{i}"),
                url: crate::node_http::p2p_base_url(&fresh_peer_id()),
                last_seen_at: OffsetDateTime::now_utc(),
            })
            .collect();
        let (kept, refused) = validated_shards(&adm, &ShardRegistry::new(), &entries).await;
        assert_eq!(kept.len(), MAX_P2P_SHARD_URLS_PER_EXCHANGE);
        assert_eq!(refused, 3);
    }

    #[test]
    fn a_pooled_p2p_entry_never_replaces_an_existing_main_entry_when_promoted() {
        let adm = admission_for_tests(false, |_| {});
        let id = fresh_peer_id();
        let table = PeerTable::new();
        table.upsert(p2p_entry(&id, true));
        let mut pooled = p2p_entry(&id, false);
        pooled.roles = vec!["indexer".into()];
        table.insert_unverified(pooled.clone());
        table.promote_unverified_by_libp2p_peer(&id.to_string(), 10);
        assert_eq!(table.list_all()[0].roles, vec!["combined".to_string()]);
        table.insert_unverified(pooled);
        promote_on_contact(&table, &adm, &crate::node_http::p2p_base_url(&id));
        assert_eq!(table.list_all()[0].roles, vec!["combined".to_string()]);
    }
    #[test]
    fn an_http_hello_from_a_url_less_node_promotes_nothing() {
        let adm = admission_for_tests(false, |_| {});
        let table = PeerTable::new();
        let url = "http://127.0.0.1:1";
        let response = AnnounceResponse {
            peers: vec![],
            known_shards: vec![],
            head_summaries: vec![],
            witness: None,
            coordinate: Coordinate::default(),
            node: None,
        };
        table.insert_unverified(supported(url, OffsetDateTime::now_utc()));
        let n = table.neighbors().clone();
        record_contact(
            &n,
            &table,
            &adm,
            url,
            &response,
            Duration::from_millis(5),
            true,
        );
        assert!(!table.contains(url));
        record_contact(
            &n,
            &table,
            &adm,
            url,
            &response,
            Duration::from_millis(5),
            false,
        );
        assert!(table.contains(url));
    }
    #[test]
    fn a_p2p_newcomer_at_the_cap_never_evicts_an_http_peer() {
        let table = PeerTable::new();
        let now = OffsetDateTime::now_utc();
        table
            .insert_bounded(supported("http://127.0.0.1:1", now), 1)
            .unwrap();
        let err = table.insert_bounded(p2p_entry(&fresh_peer_id(), true), 1);
        assert_eq!(err, Err(TableFull));
        assert!(table.contains("http://127.0.0.1:1"));
        // A p2p entry can displace another p2p entry.
        let t2 = PeerTable::new();
        t2.insert_bounded(p2p_entry(&fresh_peer_id(), true), 1)
            .unwrap();
        assert!(t2
            .insert_bounded(p2p_entry(&fresh_peer_id(), true), 1)
            .unwrap()
            .is_some());
    }

    #[test]
    fn p2p_entries_cannot_flush_the_unverified_pool_of_http_entries() {
        let table = PeerTable::new();
        let now = OffsetDateTime::now_utc();
        for i in 0..MAX_UNVERIFIED_PEERS {
            table.insert_unverified(supported(
                &format!("http://127.0.0.1:{}", 1000 + i),
                now - time::Duration::hours(1),
            ));
        }
        for _ in 0..(MAX_P2P_UNVERIFIED * 3) {
            table.insert_unverified(p2p_entry(&fresh_peer_id(), false));
        }
        let pool = table.list_unverified();
        assert_eq!(pool.iter().filter(|p| is_p2p_url(&p.base_url)).count(), 0);
        assert_eq!(pool.len(), MAX_UNVERIFIED_PEERS);
        // With room, p2p entries are held up to their own cap.
        let roomy = PeerTable::new();
        for _ in 0..(MAX_P2P_UNVERIFIED + 5) {
            roomy.insert_unverified(p2p_entry(&fresh_peer_id(), false));
        }
        assert_eq!(roomy.unverified_len(), MAX_P2P_UNVERIFIED);
    }

    #[test]
    fn the_shard_registry_holds_a_bounded_total_of_p2p_urls() {
        let reg = ShardRegistry::new();
        let now = OffsetDateTime::now_utc();
        for i in 0..MAX_P2P_SHARD_URLS_TOTAL + 5 {
            reg.merge(&[ShardAnnouncement {
                shard_id: format!("s{i}"),
                url: crate::node_http::p2p_base_url(&fresh_peer_id()),
                last_seen_at: now + time::Duration::seconds(i as i64),
            }]);
        }
        reg.merge(&[ShardAnnouncement {
            shard_id: "http".into(),
            url: "http://a.test".into(),
            last_seen_at: now - time::Duration::days(1),
        }]);
        let snap = reg.snapshot();
        assert_eq!(
            snap.iter().filter(|a| is_p2p_url(&a.url)).count(),
            MAX_P2P_SHARD_URLS_TOTAL
        );
        assert!(snap.iter().any(|a| a.url == "http://a.test"));
        // The oldest p2p URLs went.
        assert!(!snap.iter().any(|a| a.shard_id == "s0"));
    }

    #[test]
    fn an_http_entry_naming_a_p2p_peers_id_is_still_bound() {
        let id = fresh_peer_id();
        let table = PeerTable::new();
        table.upsert(p2p_entry(&id, true));
        assert!(!table.is_bound_libp2p_peer(&id));
        table.upsert(table_entry("http://v.test", Some(&id), true));
        assert!(table.is_bound_libp2p_peer(&id));
    }

    #[tokio::test]
    async fn a_vouch_over_http_for_a_url_less_node_contacts_nobody() {
        let server = wiremock::MockServer::start().await;
        announce_mock(&server, 0).await;
        let adm = admission_for_tests(true, |_| {});
        let client = crate::node_http::NodeClient::new();
        let table = PeerTable::new();
        let url = normalized_base_url(&server.uri());
        let own = crate::node_http::p2p_base_url(&fresh_libp2p());
        vouch_contact(&client, &table, &adm, &url, &url, &announce_body(&own)).await;

        // With an http identity the same call does announce.
        let server = wiremock::MockServer::start().await;
        announce_mock(&server, 1).await;
        let url = normalized_base_url(&server.uri());
        vouch_contact(
            &client,
            &table,
            &adm,
            &url,
            &url,
            &announce_body("http://me.test"),
        )
        .await;
    }
}
