//! Keeps relaying working across short outages: the relay server stays on through a brief
//! AutoNAT `private` verdict, and a failed relayed dial is retried with bounded backoff.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use libp2p::swarm::{ConnectionId, DialError};
use libp2p::{Multiaddr, PeerId};

use crate::reachability::Reachability;
use crate::relay::{bounded_env, bounded_env_from};

const DEFAULT_PRIVATE_GRACE_SECS: u64 = 90;
const MAX_PRIVATE_GRACE_SECS: u64 = 600;
const DEFAULT_DIAL_RETRIES: u64 = 5;
const MAX_DIAL_RETRIES: u64 = 20;
const DEFAULT_RETRY_BASE_SECS: u64 = 5;
const MAX_RETRY_BASE_SECS: u64 = 60;
/// Longest wait between two retries of one peer.
const RETRY_CAP: Duration = Duration::from_secs(120);

/// Resolved once at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayResilience {
    /// How long a serving relay keeps serving after AutoNAT says `private`; zero stops at once.
    pub private_grace: Duration,
    /// Retries of a failed relayed dial per peer; zero disables retrying.
    pub dial_retries: u32,
    /// Wait before the first retry; each further one doubles it up to [`RETRY_CAP`].
    pub dial_retry_base: Duration,
}

impl Default for RelayResilience {
    fn default() -> Self {
        Self {
            private_grace: Duration::from_secs(DEFAULT_PRIVATE_GRACE_SECS),
            dial_retries: DEFAULT_DIAL_RETRIES as u32,
            dial_retry_base: Duration::from_secs(DEFAULT_RETRY_BASE_SECS),
        }
    }
}

impl RelayResilience {
    /// `AVALON_RELAY_SERVER_PRIVATE_GRACE_SECS` (90, at most 600, 0 stops serving at once),
    /// `AVALON_RELAY_DIAL_RETRIES` (5, at most 20, 0 disables) and
    /// `AVALON_RELAY_DIAL_RETRY_BASE_SECS` (5, 1 to 60).
    pub fn from_env() -> Result<Self, String> {
        let base = bounded_env(
            "AVALON_RELAY_DIAL_RETRY_BASE_SECS",
            DEFAULT_RETRY_BASE_SECS,
            MAX_RETRY_BASE_SECS,
        )?;
        Ok(Self {
            private_grace: Duration::from_secs(bounded_env_from(
                "AVALON_RELAY_SERVER_PRIVATE_GRACE_SECS",
                DEFAULT_PRIVATE_GRACE_SECS,
                0,
                MAX_PRIVATE_GRACE_SECS,
            )?),
            dial_retries: bounded_env_from(
                "AVALON_RELAY_DIAL_RETRIES",
                DEFAULT_DIAL_RETRIES,
                0,
                MAX_DIAL_RETRIES,
            )? as u32,
            dial_retry_base: Duration::from_secs(base),
        })
    }
}

/// Decides whether the relay server is on. A node that is serving keeps serving through a
/// `private` verdict for the grace period, so one failed probe does not take it out of service;
/// `public` resumes at once, and a node that never served is not switched on by the grace.
#[derive(Debug)]
pub struct RelayGate {
    grace: Duration,
    serving: bool,
    private_since: Option<Instant>,
}

impl RelayGate {
    pub fn new(grace: Duration) -> Self {
        Self {
            grace,
            serving: false,
            private_since: None,
        }
    }

    /// Whether the server should be on now. Call on every verdict change and on a timer, so the
    /// end of the grace period takes effect without another verdict.
    pub fn should_serve(
        &mut self,
        reachability: Reachability,
        has_external_addr: bool,
        now: Instant,
    ) -> bool {
        self.serving = match reachability {
            Reachability::Public => true,
            Reachability::Unknown => has_external_addr,
            Reachability::Private => self.serving && self.within_grace(now),
        };
        if reachability != Reachability::Private {
            self.private_since = None;
        }
        self.serving
    }

    fn within_grace(&mut self, now: Instant) -> bool {
        let since = *self.private_since.get_or_insert(now);
        now.saturating_duration_since(since) < self.grace
    }
}

/// Whether a failed dial went through a relay and may succeed once the relay is back: the relay
/// was briefly unavailable (for example it did not offer the hop protocol), timed out or was at
/// its limits. Policy refusals and identity mismatches are not retried. Any failure of a
/// circuit-address dial counts against the retry budget, whatever its cause.
pub fn is_retryable_relayed_failure(error: &DialError) -> bool {
    match error {
        DialError::Transport(failures) => failures.iter().any(|(addr, _)| {
            addr.iter()
                .any(|p| matches!(p, libp2p::multiaddr::Protocol::P2pCircuit))
        }),
        _ => false,
    }
}

/// The connection each pending AutoNAT dial-back opened. It stays open until the idle timeout, so
/// repeated probes pile connections up against the per-peer limit, which then denies further
/// dial-backs and the prober is told `private`; it is closed once the probe is answered.
#[derive(Default)]
pub struct DialbackConns {
    pending: HashMap<PeerId, Pending>,
}

struct Pending {
    addresses: Vec<Multiaddr>,
    conn: Option<ConnectionId>,
}

/// Dial-backs awaited at once.
const MAX_PENDING_DIALBACKS: usize = 256;

impl DialbackConns {
    /// A dial-back request from `peer`, to be attempted at `addresses`.
    pub fn requested(&mut self, peer: PeerId, addresses: Vec<Multiaddr>) {
        if self.pending.len() < MAX_PENDING_DIALBACKS || self.pending.contains_key(&peer) {
            self.pending.insert(
                peer,
                Pending {
                    addresses,
                    conn: None,
                },
            );
        }
    }

    /// An outbound connection to `peer` opened; only the first one at a requested address, with
    /// the dialer role (not a hole punch), is taken for the dial-back's.
    pub fn opened(&mut self, id: ConnectionId, peer: &PeerId, address: &Multiaddr, dialer: bool) {
        if let Some(p) = self.pending.get_mut(peer) {
            if dialer && p.conn.is_none() && p.addresses.contains(address) {
                p.conn = Some(id);
            }
        }
    }

    pub fn closed(&mut self, id: &ConnectionId) {
        for p in self.pending.values_mut() {
            if p.conn.as_ref() == Some(id) {
                p.conn = None;
            }
        }
    }

    /// The probe from `peer` ended without a connection to close.
    pub fn failed(&mut self, peer: &PeerId) {
        self.pending.remove(peer);
    }

    /// The probe from `peer` succeeded: the connection to close, if one was seen.
    pub fn answered(&mut self, peer: &PeerId) -> Option<ConnectionId> {
        self.pending.remove(peer)?.conn
    }
}

struct Backoff {
    failures: u32,
    next_at: Instant,
}

/// Per-peer exponential backoff for relayed dials, bounded in attempts and in tracked peers.
pub struct DialRetry {
    resilience: RelayResilience,
    /// Peers tracked at once.
    cap: usize,
    peers: HashMap<PeerId, Backoff>,
}

impl DialRetry {
    pub fn new(resilience: RelayResilience, cap: usize) -> Self {
        Self {
            resilience,
            cap: cap.max(1),
            peers: HashMap::new(),
        }
    }

    /// Records a failed relayed dial. `Some(wait)` when a retry is allowed after `wait`; `None`
    /// once the retries are spent. A failure while still backing off (another dial of the same
    /// peer) does not use up a retry.
    pub fn on_failure(&mut self, peer: PeerId, now: Instant) -> Option<Duration> {
        if !self.peers.contains_key(&peer) && self.peers.len() >= self.cap {
            // Only a spent entry may go: evicting a waiting one would reset its budget.
            let spent = self
                .peers
                .iter()
                .find(|(_, b)| b.failures >= self.resilience.dial_retries)
                .map(|(p, _)| *p);
            self.peers.remove(&spent?);
        }
        let entry = self.peers.entry(peer).or_insert(Backoff {
            failures: 0,
            next_at: now,
        });
        if entry.failures >= self.resilience.dial_retries {
            return None;
        }
        if entry.failures > 0 && now < entry.next_at {
            return Some(entry.next_at - now);
        }
        let wait = self
            .resilience
            .dial_retry_base
            .saturating_mul(1u32 << entry.failures.min(16))
            .min(RETRY_CAP);
        entry.failures += 1;
        entry.next_at = now + wait;
        Some(wait)
    }

    /// Whether a dial to `peer` may start: it is not backing off.
    pub fn due(&self, peer: &PeerId, now: Instant) -> bool {
        self.peers.get(peer).is_none_or(|b| now >= b.next_at)
    }

    /// A connection to `peer` opened; its failures no longer count.
    pub fn on_connected(&mut self, peer: &PeerId) {
        self.peers.remove(peer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRACE: Duration = Duration::from_secs(90);

    fn gate_serving() -> (RelayGate, Instant) {
        let t0 = Instant::now();
        let mut gate = RelayGate::new(GRACE);
        assert!(gate.should_serve(Reachability::Public, true, t0));
        (gate, t0)
    }

    #[test]
    fn a_serving_relay_survives_a_brief_private_verdict() {
        let (mut gate, t0) = gate_serving();
        assert!(gate.should_serve(Reachability::Private, true, t0 + Duration::from_secs(1)));
        assert!(gate.should_serve(Reachability::Private, true, t0 + Duration::from_secs(60)));
        assert!(gate.should_serve(Reachability::Public, true, t0 + Duration::from_secs(61)));
    }

    #[test]
    fn a_relay_that_stays_private_stops_after_the_grace() {
        let (mut gate, t0) = gate_serving();
        assert!(gate.should_serve(Reachability::Private, true, t0));
        assert!(gate.should_serve(
            Reachability::Private,
            true,
            t0 + GRACE - Duration::from_secs(1)
        ));
        assert!(!gate.should_serve(Reachability::Private, true, t0 + GRACE));
        // Stays off, and only a public verdict switches it on again.
        assert!(!gate.should_serve(Reachability::Private, true, t0 + GRACE * 2));
        assert!(gate.should_serve(Reachability::Public, true, t0 + GRACE * 2));
    }

    #[test]
    fn public_in_between_restarts_the_grace() {
        let (mut gate, t0) = gate_serving();
        assert!(gate.should_serve(Reachability::Private, true, t0));
        assert!(gate.should_serve(Reachability::Public, true, t0 + Duration::from_secs(80)));
        let t1 = t0 + Duration::from_secs(100);
        assert!(gate.should_serve(Reachability::Private, true, t1));
        assert!(gate.should_serve(
            Reachability::Private,
            true,
            t1 + GRACE - Duration::from_secs(1)
        ));
    }

    #[test]
    fn a_node_that_never_served_is_not_switched_on_by_the_grace() {
        let t0 = Instant::now();
        let mut gate = RelayGate::new(GRACE);
        assert!(!gate.should_serve(Reachability::Unknown, false, t0));
        assert!(!gate.should_serve(Reachability::Private, true, t0));
    }

    #[test]
    fn unknown_serves_only_with_a_stated_external_address() {
        let t0 = Instant::now();
        let mut gate = RelayGate::new(GRACE);
        assert!(gate.should_serve(Reachability::Unknown, true, t0));
        assert!(!RelayGate::new(GRACE).should_serve(Reachability::Unknown, false, t0));
    }

    #[test]
    fn a_zero_grace_stops_at_the_first_private_verdict() {
        let t0 = Instant::now();
        let mut gate = RelayGate::new(Duration::ZERO);
        assert!(gate.should_serve(Reachability::Public, true, t0));
        assert!(!gate.should_serve(Reachability::Private, true, t0));
    }

    fn retry(retries: u32) -> DialRetry {
        DialRetry::new(
            RelayResilience {
                dial_retries: retries,
                dial_retry_base: Duration::from_secs(5),
                ..RelayResilience::default()
            },
            1000,
        )
    }

    #[test]
    fn retries_back_off_exponentially_to_the_cap_and_then_stop() {
        let (mut retry, peer, mut now) = (retry(7), PeerId::random(), Instant::now());
        let mut waits = Vec::new();
        for _ in 0..7 {
            let wait = retry.on_failure(peer, now).unwrap();
            waits.push(wait.as_secs());
            now += wait;
        }
        assert_eq!(waits, [5, 10, 20, 40, 80, 120, 120]);
        assert_eq!(retry.on_failure(peer, now), None);
    }

    #[test]
    fn a_peer_backing_off_is_not_due_until_its_wait_passes() {
        let (mut retry, peer, t0) = (retry(3), PeerId::random(), Instant::now());
        assert!(retry.due(&peer, t0));
        retry.on_failure(peer, t0);
        assert!(!retry.due(&peer, t0 + Duration::from_secs(4)));
        assert!(retry.due(&peer, t0 + Duration::from_secs(5)));
    }

    #[test]
    fn a_failure_while_backing_off_uses_no_retry() {
        let (mut retry, peer, t0) = (retry(2), PeerId::random(), Instant::now());
        assert_eq!(retry.on_failure(peer, t0), Some(Duration::from_secs(5)));
        assert_eq!(
            retry.on_failure(peer, t0 + Duration::from_secs(1)),
            Some(Duration::from_secs(4))
        );
        let t1 = t0 + Duration::from_secs(5);
        assert_eq!(retry.on_failure(peer, t1), Some(Duration::from_secs(10)));
        assert_eq!(retry.on_failure(peer, t1 + Duration::from_secs(10)), None);
    }

    #[test]
    fn a_connection_clears_the_failures() {
        let (mut retry, peer, t0) = (retry(1), PeerId::random(), Instant::now());
        retry.on_failure(peer, t0);
        assert_eq!(retry.on_failure(peer, t0), None);
        retry.on_connected(&peer);
        assert!(retry.on_failure(peer, t0).is_some());
    }

    #[test]
    fn zero_retries_never_retry() {
        let mut retry = retry(0);
        assert_eq!(retry.on_failure(PeerId::random(), Instant::now()), None);
    }

    fn capped(retries: u32, cap: usize) -> DialRetry {
        DialRetry::new(
            RelayResilience {
                dial_retries: retries,
                ..RelayResilience::default()
            },
            cap,
        )
    }

    #[test]
    fn a_waiting_peer_keeps_its_budget_when_the_set_is_full() {
        let (mut retry, t0) = (capped(3, 2), Instant::now());
        let (a, b, c) = (PeerId::random(), PeerId::random(), PeerId::random());
        retry.on_failure(a, t0);
        retry.on_failure(b, t0);
        // Neither is spent, so a third peer is not tracked and nobody loses their count.
        assert_eq!(retry.on_failure(c, t0), None);
        assert_eq!(retry.peers.len(), 2);
        assert_eq!(retry.peers[&a].failures, 1);
    }

    #[test]
    fn a_spent_entry_makes_room_first() {
        let (mut retry, t0) = (capped(1, 2), Instant::now());
        let (a, b, c) = (PeerId::random(), PeerId::random(), PeerId::random());
        retry.on_failure(a, t0);
        retry.on_failure(b, t0);
        assert!(retry.peers.len() == 2);
        // Both are spent (one retry each); the new peer replaces one of them.
        assert!(retry.on_failure(c, t0).is_some());
        assert_eq!(retry.peers.len(), 2);
        assert!(retry.peers.contains_key(&c));
    }

    #[test]
    fn the_tracked_set_never_exceeds_its_cap() {
        let (mut retry, t0) = (capped(1, 8), Instant::now());
        for _ in 0..50 {
            retry.on_failure(PeerId::random(), t0);
        }
        assert!(retry.peers.len() <= 8);
    }

    #[test]
    fn only_failures_through_a_relay_are_retryable() {
        use libp2p::core::transport::TransportError;
        let other = |addr: &str| {
            DialError::Transport(vec![(
                addr.parse().unwrap(),
                TransportError::Other(std::io::Error::other("remote does not support hop")),
            )])
        };
        let circuit = "/ip4/10.0.0.1/tcp/4001/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN/p2p-circuit";
        assert!(is_retryable_relayed_failure(&other(circuit)));
        assert!(!is_retryable_relayed_failure(&other(
            "/ip4/10.0.0.1/tcp/4001"
        )));
        assert!(!is_retryable_relayed_failure(&DialError::NoAddresses));
        assert!(!is_retryable_relayed_failure(&DialError::WrongPeerId {
            obtained: PeerId::random(),
            address: circuit.parse().unwrap(),
        }));
        assert!(!is_retryable_relayed_failure(&DialError::LocalPeerId {
            address: circuit.parse().unwrap(),
        }));
        assert!(!is_retryable_relayed_failure(&DialError::Denied {
            cause: libp2p::swarm::ConnectionDenied::new(std::io::Error::other("limit")),
        }));
        assert!(!is_retryable_relayed_failure(&DialError::Aborted));
    }

    fn conn(n: usize) -> ConnectionId {
        ConnectionId::new_unchecked(n)
    }

    fn addr(s: &str) -> Multiaddr {
        s.parse().unwrap()
    }

    #[test]
    fn only_the_first_dialback_connection_is_closed_not_an_older_one() {
        let mut conns = DialbackConns::default();
        let (peer, a) = (PeerId::random(), addr("/ip4/10.0.0.1/tcp/4001"));
        // An older extra connection to the same peer and address was never recorded.
        conns.opened(conn(1), &peer, &a, true);
        conns.requested(peer, vec![a.clone()]);
        conns.opened(conn(2), &peer, &a, true);
        conns.opened(conn(3), &peer, &a, true);
        assert_eq!(conns.answered(&peer), Some(conn(2)));
        assert_eq!(conns.answered(&peer), None);
    }

    #[test]
    fn a_connection_without_a_pending_request_is_never_recorded() {
        let mut conns = DialbackConns::default();
        let (peer, a) = (PeerId::random(), addr("/ip4/10.0.0.1/tcp/4001"));
        conns.opened(conn(1), &peer, &a, true);
        assert_eq!(conns.answered(&peer), None);
        conns.requested(peer, vec![a.clone()]);
        assert_eq!(conns.answered(&peer), None);
    }

    #[test]
    fn a_listener_role_dial_is_never_recorded() {
        let mut conns = DialbackConns::default();
        let (peer, a) = (PeerId::random(), addr("/ip4/10.0.0.1/tcp/4001"));
        conns.requested(peer, vec![a.clone()]);
        conns.opened(conn(1), &peer, &a, false);
        assert_eq!(conns.answered(&peer), None);
    }

    #[test]
    fn an_address_outside_the_request_is_never_recorded() {
        let mut conns = DialbackConns::default();
        let peer = PeerId::random();
        conns.requested(peer, vec![addr("/ip4/10.0.0.1/tcp/4001")]);
        conns.opened(conn(1), &peer, &addr("/ip4/10.0.0.2/tcp/4001"), true);
        assert_eq!(conns.answered(&peer), None);
    }

    #[test]
    fn a_failed_probe_or_closed_connection_clears_the_record() {
        let mut conns = DialbackConns::default();
        let (peer, a) = (PeerId::random(), addr("/ip4/10.0.0.1/tcp/4001"));
        conns.requested(peer, vec![a.clone()]);
        conns.opened(conn(1), &peer, &a, true);
        conns.closed(&conn(1));
        assert_eq!(conns.answered(&peer), None);
        conns.requested(peer, vec![a.clone()]);
        conns.opened(conn(2), &peer, &a, true);
        conns.failed(&peer);
        assert_eq!(conns.answered(&peer), None);
    }

    #[test]
    fn pending_dialbacks_are_bounded() {
        let mut conns = DialbackConns::default();
        for _ in 0..MAX_PENDING_DIALBACKS + 10 {
            conns.requested(PeerId::random(), Vec::new());
        }
        assert_eq!(conns.pending.len(), MAX_PENDING_DIALBACKS);
    }

    #[test]
    fn env_settings_default_range_and_ceiling() {
        const VARS: [&str; 3] = [
            "AVALON_RELAY_SERVER_PRIVATE_GRACE_SECS",
            "AVALON_RELAY_DIAL_RETRIES",
            "AVALON_RELAY_DIAL_RETRY_BASE_SECS",
        ];
        let set = |name: &str, v: Option<&str>| unsafe {
            match v {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        };
        for v in VARS {
            set(v, None);
        }
        assert_eq!(
            RelayResilience::from_env().unwrap(),
            RelayResilience::default()
        );

        set(VARS[0], Some("0"));
        set(VARS[1], Some("0"));
        let off = RelayResilience::from_env().unwrap();
        assert_eq!((off.private_grace, off.dial_retries), (Duration::ZERO, 0));

        set(VARS[0], Some("600"));
        set(VARS[1], Some("20"));
        set(VARS[2], Some("60"));
        let max = RelayResilience::from_env().unwrap();
        assert_eq!(max.private_grace, Duration::from_secs(600));
        assert_eq!(
            (max.dial_retries, max.dial_retry_base),
            (20, Duration::from_secs(60))
        );

        for (name, bad) in [
            (VARS[0], "601"),
            (VARS[0], "-1"),
            (VARS[1], "21"),
            (VARS[2], "0"),
            (VARS[2], "61"),
            (VARS[2], "x"),
        ] {
            for v in VARS {
                set(v, None);
            }
            set(name, Some(bad));
            assert!(RelayResilience::from_env().is_err(), "{name}={bad}");
        }
        for v in VARS {
            set(v, None);
        }
    }

    #[test]
    fn settings_default_within_their_ceilings() {
        let d = RelayResilience::default();
        assert!(d.private_grace.as_secs() <= MAX_PRIVATE_GRACE_SECS);
        assert!(u64::from(d.dial_retries) <= MAX_DIAL_RETRIES);
        assert!(d.dial_retry_base.as_secs() <= MAX_RETRY_BASE_SECS);
    }
}
