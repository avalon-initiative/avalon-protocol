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
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::AppError;
use crate::network_coordinates::Coordinate;
use crate::peer_admission::{admission, AdmitError, PeerAdmission};
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
        let mut evicted = None;
        if !peers.contains_key(&info.base_url) && peers.len() >= max {
            let victim = peers
                .values()
                .filter(|p| !protected.contains(&p.base_url))
                .min_by(|a, b| {
                    a.last_announced_at
                        .cmp(&b.last_announced_at)
                        .then_with(|| a.base_url.cmp(&b.base_url))
                })
                .map(|p| p.base_url.clone())
                .ok_or(TableFull)?;
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
        if !pool.contains_key(&info.base_url) && pool.len() >= MAX_UNVERIFIED_PEERS {
            if let Some(victim) = pool
                .values()
                .min_by(|a, b| {
                    a.last_announced_at
                        .cmp(&b.last_announced_at)
                        .then_with(|| a.base_url.cmp(&b.base_url))
                })
                .map(|p| p.base_url.clone())
            {
                pool.remove(&victim);
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

    /// Same expiry rule as [`Self::prune_older_than`], applied to the
    /// unverified pool: an entry that never gets promoted ages out.
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
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<AnnounceRequest>,
) -> Result<Json<AnnounceResponse>, AnnounceError> {
    if body.network_id != state.chain.network_id() {
        return Err(AppError::PeerNetworkMismatch.into());
    }

    let adm = admission();
    let caller_base_url = body.base_url.clone();
    let mut info = PeerInfo {
        base_url: body.base_url,
        roles: body.roles,
        protocol_version: body.protocol_version,
        network_id: body.network_id,
        last_announced_at: OffsetDateTime::now_utc(),
        libp2p_peer_id: body.libp2p_peer_id,
        libp2p_listen_addrs: body.libp2p_listen_addrs,
        witness: None,
    };
    if crate::version::is_supported(&info.protocol_version) {
        info.base_url = adm
            .check_shape(&info.base_url)
            .map_err(TopologyError::from)?;
        info.witness = verified_advert(&info.base_url, body.witness, info.last_announced_at);
        if state.peers.contains(&info.base_url) {
            state.peers.upsert(info);
        } else {
            admit_new_announcer(&state, adm, client_ip(source, &headers), info).await?;
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

    let own_witness = state
        .own_witness
        .as_ref()
        .zip(state.own_base_url.as_deref())
        .map(|(w, url)| w.advert(url, OffsetDateTime::now_utc()));
    Ok(Json(AnnounceResponse {
        witness: own_witness,
        peers: state.peers.list_excluding(&caller_base_url),
        known_shards: state.shard_registry.snapshot(),
        head_summaries: state.head_gossip.snapshot(),
        coordinate: state.peers.neighbors().own_coordinate(),
    }))
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
    if adm.cfg.verify_reachability {
        adm.verify_reachable(&checked, state.chain.network_id())
            .await?;
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
    let Some(info) = peers.take_unverified(contacted) else {
        return;
    };
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
        let Ok(base) = adm.check_shape(&info.base_url) else {
            skipped.rejected += 1;
            continue;
        };
        info.base_url = base;
        info.last_announced_at = info.last_announced_at.min(now);
        info.witness = verified_advert(&info.base_url, info.witness.take(), now);
        if !crate::version::is_supported(&info.protocol_version) {
            peers.admit_if_supported(info);
            continue;
        }
        if peers.contains(&info.base_url) {
            peers.upsert(info.clone());
            admitted.push(info);
            continue;
        }
        if accepted >= cap || examined >= examine_limit {
            skipped.over_limit += 1;
            continue;
        }
        examined += 1;
        if adm.check_address(&info.base_url).await.is_err() {
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
    let (mut examined, mut refused) = (0usize, 0usize);
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

    NodeStatusResponse {
        protocol_version: crate::version::PROTOCOL_VERSION.to_string(),
        network_id: state.chain.network_id().to_string(),
        roles: node_roles(),
        stale,
        newest_known_peer_version: newest_known_peer_version.map(|v| v.to_string()),
        resources,
        own_shard_replication,
        core_author_pinned: crate::core_author_guard::recorded_outcome(),
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

    let client = reqwest::Client::builder()
        .timeout(ANNOUNCE_TIMEOUT)
        .build()
        .unwrap_or_default();
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
                    announce_to(
                        &client,
                        peer,
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
                    Ok((discovered, rtt)) => {
                        neighbors.observe_coordinate(peer, &discovered.coordinate, rtt);
                        let adm = admission();
                        promote_on_contact(&peers, adm, peer);
                        if let Some(advert) = verified_advert(
                            &normalized_base_url(peer),
                            discovered.witness.clone(),
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
                    vouch_contact(&client, &peers, admission(), peer, &request).await;
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
    client: &reqwest::Client,
    peers: &PeerTable,
    adm: &PeerAdmission,
    peer: &str,
    request: &AnnounceRequest,
) {
    if adm.check_address(peer).await.is_err() {
        return;
    }
    let Ok(response) = announce_to(client, peer, request).await else {
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
            .map(|d| d.listen_addrs.clone())
            .unwrap_or_default(),
        known_shards: known_shards.to_vec(),
        head_summaries: head_summaries.to_vec(),
        witness,
        coordinate,
    }
}

async fn announce_to(
    client: &reqwest::Client,
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

    fn peer(base_url: &str, announced_at: OffsetDateTime) -> PeerInfo {
        PeerInfo {
            base_url: base_url.to_string(),
            roles: vec!["combined".to_string()],
            protocol_version: "0.1.0".to_string(),
            network_id: "avalon-dev-local".to_string(),
            last_announced_at: announced_at,
            libp2p_peer_id: None,
            libp2p_listen_addrs: Vec::new(),
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
        let client = reqwest::Client::new();
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
        let client = reqwest::Client::new();
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
            base_url: base_url.to_string(),
            roles: vec!["combined".to_string()],
            protocol_version: protocol_version.to_string(),
            network_id: "avalon-dev-local".to_string(),
            last_announced_at: OffsetDateTime::now_utc(),
            libp2p_peer_id: None,
            libp2p_listen_addrs: Vec::new(),
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
        };
        let json = serde_json::to_value(&response).expect("must serialize even when empty");
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
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
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
            vouch_contact(&client, &table, &adm, t, &request).await;
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
}
