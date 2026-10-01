//! Circuit relay v2 settings and state: the opt-in, bounded relay server role and the client
//! role that keeps reservations for a node AutoNAT found `private`.
//!
//! The libp2p behaviours live in the DHT swarm (`crate::dht`). This module holds what can be
//! decided without a swarm: env parsing, limit mapping, relay candidate choice and the
//! reservation bookkeeping published into [`crate::reachability::ReachabilityHandle`].

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use libp2p::core::transport::ListenerId;
use libp2p::multiaddr::Protocol;
use libp2p::swarm::NetworkBehaviour;
use libp2p::{relay, Multiaddr, PeerId, Swarm};
use serde::Serialize;

use crate::neighbors::NeighborSnapshot;
use crate::nodes::{PeerInfo, PeerTable};
use crate::outbound_policy::{always_forbidden, is_private, OutboundPolicy};
use crate::reachability::{Reachability, ReachabilityHandle, RelayReservation};

const DEFAULT_SERVER_MAX_RESERVATIONS: u64 = 128;
const DEFAULT_SERVER_MAX_RESERVATIONS_PER_PEER: u64 = 2;
const DEFAULT_SERVER_RESERVATION_SECS: u64 = 60 * 60;
const DEFAULT_SERVER_MAX_CIRCUITS: u64 = 16;
const DEFAULT_SERVER_MAX_CIRCUITS_PER_PEER: u64 = 4;
const DEFAULT_SERVER_CIRCUIT_SECS: u64 = 120;
const DEFAULT_SERVER_CIRCUIT_BYTES: u64 = 512 * 1024;
const DEFAULT_CLIENT_MAX_RESERVATIONS: u64 = 2;
const DEFAULT_CLIENT_BACKOFF_MAX_SECS: u64 = 15 * 60;

/// Hard ceilings: a relay knob can be raised, never made effectively unbounded.
const MAX_SERVER_RESERVATIONS: u64 = 4096;
const MAX_SERVER_CIRCUITS: u64 = 1024;
const MAX_SERVER_RESERVATION_SECS: u64 = 24 * 60 * 60;
const MAX_SERVER_CIRCUIT_SECS: u64 = 60 * 60;
const MAX_SERVER_CIRCUIT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CLIENT_RESERVATIONS: u64 = 8;

/// Most relay candidates a client remembers.
const MAX_CANDIDATES: usize = 64;

/// Measured round trips closer than this are treated as equal so outcome history can decide.
const LATENCY_BUCKET_MS: f64 = 10.0;

/// Most lost reservations that count against a relay's history.
const MAX_PENALISED_DROPS: u32 = 4;

/// Limits a relay server enforces, all finite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayServerSettings {
    pub max_reservations: usize,
    pub max_reservations_per_peer: usize,
    pub reservation_duration: Duration,
    pub max_circuits: usize,
    pub max_circuits_per_peer: usize,
    pub max_circuit_duration: Duration,
    pub max_circuit_bytes: u64,
}

impl Default for RelayServerSettings {
    fn default() -> Self {
        Self {
            max_reservations: DEFAULT_SERVER_MAX_RESERVATIONS as usize,
            max_reservations_per_peer: DEFAULT_SERVER_MAX_RESERVATIONS_PER_PEER as usize,
            reservation_duration: Duration::from_secs(DEFAULT_SERVER_RESERVATION_SECS),
            max_circuits: DEFAULT_SERVER_MAX_CIRCUITS as usize,
            max_circuits_per_peer: DEFAULT_SERVER_MAX_CIRCUITS_PER_PEER as usize,
            max_circuit_duration: Duration::from_secs(DEFAULT_SERVER_CIRCUIT_SECS),
            max_circuit_bytes: DEFAULT_SERVER_CIRCUIT_BYTES,
        }
    }
}

impl RelayServerSettings {
    /// The libp2p relay configuration these limits describe; libp2p's default per-peer and
    /// per-IP request rate limiters stay in place.
    pub fn libp2p_config(&self) -> relay::Config {
        relay::Config {
            max_reservations: self.max_reservations,
            max_reservations_per_peer: self.max_reservations_per_peer,
            reservation_duration: self.reservation_duration,
            max_circuits: self.max_circuits,
            max_circuits_per_peer: self.max_circuits_per_peer,
            max_circuit_duration: self.max_circuit_duration,
            max_circuit_bytes: self.max_circuit_bytes,
            ..relay::Config::default()
        }
    }
}

/// How a `private` node finds and keeps relay reservations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayClientSettings {
    /// `AVALON_RELAY_CLIENT_ENABLED` (default true).
    pub enabled: bool,
    /// Reservations to hold at once.
    pub max_reservations: usize,
    /// Operator-provided relays, each a multiaddr ending in `/p2p/<relay peer id>`.
    pub relay_addrs: Vec<Multiaddr>,
    /// Whether private and loopback relay addresses may be used.
    pub allow_private: bool,
    /// How long a relay that failed once is skipped; doubles with each consecutive failure.
    pub retry_backoff: Duration,
    /// Upper bound of the doubling backoff.
    pub retry_backoff_max: Duration,
    /// How long a reservation may stay unanswered before it is abandoned.
    pub pending_timeout: Duration,
    /// How often reservations are reconciled with what is wanted.
    pub reconcile_interval: Duration,
}

impl Default for RelayClientSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_reservations: DEFAULT_CLIENT_MAX_RESERVATIONS as usize,
            relay_addrs: Vec::new(),
            allow_private: false,
            retry_backoff: Duration::from_secs(30),
            retry_backoff_max: Duration::from_secs(DEFAULT_CLIENT_BACKOFF_MAX_SECS),
            pending_timeout: Duration::from_secs(30),
            reconcile_interval: Duration::from_secs(5),
        }
    }
}

/// Relay settings resolved once at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaySettings {
    /// `Some` when this node serves as a relay (`AVALON_RELAY_SERVER_ENABLED`, default off).
    pub server: Option<RelayServerSettings>,
    pub client: RelayClientSettings,
    /// `AVALON_DCUTR_ENABLED` (default true): upgrade relayed connections by hole punching.
    pub hole_punching: bool,
}

impl Default for RelaySettings {
    fn default() -> Self {
        Self {
            server: None,
            client: RelayClientSettings::default(),
            hole_punching: true,
        }
    }
}

fn env_flag(name: &str, default: bool) -> bool {
    std::env::var(name)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "true" | "1" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

/// An integer knob that must be within `1..=max`; zero and oversized values are refused.
pub(crate) fn bounded_env(name: &str, default: u64, max: u64) -> Result<u64, String> {
    let Ok(raw) = std::env::var(name) else {
        return Ok(default);
    };
    match raw.trim().parse::<u64>() {
        Ok(v) if (1..=max).contains(&v) => Ok(v),
        _ => Err(format!(
            "{name} must be an integer from 1 to {max}, got {raw:?}"
        )),
    }
}

/// A `/p2p/<id>`-terminated relay address split into its peer id and dial address.
fn split_relay_addr(addr: &Multiaddr) -> Option<(PeerId, Multiaddr)> {
    let mut dial = addr.clone();
    match dial.pop()? {
        Protocol::P2p(peer) if !dial.is_empty() && !is_relayed(&dial) => Some((peer, dial)),
        _ => None,
    }
}

fn is_relayed(addr: &Multiaddr) -> bool {
    addr.iter().any(|p| matches!(p, Protocol::P2pCircuit))
}

impl RelaySettings {
    /// Server: `AVALON_RELAY_SERVER_ENABLED` (default false), `AVALON_RELAY_MAX_RESERVATIONS`
    /// (128), `AVALON_RELAY_MAX_RESERVATIONS_PER_PEER` (2), `AVALON_RELAY_RESERVATION_SECS`
    /// (3600), `AVALON_RELAY_MAX_CIRCUITS` (16), `AVALON_RELAY_MAX_CIRCUITS_PER_PEER` (4),
    /// `AVALON_RELAY_MAX_CIRCUIT_SECS` (120), `AVALON_RELAY_MAX_CIRCUIT_BYTES` (524288).
    /// Client: `AVALON_RELAY_CLIENT_ENABLED` (true), `AVALON_RELAY_CLIENT_MAX_RESERVATIONS` (2),
    /// `AVALON_RELAY_ADDRS` (comma-separated multiaddrs ending in `/p2p/<peer id>`).
    /// Every limit is at least 1 and has a ceiling.
    pub fn from_env(policy: OutboundPolicy) -> Result<Self, String> {
        let server = if env_flag("AVALON_RELAY_SERVER_ENABLED", false) {
            let per_peer = bounded_env(
                "AVALON_RELAY_MAX_RESERVATIONS_PER_PEER",
                DEFAULT_SERVER_MAX_RESERVATIONS_PER_PEER,
                MAX_SERVER_RESERVATIONS,
            )?;
            let circuits_per_peer = bounded_env(
                "AVALON_RELAY_MAX_CIRCUITS_PER_PEER",
                DEFAULT_SERVER_MAX_CIRCUITS_PER_PEER,
                MAX_SERVER_CIRCUITS,
            )?;
            Some(RelayServerSettings {
                max_reservations: bounded_env(
                    "AVALON_RELAY_MAX_RESERVATIONS",
                    DEFAULT_SERVER_MAX_RESERVATIONS,
                    MAX_SERVER_RESERVATIONS,
                )? as usize,
                max_reservations_per_peer: per_peer as usize,
                reservation_duration: Duration::from_secs(bounded_env(
                    "AVALON_RELAY_RESERVATION_SECS",
                    DEFAULT_SERVER_RESERVATION_SECS,
                    MAX_SERVER_RESERVATION_SECS,
                )?),
                max_circuits: bounded_env(
                    "AVALON_RELAY_MAX_CIRCUITS",
                    DEFAULT_SERVER_MAX_CIRCUITS,
                    MAX_SERVER_CIRCUITS,
                )? as usize,
                max_circuits_per_peer: circuits_per_peer as usize,
                max_circuit_duration: Duration::from_secs(bounded_env(
                    "AVALON_RELAY_MAX_CIRCUIT_SECS",
                    DEFAULT_SERVER_CIRCUIT_SECS,
                    MAX_SERVER_CIRCUIT_SECS,
                )?),
                max_circuit_bytes: bounded_env(
                    "AVALON_RELAY_MAX_CIRCUIT_BYTES",
                    DEFAULT_SERVER_CIRCUIT_BYTES,
                    MAX_SERVER_CIRCUIT_BYTES,
                )?,
            })
        } else {
            None
        };

        let mut relay_addrs = Vec::new();
        for raw in std::env::var("AVALON_RELAY_ADDRS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let addr: Multiaddr = raw
                .parse()
                .map_err(|e| format!("AVALON_RELAY_ADDRS entry {raw:?} is not a multiaddr: {e}"))?;
            if split_relay_addr(&addr).is_none() {
                return Err(format!(
                    "AVALON_RELAY_ADDRS entry {raw:?} must be a direct address ending in /p2p/<peer id>"
                ));
            }
            relay_addrs.push(addr);
        }

        Ok(Self {
            server,
            hole_punching: env_flag("AVALON_DCUTR_ENABLED", true),
            client: RelayClientSettings {
                enabled: env_flag("AVALON_RELAY_CLIENT_ENABLED", true),
                max_reservations: bounded_env(
                    "AVALON_RELAY_CLIENT_MAX_RESERVATIONS",
                    DEFAULT_CLIENT_MAX_RESERVATIONS,
                    MAX_CLIENT_RESERVATIONS,
                )? as usize,
                relay_addrs,
                allow_private: policy.allow_private,
                ..RelayClientSettings::default()
            },
        })
    }
}

/// Counters for the relay server role, shared with whoever holds the [`crate::dht::DhtHandle`].
#[derive(Debug, Default)]
pub struct RelayServerStats {
    reservations_accepted: AtomicU64,
    reservations_denied: AtomicU64,
    reservations_closed: AtomicU64,
    reservations_timed_out: AtomicU64,
    circuits_accepted: AtomicU64,
    circuits_denied: AtomicU64,
    circuits_closed: AtomicU64,
    circuits_closed_with_error: AtomicU64,
}

/// Point-in-time copy of [`RelayServerStats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct RelayServerCounts {
    pub reservations_accepted: u64,
    pub reservations_denied: u64,
    pub reservations_closed: u64,
    pub reservations_timed_out: u64,
    pub circuits_accepted: u64,
    pub circuits_denied: u64,
    pub circuits_closed: u64,
    pub circuits_closed_with_error: u64,
}

/// The limits a relay enforces, as reported to operators.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelayServerLimits {
    pub max_reservations: usize,
    pub max_reservations_per_peer: usize,
    pub reservation_secs: u64,
    pub max_circuits: usize,
    pub max_circuits_per_peer: usize,
    pub max_circuit_secs: u64,
    pub max_circuit_bytes: u64,
}

/// What the relay is doing now against those limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelayServerUsage {
    /// Reservations held right now.
    pub reservations_active: u64,
    /// Circuits open right now.
    pub circuits_active: u64,
    /// Totals since this node started.
    #[serde(flatten)]
    pub totals: RelayServerCounts,
}

/// Operator view of the relay server role: its limits and its usage. Bytes carried are bounded by
/// `max_circuit_bytes` per circuit but are not measured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelayServerStatus {
    pub limits: RelayServerLimits,
    pub usage: RelayServerUsage,
}

/// The relay server's limits together with its live counters.
#[derive(Debug, Clone)]
pub struct RelayServerView {
    pub settings: RelayServerSettings,
    pub stats: Arc<RelayServerStats>,
}

impl RelayServerView {
    pub fn status(&self) -> RelayServerStatus {
        let totals = self.stats.counts();
        let s = &self.settings;
        RelayServerStatus {
            limits: RelayServerLimits {
                max_reservations: s.max_reservations,
                max_reservations_per_peer: s.max_reservations_per_peer,
                reservation_secs: s.reservation_duration.as_secs(),
                max_circuits: s.max_circuits,
                max_circuits_per_peer: s.max_circuits_per_peer,
                max_circuit_secs: s.max_circuit_duration.as_secs(),
                max_circuit_bytes: s.max_circuit_bytes,
            },
            usage: RelayServerUsage {
                reservations_active: totals
                    .reservations_accepted
                    .saturating_sub(totals.reservations_closed + totals.reservations_timed_out),
                circuits_active: totals
                    .circuits_accepted
                    .saturating_sub(totals.circuits_closed),
                totals,
            },
        }
    }
}

impl RelayServerStats {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn counts(&self) -> RelayServerCounts {
        let load = |c: &AtomicU64| c.load(Ordering::Relaxed);
        RelayServerCounts {
            reservations_accepted: load(&self.reservations_accepted),
            reservations_denied: load(&self.reservations_denied),
            reservations_closed: load(&self.reservations_closed),
            reservations_timed_out: load(&self.reservations_timed_out),
            circuits_accepted: load(&self.circuits_accepted),
            circuits_denied: load(&self.circuits_denied),
            circuits_closed: load(&self.circuits_closed),
            circuits_closed_with_error: load(&self.circuits_closed_with_error),
        }
    }

    pub(crate) fn record(&self, event: &relay::Event) {
        let bump = |c: &AtomicU64| c.fetch_add(1, Ordering::Relaxed);
        match event {
            relay::Event::ReservationReqAccepted { .. } => bump(&self.reservations_accepted),
            relay::Event::ReservationReqDenied { status, .. } => {
                tracing::info!(?status, "avalon-relay: reservation denied");
                bump(&self.reservations_denied)
            }
            relay::Event::ReservationClosed { .. } => bump(&self.reservations_closed),
            relay::Event::ReservationTimedOut { .. } => bump(&self.reservations_timed_out),
            relay::Event::CircuitReqAccepted { .. } => bump(&self.circuits_accepted),
            relay::Event::CircuitReqDenied { status, .. } => {
                tracing::info!(?status, "avalon-relay: circuit denied");
                bump(&self.circuits_denied)
            }
            relay::Event::CircuitClosed { error, .. } => {
                if error.is_some() {
                    bump(&self.circuits_closed_with_error);
                }
                bump(&self.circuits_closed)
            }
            _ => return,
        };
    }
}

/// `base` doubled for each consecutive failure after the first, capped at `max`.
fn backoff_delay(base: Duration, max: Duration, failures: usize) -> Duration {
    let doublings = failures.saturating_sub(1).min(32) as u32;
    base.saturating_mul(1u32 << doublings.min(31)).min(max)
}

/// Announce EWMAs (ms) of neighbors with a uniquely claimed, connected libp2p id; a forged
/// duplicate claim can only drop a relay back to unknown.
pub fn neighbor_latencies(peers: &PeerTable) -> Vec<(PeerId, f64)> {
    let entries: Vec<PeerInfo> = peers
        .list_all()
        .into_iter()
        .chain(peers.list_unverified())
        .collect();
    let ids = attributable_ids(&entries, |id| peers.paths().path(id).is_some());
    latencies_from(&peers.neighbors().snapshot(), &ids)
}

/// `base_url` to libp2p id for bound entries whose id no other entry claims and that is
/// connected.
fn attributable_ids(
    entries: &[PeerInfo],
    connected: impl Fn(&PeerId) -> bool,
) -> HashMap<String, PeerId> {
    let mut claims: HashMap<&str, usize> = HashMap::new();
    for id in entries.iter().filter_map(|p| p.libp2p_peer_id.as_deref()) {
        *claims.entry(id).or_default() += 1;
    }
    entries
        .iter()
        .filter(|p| p.identity_bound)
        .filter_map(|p| {
            let raw = p.libp2p_peer_id.as_deref()?;
            let id: PeerId = raw.parse().ok()?;
            (claims[raw] == 1 && connected(&id)).then(|| (p.base_url.clone(), id))
        })
        .collect()
}

fn latencies_from(
    neighbors: &[NeighborSnapshot],
    ids: &HashMap<String, PeerId>,
) -> Vec<(PeerId, f64)> {
    neighbors
        .iter()
        .filter_map(|n| {
            let ms = n.round_trip.as_ref()?.ewma_ms?;
            Some((*ids.get(&n.base_url)?, ms))
        })
        .collect()
}

#[derive(Debug, Clone)]
struct Candidate {
    /// Direct addresses of the relay, sorted, without the `/p2p/<id>` suffix.
    addrs: Vec<Multiaddr>,
    /// Operator-listed relays (by list position) come before discovered ones.
    rank: (u8, usize),
    /// Consecutive failures; drives the backoff and resets on an accepted reservation.
    failures: usize,
    retry_at: Option<Instant>,
    /// Latest measured or coordinate-estimated round trip to the relay, in milliseconds.
    latency_ms: Option<f64>,
    /// Reservations accepted, renewals seen, and accepted reservations later lost (halved on
    /// each renewal, so an old loss fades).
    accepts: u32,
    renewals: u32,
    drops: u32,
    /// Free reservation slots the relay claims; only breaks ties between otherwise equal relays.
    claimed_capacity: Option<u32>,
}

impl Candidate {
    fn new(addrs: Vec<Multiaddr>, rank: (u8, usize)) -> Self {
        Self {
            addrs,
            rank,
            failures: 0,
            retry_at: None,
            latency_ms: None,
            accepts: 0,
            renewals: 0,
            drops: 0,
            claimed_capacity: None,
        }
    }

    /// The address the next reservation attempt dials, rotating after failures.
    fn dial_addr(&self) -> &Multiaddr {
        &self.addrs[self.failures % self.addrs.len()]
    }

    /// Outcome score: accepted reservations and renewals count for the relay, drops against it.
    fn history(&self) -> i64 {
        i64::from(self.accepts) + i64::from(self.renewals.min(16))
            - 2 * i64::from(self.drops.min(MAX_PENALISED_DROPS))
    }
}

/// Network neighbourhood of a relay address, used so two reservations never share one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Neighbourhood {
    V4([u8; 3]),
    V6([u8; 6]),
    /// A relay with no IP address (DNS name) is its own group per host name.
    Host(String),
}

/// IPv4 /24, IPv6 /48 or DNS host name. Sybils across prefixes, NAT64 and self-reported
/// addresses are not defended against.
fn neighbourhood(addr: &Multiaddr) -> Option<Neighbourhood> {
    addr.iter().find_map(|p| match p {
        Protocol::Ip4(ip) => Some(ip_neighbourhood(ip.into())),
        Protocol::Ip6(ip) => Some(ip_neighbourhood(ip.into())),
        Protocol::Dns(h) | Protocol::Dns4(h) | Protocol::Dns6(h) | Protocol::Dnsaddr(h) => Some(
            Neighbourhood::Host(h.trim_end_matches('.').to_ascii_lowercase()),
        ),
        _ => None,
    })
}

fn ip_neighbourhood(ip: IpAddr) -> Neighbourhood {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            Neighbourhood::V4([o[0], o[1], o[2]])
        }
        IpAddr::V6(v6) => {
            let o = v6.octets();
            Neighbourhood::V6([o[0], o[1], o[2], o[3], o[4], o[5]])
        }
    }
}

#[derive(Debug)]
struct Slot {
    listener_id: ListenerId,
    relayed_addr: Multiaddr,
    neighbourhood: Option<Neighbourhood>,
    accepted: bool,
    renewals: u64,
    started: Instant,
}

/// Chooses relays and tracks reservations for a `private` node: skip relays in backoff, prefer a
/// new neighbourhood, then operator list order, then round trip, history, capacity, peer id.
pub struct RelayClient {
    settings: RelayClientSettings,
    local_peer: PeerId,
    candidates: BTreeMap<PeerId, Candidate>,
    slots: BTreeMap<PeerId, Slot>,
    handle: ReachabilityHandle,
}

impl RelayClient {
    pub fn new(
        settings: RelayClientSettings,
        local_peer: PeerId,
        handle: ReachabilityHandle,
    ) -> Self {
        let mut client = Self {
            settings,
            local_peer,
            candidates: BTreeMap::new(),
            slots: BTreeMap::new(),
            handle,
        };
        for (index, addr) in client.settings.relay_addrs.clone().into_iter().enumerate() {
            if let Some((peer, dial)) = split_relay_addr(&addr) {
                client.add_candidate(peer, vec![dial], (0, index));
            }
        }
        client
    }

    fn addr_allowed(&self, addr: &Multiaddr) -> bool {
        addr.iter().all(|p| {
            let ip = match p {
                Protocol::Ip4(ip) => ip.into(),
                Protocol::Ip6(ip) => ip.into(),
                _ => return true,
            };
            !always_forbidden(ip) && (self.settings.allow_private || !is_private(ip))
        })
    }

    fn add_candidate(&mut self, peer: PeerId, addrs: Vec<Multiaddr>, rank: (u8, usize)) {
        let mut addrs: Vec<Multiaddr> = addrs
            .into_iter()
            .filter(|a| !is_relayed(a) && self.addr_allowed(a))
            .collect();
        addrs.sort_by_key(|a| a.to_string());
        addrs.dedup();
        if addrs.is_empty() || peer == self.local_peer {
            return;
        }
        if let Some(existing) = self.candidates.get_mut(&peer) {
            if rank.0 > existing.rank.0 {
                return;
            }
            existing.addrs = addrs;
            return;
        }
        if self.candidates.len() >= MAX_CANDIDATES {
            return;
        }
        self.candidates.insert(peer, Candidate::new(addrs, rank));
    }

    /// Replaces the known relay round trips (milliseconds); relays not listed become unknown.
    pub fn set_latencies(&mut self, latencies: impl IntoIterator<Item = (PeerId, f64)>) {
        for c in self.candidates.values_mut() {
            c.latency_ms = None;
        }
        for (peer, ms) in latencies {
            if let Some(c) = self.candidates.get_mut(&peer) {
                c.latency_ms = Some(ms).filter(|m| m.is_finite() && *m >= 0.0);
            }
        }
    }

    /// Records the free reservation slots a relay reports for itself.
    pub fn note_claimed_capacity(&mut self, peer: PeerId, free_reservations: u32) {
        if let Some(c) = self.candidates.get_mut(&peer) {
            c.claimed_capacity = Some(free_reservations);
        }
    }

    /// A connected peer advertised the relay hop protocol; `addrs` are the addresses it reported.
    pub fn note_hop_relay(&mut self, peer: PeerId, addrs: Vec<Multiaddr>) {
        self.add_candidate(peer, addrs, (1, 0));
    }

    /// Ordering key, smallest first: operator list before discovered, then latency bucket
    /// (unknown last), history, list position, claimed capacity, peer id.
    fn rank_key(peer: PeerId, c: &Candidate) -> impl Ord {
        let bucket = c.latency_ms.map(|ms| (ms / LATENCY_BUCKET_MS) as u64);
        (
            c.rank.0,
            c.rank.1,
            bucket.is_none(),
            bucket,
            Reverse(c.history()),
            Reverse(c.claimed_capacity),
            peer,
        )
    }

    /// Of the relays not held and not in backoff, the best one outside every held
    /// neighbourhood; when none is outside, the best one overall.
    fn next_candidate(&self, now: Instant) -> Option<PeerId> {
        let held: HashSet<Neighbourhood> = self
            .slots
            .values()
            .filter_map(|s| s.neighbourhood.clone())
            .collect();
        let eligible = || {
            self.candidates.iter().filter(|(peer, c)| {
                !self.slots.contains_key(peer) && c.retry_at.is_none_or(|t| now >= t)
            })
        };
        let best = |diverse_only: bool| {
            eligible()
                .filter(|(_, c)| {
                    !diverse_only || neighbourhood(c.dial_addr()).is_none_or(|n| !held.contains(&n))
                })
                .min_by_key(|(peer, c)| Self::rank_key(**peer, c))
                .map(|(peer, _)| *peer)
        };
        best(true).or_else(|| best(false))
    }

    /// Whether a reconcile would try to fill a slot now.
    pub fn wants_slots(&self, reachability: Reachability) -> bool {
        self.settings.enabled
            && reachability == Reachability::Private
            && self.slots.len() < self.settings.max_reservations
            && !self.candidates.is_empty()
    }

    /// Brings reservations in line with `reachability`: none unless `private`, up to the
    /// configured count when it is.
    pub fn reconcile<B: NetworkBehaviour>(
        &mut self,
        swarm: &mut Swarm<B>,
        reachability: Reachability,
        now: Instant,
    ) {
        if !self.settings.enabled || reachability == Reachability::Public {
            self.release_all(swarm);
            return;
        }
        if reachability != Reachability::Private {
            return;
        }

        let timed_out: Vec<PeerId> = self
            .slots
            .iter()
            .filter(|(_, s)| {
                !s.accepted && now.duration_since(s.started) >= self.settings.pending_timeout
            })
            .map(|(p, _)| *p)
            .collect();
        for peer in timed_out {
            tracing::warn!(%peer, "avalon-relay: reservation unanswered, abandoning it");
            if let Some(slot) = self.slots.remove(&peer) {
                swarm.remove_listener(slot.listener_id);
            }
            self.mark_failed(peer, now, false);
        }

        while self.slots.len() < self.settings.max_reservations {
            let Some(peer) = self.next_candidate(now) else {
                break;
            };
            let candidate = &self.candidates[&peer];
            let dial = candidate.dial_addr().clone();
            let group = neighbourhood(&dial);
            let circuit = dial
                .clone()
                .with(Protocol::P2p(peer))
                .with(Protocol::P2pCircuit);
            match swarm.listen_on(circuit) {
                Ok(listener_id) => {
                    let relayed_addr = dial
                        .with(Protocol::P2p(peer))
                        .with(Protocol::P2pCircuit)
                        .with(Protocol::P2p(self.local_peer));
                    tracing::info!(%peer, "avalon-relay: reserving a slot");
                    self.slots.insert(
                        peer,
                        Slot {
                            listener_id,
                            relayed_addr,
                            neighbourhood: group,
                            accepted: false,
                            renewals: 0,
                            started: now,
                        },
                    );
                }
                Err(e) => {
                    tracing::warn!(%peer, "avalon-relay: cannot listen through relay: {e}");
                    self.mark_failed(peer, now, false);
                }
            }
        }
        self.publish();
    }

    fn release_all<B: NetworkBehaviour>(&mut self, swarm: &mut Swarm<B>) {
        if self.slots.is_empty() {
            return;
        }
        for (_, slot) in std::mem::take(&mut self.slots) {
            swarm.remove_listener(slot.listener_id);
        }
        self.publish();
    }

    /// Backs the relay off; `lost` is true only for an accepted reservation that ended, the one
    /// outcome that counts against the relay's history.
    fn mark_failed(&mut self, peer: PeerId, now: Instant, lost: bool) {
        let (base, max) = (self.settings.retry_backoff, self.settings.retry_backoff_max);
        if let Some(c) = self.candidates.get_mut(&peer) {
            c.failures += 1;
            if lost {
                c.drops = c.drops.saturating_add(1);
            }
            c.retry_at = Some(now + backoff_delay(base, max, c.failures));
        }
    }

    /// The relay accepted or renewed a reservation.
    pub fn on_accepted(&mut self, relay_peer: PeerId, renewal: bool) {
        let Some(slot) = self.slots.get_mut(&relay_peer) else {
            return;
        };
        slot.accepted = true;
        let candidate = self.candidates.get_mut(&relay_peer);
        if renewal {
            slot.renewals += 1;
            // Surviving to a renewal clears the backoff; accept-then-drop never does.
            if let Some(c) = candidate {
                c.renewals = c.renewals.saturating_add(1);
                c.drops /= 2;
                c.failures = 0;
                c.retry_at = None;
            }
        } else if let Some(c) = candidate {
            c.accepts = c.accepts.saturating_add(1);
        }
        tracing::info!(%relay_peer, renewal, "avalon-relay: reservation accepted");
        self.publish();
    }

    /// A reservation listener ended (refused, relay lost, or dial failed). The relay is skipped
    /// for the backoff period; the next reconcile picks another.
    pub fn on_listener_closed(&mut self, listener_id: ListenerId, now: Instant) {
        let Some(peer) = self
            .slots
            .iter()
            .find(|(_, s)| s.listener_id == listener_id)
            .map(|(p, _)| *p)
        else {
            return;
        };
        let lost = self.slots.remove(&peer).is_some_and(|s| s.accepted);
        tracing::warn!(relay = %peer, "avalon-relay: reservation lost");
        self.mark_failed(peer, now, lost);
        self.publish();
    }

    fn publish(&self) {
        self.handle.set_relay_reservations(
            self.slots
                .iter()
                .filter(|(_, s)| s.accepted)
                .map(|(peer, s)| RelayReservation {
                    relay_peer_id: peer.to_string(),
                    relayed_addr: s.relayed_addr.to_string(),
                    renewals: s.renewals,
                })
                .collect(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::identity::Keypair;
    use libp2p::swarm::ConnectionId;

    fn peer() -> PeerId {
        Keypair::generate_ed25519().public().into()
    }

    fn client(addrs: Vec<Multiaddr>, max: usize) -> RelayClient {
        RelayClient::new(
            RelayClientSettings {
                relay_addrs: addrs,
                max_reservations: max,
                allow_private: true,
                ..RelayClientSettings::default()
            },
            peer(),
            ReachabilityHandle::unknown(),
        )
    }

    fn with_peer(addr: &str, peer: PeerId) -> Multiaddr {
        format!("{addr}/p2p/{peer}").parse().unwrap()
    }

    #[test]
    fn operator_relays_come_first_in_list_order_then_discovered_by_peer_id() {
        let (a, b, d) = (peer(), peer(), peer());
        let mut c = client(
            vec![
                with_peer("/ip4/127.0.0.1/tcp/1", b),
                with_peer("/ip4/127.0.0.1/tcp/2", a),
            ],
            2,
        );
        c.note_hop_relay(d, vec!["/ip4/127.0.0.1/tcp/3".parse().unwrap()]);
        let now = Instant::now();
        assert_eq!(c.next_candidate(now), Some(b));
        c.candidates.remove(&b);
        assert_eq!(c.next_candidate(now), Some(a));
        c.candidates.remove(&a);
        assert_eq!(c.next_candidate(now), Some(d));
    }

    #[test]
    fn a_failed_relay_is_skipped_until_its_backoff_ends() {
        let (a, b) = (peer(), peer());
        let mut c = client(
            vec![
                with_peer("/ip4/127.0.0.1/tcp/1", a),
                with_peer("/ip4/127.0.0.1/tcp/2", b),
            ],
            1,
        );
        let now = Instant::now();
        c.mark_failed(a, now, false);
        assert_eq!(c.next_candidate(now), Some(b));
        let later = now + c.settings.retry_backoff;
        c.candidates.remove(&b);
        assert_eq!(c.next_candidate(now), None);
        assert_eq!(c.next_candidate(later), Some(a));
    }

    fn relay_at(c: &mut RelayClient, addr: &str) -> PeerId {
        let p = peer();
        c.note_hop_relay(p, vec![addr.parse().unwrap()]);
        p
    }

    fn hold(c: &mut RelayClient, peer: PeerId) {
        let group = neighbourhood(c.candidates[&peer].dial_addr());
        c.slots.insert(
            peer,
            Slot {
                listener_id: ListenerId::next(),
                relayed_addr: "/ip4/127.0.0.1/tcp/1/p2p-circuit".parse().unwrap(),
                neighbourhood: group,
                accepted: true,
                renewals: 0,
                started: Instant::now(),
            },
        );
    }

    /// A swarm whose relay client accepts `listen_on` of a circuit address without polling.
    fn test_swarm() -> Swarm<relay::client::Behaviour> {
        libp2p::SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .unwrap()
            .with_relay_client(libp2p::noise::Config::new, libp2p::yamux::Config::default)
            .unwrap()
            .with_behaviour(|_, relay_client| relay_client)
            .unwrap()
            .build()
    }

    #[test]
    fn the_lowest_latency_discovered_relay_is_chosen_and_unknown_ranks_last() {
        let mut c = client(vec![], 1);
        let slow = relay_at(&mut c, "/ip4/203.0.113.1/tcp/1");
        let fast = relay_at(&mut c, "/ip4/198.51.100.1/tcp/1");
        let unknown = relay_at(&mut c, "/ip4/192.0.2.1/tcp/1");
        c.set_latencies([(slow, 90.0), (fast, 12.0)]);
        let now = Instant::now();
        assert_eq!(c.next_candidate(now), Some(fast));
        c.candidates.remove(&fast);
        assert_eq!(c.next_candidate(now), Some(slow));
        c.candidates.remove(&slow);
        assert_eq!(c.next_candidate(now), Some(unknown));
    }

    #[test]
    fn operator_relays_stay_first_whatever_the_measurements() {
        let (first, second, found) = (peer(), peer(), peer());
        let mut c = client(
            vec![
                with_peer("/ip4/203.0.113.1/tcp/1", first),
                with_peer("/ip4/198.51.100.1/tcp/1", second),
            ],
            1,
        );
        c.note_hop_relay(found, vec!["/ip4/192.0.2.1/tcp/1".parse().unwrap()]);
        c.set_latencies([(first, 400.0), (second, 1.0), (found, 1.0)]);
        c.candidates.get_mut(&found).unwrap().accepts = 9;
        let now = Instant::now();
        assert_eq!(
            c.next_candidate(now),
            Some(first),
            "list order, not latency"
        );
        c.candidates.remove(&first);
        assert_eq!(c.next_candidate(now), Some(second));
        c.candidates.remove(&second);
        assert_eq!(c.next_candidate(now), Some(found));
    }

    #[test]
    fn latencies_within_a_bucket_are_decided_by_history() {
        let mut c = client(vec![], 1);
        let a = relay_at(&mut c, "/ip4/203.0.113.1/tcp/1");
        let b = relay_at(&mut c, "/ip4/198.51.100.1/tcp/1");
        c.set_latencies([(a, 11.0), (b, 18.0)]);
        c.candidates.get_mut(&b).unwrap().accepts = 3;
        assert_eq!(c.next_candidate(Instant::now()), Some(b));
        c.set_latencies([(a, 11.0), (b, 35.0)]);
        assert_eq!(
            c.next_candidate(Instant::now()),
            Some(a),
            "a wider gap wins"
        );
    }

    #[test]
    fn outcome_history_overrides_claimed_capacity() {
        let mut c = client(vec![], 1);
        let proven = relay_at(&mut c, "/ip4/203.0.113.1/tcp/1");
        let claims = relay_at(&mut c, "/ip4/198.51.100.1/tcp/1");
        c.note_claimed_capacity(proven, 0);
        c.note_claimed_capacity(claims, 1000);
        c.candidates.get_mut(&proven).unwrap().accepts = 2;
        c.candidates.get_mut(&claims).unwrap().drops = 2;
        assert_eq!(c.next_candidate(Instant::now()), Some(proven));
    }

    #[test]
    fn claimed_capacity_only_breaks_ties_and_unclaimed_ranks_last() {
        // Fresh random peer ids each round, so a missing capacity key cannot pass on id order.
        for _ in 0..16 {
            let mut c = client(vec![], 1);
            let a = relay_at(&mut c, "/ip4/203.0.113.1/tcp/1");
            let b = relay_at(&mut c, "/ip4/198.51.100.1/tcp/1");
            let none = relay_at(&mut c, "/ip4/192.0.2.1/tcp/1");
            c.note_claimed_capacity(a, 1);
            c.note_claimed_capacity(b, 50);
            let now = Instant::now();
            assert_eq!(c.next_candidate(now), Some(b));
            c.candidates.remove(&b);
            assert_eq!(c.next_candidate(now), Some(a), "a claim beats no claim");
            c.candidates.remove(&a);
            assert_eq!(c.next_candidate(now), Some(none));
            let b2 = relay_at(&mut c, "/ip4/198.18.0.1/tcp/1");
            c.candidates.get_mut(&none).unwrap().accepts = 1;
            c.note_claimed_capacity(b2, 99);
            assert_eq!(c.next_candidate(now), Some(none), "history first");
        }
    }

    #[test]
    fn non_finite_or_negative_latencies_stay_unknown() {
        let mut c = client(vec![], 1);
        let bad = relay_at(&mut c, "/ip4/203.0.113.1/tcp/1");
        let neg = relay_at(&mut c, "/ip4/203.0.114.1/tcp/1");
        let good = relay_at(&mut c, "/ip4/203.0.115.1/tcp/1");
        c.set_latencies([(bad, f64::NAN), (neg, -5.0), (good, 80.0)]);
        assert_eq!(c.candidates[&bad].latency_ms, None);
        assert_eq!(c.candidates[&neg].latency_ms, None);
        assert_eq!(c.next_candidate(Instant::now()), Some(good));
        c.set_latencies([(bad, f64::INFINITY)]);
        assert_eq!(c.candidates[&bad].latency_ms, None);
        assert_eq!(
            c.candidates[&good].latency_ms, None,
            "unlisted becomes unknown"
        );
    }

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let base = Duration::from_secs(30);
        let max = Duration::from_secs(300);
        let secs = |n| backoff_delay(base, max, n).as_secs();
        assert_eq!([secs(1), secs(2), secs(3), secs(4)], [30, 60, 120, 240]);
        assert_eq!([secs(5), secs(1000), secs(usize::MAX)], [300, 300, 300]);
    }

    #[test]
    fn accept_then_drop_keeps_growing_the_backoff_until_a_renewal() {
        let mut c = client(vec![], 1);
        let p = relay_at(&mut c, "/ip4/203.0.113.1/tcp/1");
        let d = RelayClientSettings::default();
        let now = Instant::now();
        for n in 1..=3 {
            hold(&mut c, p);
            c.slots.get_mut(&p).unwrap().accepted = false;
            c.on_accepted(p, false);
            let id = c.slots[&p].listener_id;
            c.on_listener_closed(id, now);
            let wait = c.candidates[&p].retry_at.unwrap() - now;
            assert_eq!(wait, backoff_delay(d.retry_backoff, d.retry_backoff_max, n));
        }
        hold(&mut c, p);
        c.on_accepted(p, true);
        let cand = &c.candidates[&p];
        assert_eq!(
            (cand.failures, cand.retry_at),
            (0, None),
            "a renewal clears it"
        );
        let id = c.slots[&p].listener_id;
        c.on_listener_closed(id, now);
        assert_eq!(c.candidates[&p].retry_at.unwrap() - now, d.retry_backoff);
    }

    #[test]
    fn only_a_lost_accepted_reservation_counts_against_the_relay() {
        let mut c = client(vec![], 1);
        let p = relay_at(&mut c, "/ip4/203.0.113.1/tcp/1");
        hold(&mut c, p);
        c.slots.get_mut(&p).unwrap().accepted = false;
        let id = c.slots[&p].listener_id;
        c.on_listener_closed(id, Instant::now());
        let cand = &c.candidates[&p];
        assert_eq!((cand.drops, cand.failures), (0, 1), "refused: backoff only");
        c.mark_failed(p, Instant::now(), false);
        assert_eq!(c.candidates[&p].drops, 0, "local errors are not drops");

        hold(&mut c, p);
        let id = c.slots[&p].listener_id;
        c.on_listener_closed(id, Instant::now());
        assert_eq!(c.candidates[&p].drops, 1);
        assert!(c.candidates[&p].history() < 0);
    }

    #[test]
    fn drops_fade_with_renewals_and_their_penalty_is_capped() {
        let mut c = client(vec![], 1);
        let p = relay_at(&mut c, "/ip4/203.0.113.1/tcp/1");
        c.candidates.get_mut(&p).unwrap().drops = 100;
        assert_eq!(
            c.candidates[&p].history(),
            -2 * i64::from(MAX_PENALISED_DROPS)
        );
        c.candidates.get_mut(&p).unwrap().drops = 8;
        hold(&mut c, p);
        c.on_accepted(p, true);
        assert_eq!(c.candidates[&p].drops, 4);
        c.on_accepted(p, true);
        assert_eq!(c.candidates[&p].drops, 2);
    }

    #[test]
    fn renewal_credit_is_capped() {
        let mut c = client(vec![], 1);
        let p = relay_at(&mut c, "/ip4/203.0.113.1/tcp/1");
        c.candidates.get_mut(&p).unwrap().renewals = 1000;
        assert_eq!(c.candidates[&p].history(), 16);
    }

    #[test]
    fn a_diverse_relay_is_preferred_over_a_better_ranked_same_24() {
        let mut c = client(vec![], 2);
        let first = relay_at(&mut c, "/ip4/203.0.113.10/tcp/1");
        let same = relay_at(&mut c, "/ip4/203.0.113.77/tcp/1");
        let other = relay_at(&mut c, "/ip4/203.0.114.10/tcp/1");
        c.set_latencies([(first, 5.0), (same, 6.0), (other, 400.0)]);
        hold(&mut c, first);
        assert_eq!(c.next_candidate(Instant::now()), Some(other));
        c.candidates.remove(&other);
        assert_eq!(
            c.next_candidate(Instant::now()),
            Some(same),
            "with no diverse relay the best same-/24 one is used"
        );
    }

    #[test]
    fn a_diverse_relay_in_backoff_does_not_block_the_fallback() {
        let mut c = client(vec![], 2);
        let first = relay_at(&mut c, "/ip4/203.0.113.10/tcp/1");
        let same = relay_at(&mut c, "/ip4/203.0.113.77/tcp/1");
        let other = relay_at(&mut c, "/ip4/203.0.114.10/tcp/1");
        hold(&mut c, first);
        let now = Instant::now();
        c.mark_failed(other, now, false);
        assert_eq!(c.next_candidate(now), Some(same));
    }

    #[test]
    fn ipv6_relays_are_grouped_by_48_and_mapped_ipv4_by_24() {
        let mut c = client(vec![], 2);
        let first = relay_at(&mut c, "/ip6/2001:db8:1:1::1/tcp/1");
        let same48 = relay_at(&mut c, "/ip6/2001:db8:1:ffff::9/tcp/1");
        let other48 = relay_at(&mut c, "/ip6/2001:db8:2:1::1/tcp/1");
        c.set_latencies([(same48, 1.0), (other48, 300.0)]);
        hold(&mut c, first);
        assert_eq!(c.next_candidate(Instant::now()), Some(other48));

        let v4: Multiaddr = "/ip4/203.0.113.9/tcp/1".parse().unwrap();
        let mapped: Multiaddr = "/ip6/::ffff:203.0.113.200/tcp/1".parse().unwrap();
        assert_eq!(neighbourhood(&v4), neighbourhood(&mapped));
    }

    #[test]
    fn dns_relays_are_grouped_by_normalised_host_name() {
        let n = |a: &str| neighbourhood(&a.parse().unwrap());
        let base = n("/dns4/relay.example.org/tcp/1");
        assert!(base.is_some());
        for same in [
            "/dns/Relay.Example.org/tcp/1",
            "/dns6/relay.example.org/tcp/2",
            "/dnsaddr/relay.example.org",
            "/dns4/relay.example.org./tcp/1",
        ] {
            assert_eq!(n(same), base, "{same}");
        }
        assert_ne!(n("/dns4/other.example.org/tcp/1"), base);
        assert_eq!(n("/memory/1"), None);
    }

    #[test]
    fn ranking_state_stays_within_the_candidate_cap() {
        let mut c = client(vec![], 1);
        for _ in 0..MAX_CANDIDATES + 10 {
            let p = peer();
            c.note_hop_relay(p, vec!["/ip4/203.0.113.1/tcp/1".parse().unwrap()]);
        }
        let strangers: Vec<(PeerId, f64)> = (0..100).map(|i| (peer(), f64::from(i))).collect();
        c.set_latencies(strangers.clone());
        for (p, _) in &strangers {
            c.note_claimed_capacity(*p, 1);
        }
        assert_eq!(c.candidates.len(), MAX_CANDIDATES);
        assert!(c.candidates.values().all(|k| k.latency_ms.is_none()));
    }

    #[tokio::test]
    async fn reconcile_prefers_a_diverse_relay_and_records_each_slots_neighbourhood() {
        let mut c = client(vec![], 2);
        let near = relay_at(&mut c, "/ip4/203.0.113.10/tcp/1");
        let near_twin = relay_at(&mut c, "/ip4/203.0.113.11/tcp/1");
        let far = relay_at(&mut c, "/ip4/198.51.100.10/tcp/1");
        c.set_latencies([(near, 5.0), (near_twin, 30.0), (far, 300.0)]);
        let mut swarm = test_swarm();
        c.reconcile(&mut swarm, Reachability::Private, Instant::now());
        let held: HashSet<PeerId> = c.slots.keys().copied().collect();
        assert_eq!(
            held,
            HashSet::from([near, far]),
            "pending slots count as held"
        );
        assert_eq!(
            c.slots[&near].neighbourhood,
            Some(Neighbourhood::V4([203, 0, 113]))
        );
        assert_eq!(
            c.slots[&far].neighbourhood,
            Some(Neighbourhood::V4([198, 51, 100]))
        );
    }

    #[tokio::test]
    async fn reconcile_falls_back_to_one_prefix_when_that_is_all_there_is() {
        let mut c = client(vec![], 2);
        for host in 10..13 {
            relay_at(&mut c, &format!("/ip4/203.0.113.{host}/tcp/1"));
        }
        let mut swarm = test_swarm();
        c.reconcile(&mut swarm, Reachability::Private, Instant::now());
        assert_eq!(c.slots.len(), 2);
    }

    #[test]
    fn slots_are_wanted_only_when_private_with_candidates_and_room() {
        let mut c = client(vec![], 1);
        assert!(!c.wants_slots(Reachability::Private), "no candidates");
        let p = relay_at(&mut c, "/ip4/203.0.113.1/tcp/1");
        assert!(c.wants_slots(Reachability::Private));
        assert!(!c.wants_slots(Reachability::Public));
        assert!(!c.wants_slots(Reachability::Unknown));
        hold(&mut c, p);
        assert!(!c.wants_slots(Reachability::Private), "full");
    }

    fn entry(url: &str, id: Option<&PeerId>, bound: bool) -> PeerInfo {
        PeerInfo {
            identity_bound: bound,
            base_url: url.to_string(),
            roles: vec!["combined".to_string()],
            protocol_version: crate::version::PROTOCOL_VERSION.to_string(),
            network_id: "n".to_string(),
            last_announced_at: time::OffsetDateTime::now_utc(),
            libp2p_peer_id: id.map(|i| i.to_string()),
            libp2p_listen_addrs: vec![],
            connectivity: None,
            witness: None,
        }
    }

    #[test]
    fn an_id_claimed_by_two_entries_gets_no_latency() {
        let (shared, solo) = (peer(), peer());
        let entries = [
            entry("http://honest.test", Some(&shared), true),
            entry("http://forger.test", Some(&shared), true),
            entry("http://solo.test", Some(&solo), true),
        ];
        let ids = attributable_ids(&entries, |_| true);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids["http://solo.test"], solo);
    }

    #[test]
    fn an_unverified_duplicate_claim_also_drops_the_id() {
        let id = peer();
        let entries = [
            entry("http://honest.test", Some(&id), true),
            entry("http://pooled.test", Some(&id), false),
        ];
        assert!(attributable_ids(&entries, |_| true).is_empty());
    }

    #[test]
    fn an_unconnected_unbound_or_unparsable_entry_gets_no_latency() {
        let (idle, unbound, live) = (peer(), peer(), peer());
        let mut garbled = entry("http://garbled.test", None, true);
        garbled.libp2p_peer_id = Some("not-a-peer-id".into());
        let entries = [
            entry("http://idle.test", Some(&idle), true),
            entry("http://unbound.test", Some(&unbound), false),
            entry("http://live.test", Some(&live), true),
            garbled,
            entry("http://none.test", None, true),
        ];
        let ids = attributable_ids(&entries, |id| *id != idle);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids["http://live.test"], live);
        assert!(!ids.contains_key("http://idle.test"));
    }

    #[test]
    fn neighbor_latency_comes_from_the_ewma_of_attributable_connected_neighbors() {
        let (good, forged, offline, unbound, pooled) = (peer(), peer(), peer(), peer(), peer());
        let peers = PeerTable::new();
        for e in [
            entry("http://good.test", Some(&good), true),
            entry("http://forged-a.test", Some(&forged), true),
            entry("http://forged-b.test", Some(&forged), true),
            entry("http://offline.test", Some(&offline), true),
            entry("http://unbound.test", Some(&unbound), false),
            entry("http://plain.test", None, true),
            entry("http://pooled-bound.test", Some(&pooled), true),
        ] {
            peers.upsert(e);
        }
        peers.insert_unverified(entry("http://pooled-forger.test", Some(&pooled), false));
        let urls: Vec<String> = [
            "http://good.test",
            "http://forged-a.test",
            "http://forged-b.test",
            "http://offline.test",
            "http://unbound.test",
            "http://plain.test",
            "http://pooled-bound.test",
        ]
        .map(String::from)
        .into();
        peers.neighbors().set_active(&urls, &[]);
        for u in &urls {
            peers
                .neighbors()
                .record_success(u, Duration::from_millis(40));
        }
        for (n, id) in [(1, good), (2, forged), (3, unbound), (4, pooled)] {
            peers
                .paths()
                .connection_opened(id, ConnectionId::new_unchecked(n), false);
        }
        let got = neighbor_latencies(&peers);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].0, good);
        assert!((got[0].1 - 40.0).abs() < 1e-6);
    }

    #[test]
    fn relayed_private_and_own_addresses_are_not_candidates() {
        let mut c = RelayClient::new(
            RelayClientSettings::default(),
            peer(),
            ReachabilityHandle::unknown(),
        );
        let relayed: Multiaddr = format!("/ip4/203.0.113.7/tcp/1/p2p/{}/p2p-circuit", peer())
            .parse()
            .unwrap();
        c.note_hop_relay(peer(), vec![relayed]);
        c.note_hop_relay(peer(), vec!["/ip4/10.0.0.5/tcp/1".parse().unwrap()]);
        c.note_hop_relay(peer(), vec!["/ip4/169.254.169.254/tcp/1".parse().unwrap()]);
        c.note_hop_relay(
            c.local_peer,
            vec!["/ip4/203.0.113.7/tcp/1".parse().unwrap()],
        );
        assert!(c.candidates.is_empty());
        c.note_hop_relay(peer(), vec!["/ip4/203.0.113.7/tcp/1".parse().unwrap()]);
        assert_eq!(c.candidates.len(), 1);
    }

    #[test]
    fn candidates_are_bounded() {
        let mut c = client(vec![], 1);
        for _ in 0..MAX_CANDIDATES + 10 {
            c.note_hop_relay(peer(), vec!["/ip4/127.0.0.1/tcp/1".parse().unwrap()]);
        }
        assert_eq!(c.candidates.len(), MAX_CANDIDATES);
    }

    #[test]
    fn only_accepted_reservations_are_published() {
        let handle = ReachabilityHandle::unknown();
        let mut c = RelayClient::new(RelayClientSettings::default(), peer(), handle.clone());
        let relay = peer();
        let listener = ListenerId::next();
        c.slots.insert(
            relay,
            Slot {
                listener_id: listener,
                relayed_addr: "/ip4/127.0.0.1/tcp/1/p2p-circuit".parse().unwrap(),
                neighbourhood: None,
                accepted: false,
                renewals: 0,
                started: Instant::now(),
            },
        );
        c.publish();
        assert!(handle.snapshot().relay_reservations.is_empty());
        assert!(handle.advertised_addrs().is_empty());

        c.on_accepted(relay, false);
        c.on_accepted(relay, true);
        let held = handle.snapshot().relay_reservations;
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].renewals, 1);
        assert_eq!(held[0].relay_peer_id, relay.to_string());

        c.on_listener_closed(listener, Instant::now());
        assert!(handle.snapshot().relay_reservations.is_empty());
        assert!(handle.advertised_addrs().is_empty());
    }

    #[test]
    fn relay_addr_must_name_the_relay_peer() {
        assert!(split_relay_addr(&"/ip4/127.0.0.1/tcp/1".parse().unwrap()).is_none());
        let p = peer();
        let (got, dial) = split_relay_addr(&with_peer("/ip4/127.0.0.1/tcp/1", p)).unwrap();
        assert_eq!(got, p);
        assert_eq!(dial.to_string(), "/ip4/127.0.0.1/tcp/1");
    }

    #[test]
    fn server_status_reports_limits_and_active_counts() {
        let stats = RelayServerStats::new();
        let view = RelayServerView {
            settings: RelayServerSettings::default(),
            stats: stats.clone(),
        };
        let idle = view.status();
        assert_eq!(idle.limits.max_circuits, 16);
        assert_eq!(idle.limits.max_circuit_bytes, 512 * 1024);
        assert_eq!(idle.limits.reservation_secs, 3600);
        assert_eq!(idle.usage.reservations_active, 0);

        for _ in 0..3 {
            stats.reservations_accepted.fetch_add(1, Ordering::Relaxed);
        }
        stats.reservations_closed.fetch_add(1, Ordering::Relaxed);
        stats.reservations_timed_out.fetch_add(1, Ordering::Relaxed);
        stats.circuits_accepted.fetch_add(2, Ordering::Relaxed);
        stats.circuits_closed.fetch_add(1, Ordering::Relaxed);
        stats.circuits_denied.fetch_add(4, Ordering::Relaxed);
        let busy = view.status();
        assert_eq!(busy.usage.reservations_active, 1);
        assert_eq!(busy.usage.circuits_active, 1);
        let json = serde_json::to_value(&busy).unwrap();
        assert_eq!(
            json["usage"]["circuits_denied"], 4,
            "totals sit beside the gauges"
        );
        assert_eq!(json["limits"]["max_reservations_per_peer"], 2);
    }

    #[test]
    fn defaults_are_bounded_and_the_server_is_off() {
        let settings = RelaySettings::default();
        assert!(settings.server.is_none());
        let server = RelayServerSettings::default();
        let cfg = server.libp2p_config();
        assert_eq!(cfg.max_reservations, 128);
        assert_eq!(cfg.max_circuits, 16);
        assert_eq!(cfg.max_circuit_bytes, 512 * 1024);
        assert_eq!(cfg.max_circuit_duration, Duration::from_secs(120));
        assert_eq!(cfg.reservation_duration, Duration::from_secs(3600));
    }

    #[test]
    fn env_defaults_overrides_and_bounds() {
        let _env = crate::test_env::guard();
        let names = [
            "AVALON_RELAY_SERVER_ENABLED",
            "AVALON_RELAY_MAX_RESERVATIONS",
            "AVALON_RELAY_MAX_CIRCUITS",
            "AVALON_RELAY_MAX_CIRCUIT_BYTES",
            "AVALON_RELAY_CLIENT_MAX_RESERVATIONS",
            "AVALON_RELAY_ADDRS",
            "AVALON_DCUTR_ENABLED",
        ];
        let clear = || {
            for n in names {
                unsafe { std::env::remove_var(n) };
            }
        };
        clear();
        let policy = OutboundPolicy::new(false);
        let defaults = RelaySettings::from_env(policy).unwrap();
        assert!(defaults.server.is_none());
        assert!(defaults.client.enabled);
        assert_eq!(defaults.client.max_reservations, 2);
        assert!(defaults.hole_punching, "hole punching is on by default");
        unsafe { std::env::set_var("AVALON_DCUTR_ENABLED", "false") };
        assert!(!RelaySettings::from_env(policy).unwrap().hole_punching);
        unsafe { std::env::remove_var("AVALON_DCUTR_ENABLED") };

        unsafe {
            std::env::set_var("AVALON_RELAY_SERVER_ENABLED", "true");
            std::env::set_var("AVALON_RELAY_MAX_RESERVATIONS", "7");
        }
        let on = RelaySettings::from_env(policy).unwrap().server.unwrap();
        assert_eq!(on.max_reservations, 7);
        assert_eq!(on.max_circuits, 16);

        for (name, bad) in [
            ("AVALON_RELAY_MAX_CIRCUITS", "0"),
            ("AVALON_RELAY_MAX_CIRCUIT_BYTES", "99999999999"),
            ("AVALON_RELAY_MAX_RESERVATIONS", "lots"),
        ] {
            unsafe { std::env::set_var(name, bad) };
            assert!(RelaySettings::from_env(policy).is_err(), "{name}={bad}");
            unsafe { std::env::remove_var(name) };
        }

        unsafe { std::env::set_var("AVALON_RELAY_ADDRS", "/ip4/127.0.0.1/tcp/1") };
        assert!(RelaySettings::from_env(policy).is_err());
        let p = peer();
        unsafe {
            std::env::set_var(
                "AVALON_RELAY_ADDRS",
                format!("/ip4/127.0.0.1/tcp/1/p2p/{p}"),
            )
        };
        let listed = RelaySettings::from_env(policy).unwrap();
        assert_eq!(listed.client.relay_addrs.len(), 1);
        clear();
    }
}
