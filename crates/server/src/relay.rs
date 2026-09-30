//! Circuit relay v2 settings and state: the opt-in, bounded relay server role and the client
//! role that keeps reservations for a node AutoNAT found `private`.
//!
//! The libp2p behaviours live in the DHT swarm (`crate::dht`). This module holds what can be
//! decided without a swarm: env parsing, limit mapping, relay candidate choice and the
//! reservation bookkeeping published into [`crate::reachability::ReachabilityHandle`].

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use libp2p::core::transport::ListenerId;
use libp2p::multiaddr::Protocol;
use libp2p::swarm::NetworkBehaviour;
use libp2p::{relay, Multiaddr, PeerId, Swarm};

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

/// Hard ceilings: a relay knob can be raised, never made effectively unbounded.
const MAX_SERVER_RESERVATIONS: u64 = 4096;
const MAX_SERVER_CIRCUITS: u64 = 1024;
const MAX_SERVER_RESERVATION_SECS: u64 = 24 * 60 * 60;
const MAX_SERVER_CIRCUIT_SECS: u64 = 60 * 60;
const MAX_SERVER_CIRCUIT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CLIENT_RESERVATIONS: u64 = 8;

/// Most relay candidates a client remembers.
const MAX_CANDIDATES: usize = 64;

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
    /// How long a relay that failed is skipped.
    pub retry_backoff: Duration,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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

#[derive(Debug, Clone)]
struct Candidate {
    /// Direct addresses of the relay, sorted, without the `/p2p/<id>` suffix.
    addrs: Vec<Multiaddr>,
    /// Operator-listed relays (by list position) come before discovered ones.
    rank: (u8, usize),
    failures: usize,
    retry_at: Option<Instant>,
}

#[derive(Debug)]
struct Slot {
    listener_id: ListenerId,
    relayed_addr: Multiaddr,
    accepted: bool,
    renewals: u64,
    started: Instant,
}

/// Chooses relays and tracks reservations for a `private` node. Selection is deterministic:
/// operator-listed relays in list order, then discovered ones by peer id, skipping any that
/// failed recently.
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
        self.candidates.insert(
            peer,
            Candidate {
                addrs,
                rank,
                failures: 0,
                retry_at: None,
            },
        );
    }

    /// A connected peer advertised the relay hop protocol; `addrs` are the addresses it reported.
    pub fn note_hop_relay(&mut self, peer: PeerId, addrs: Vec<Multiaddr>) {
        self.add_candidate(peer, addrs, (1, 0));
    }

    fn next_candidate(&self, now: Instant) -> Option<PeerId> {
        self.candidates
            .iter()
            .filter(|(peer, c)| {
                !self.slots.contains_key(peer) && c.retry_at.is_none_or(|t| now >= t)
            })
            .min_by_key(|(peer, c)| (c.rank, **peer))
            .map(|(peer, _)| *peer)
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
            self.mark_failed(peer, now);
        }

        while self.slots.len() < self.settings.max_reservations {
            let Some(peer) = self.next_candidate(now) else {
                break;
            };
            let candidate = &self.candidates[&peer];
            let dial = candidate.addrs[candidate.failures % candidate.addrs.len()].clone();
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
                            accepted: false,
                            renewals: 0,
                            started: now,
                        },
                    );
                }
                Err(e) => {
                    tracing::warn!(%peer, "avalon-relay: cannot listen through relay: {e}");
                    self.mark_failed(peer, now);
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

    fn mark_failed(&mut self, peer: PeerId, now: Instant) {
        if let Some(c) = self.candidates.get_mut(&peer) {
            c.failures += 1;
            c.retry_at = Some(now + self.settings.retry_backoff);
        }
    }

    /// The relay accepted or renewed a reservation.
    pub fn on_accepted(&mut self, relay_peer: PeerId, renewal: bool) {
        let Some(slot) = self.slots.get_mut(&relay_peer) else {
            return;
        };
        slot.accepted = true;
        if renewal {
            slot.renewals += 1;
        } else if let Some(c) = self.candidates.get_mut(&relay_peer) {
            c.failures = 0;
            c.retry_at = None;
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
        self.slots.remove(&peer);
        tracing::warn!(relay = %peer, "avalon-relay: reservation lost");
        self.mark_failed(peer, now);
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
        c.mark_failed(a, now);
        assert_eq!(c.next_candidate(now), Some(b));
        let later = now + c.settings.retry_backoff;
        assert_eq!(c.next_candidate(later), Some(a));
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
