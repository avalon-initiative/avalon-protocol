//! Keeps relaying working across short outages: the relay server stays on through a brief
//! AutoNAT `private` verdict, and a failed relayed dial is retried with bounded backoff.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use libp2p::swarm::{ConnectionId, DialError};
use libp2p::{Multiaddr, PeerId};

use crate::reachability::Reachability;

const DEFAULT_PRIVATE_GRACE_SECS: u64 = 90;
const MAX_PRIVATE_GRACE_SECS: u64 = 600;
const DEFAULT_DIAL_RETRIES: u64 = 5;
const MAX_DIAL_RETRIES: u64 = 20;
const DEFAULT_RETRY_BASE_SECS: u64 = 5;
const MAX_RETRY_BASE_SECS: u64 = 60;
/// Longest wait between two retries of one peer.
const RETRY_CAP: Duration = Duration::from_secs(120);
/// Peers whose retry state is held at once.
const MAX_TRACKED_PEERS: usize = 1024;

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

/// An integer knob within `0..=max`.
fn env_up_to(name: &str, default: u64, max: u64) -> Result<u64, String> {
    let Ok(raw) = std::env::var(name) else {
        return Ok(default);
    };
    match raw.trim().parse::<u64>() {
        Ok(v) if v <= max => Ok(v),
        _ => Err(format!(
            "{name} must be an integer from 0 to {max}, got {raw:?}"
        )),
    }
}

impl RelayResilience {
    /// `AVALON_RELAY_SERVER_PRIVATE_GRACE_SECS` (90, at most 600, 0 stops serving at once),
    /// `AVALON_RELAY_DIAL_RETRIES` (5, at most 20, 0 disables) and
    /// `AVALON_RELAY_DIAL_RETRY_BASE_SECS` (5, 1 to 60).
    pub fn from_env() -> Result<Self, String> {
        let base = crate::relay::bounded_env(
            "AVALON_RELAY_DIAL_RETRY_BASE_SECS",
            DEFAULT_RETRY_BASE_SECS,
            MAX_RETRY_BASE_SECS,
        )?;
        Ok(Self {
            private_grace: Duration::from_secs(env_up_to(
                "AVALON_RELAY_SERVER_PRIVATE_GRACE_SECS",
                DEFAULT_PRIVATE_GRACE_SECS,
                MAX_PRIVATE_GRACE_SECS,
            )?),
            dial_retries: env_up_to(
                "AVALON_RELAY_DIAL_RETRIES",
                DEFAULT_DIAL_RETRIES,
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
/// its limits. Policy refusals and identity mismatches are not retried.
pub fn is_retryable_relayed_failure(error: &DialError) -> bool {
    match error {
        DialError::Transport(failures) => failures.iter().any(|(addr, _)| {
            addr.iter()
                .any(|p| matches!(p, libp2p::multiaddr::Protocol::P2pCircuit))
        }),
        _ => false,
    }
}

/// Extra outbound connections to peers that already have one, by the address they were dialed
/// at. An AutoNAT dial-back opens one and nothing closes it before the idle timeout, so repeated
/// probes pile connections up against the per-peer limit, which then denies further dial-backs
/// and the prober is told `private`. The dial-back's own connection is closed once it is answered.
#[derive(Default)]
pub struct DialbackConns {
    extra: HashMap<ConnectionId, (PeerId, Multiaddr)>,
}

impl DialbackConns {
    /// An outbound connection to `peer` at `address` opened while `peer` had others.
    pub fn opened_extra(&mut self, id: ConnectionId, peer: PeerId, address: Multiaddr) {
        self.extra.insert(id, (peer, address));
    }

    pub fn closed(&mut self, id: &ConnectionId) {
        self.extra.remove(id);
    }

    /// The connection a dial-back to `peer` at `address` succeeded over, to be closed now.
    pub fn answered(&mut self, peer: &PeerId, address: &Multiaddr) -> Option<ConnectionId> {
        let id = *self
            .extra
            .iter()
            .find(|(_, (p, a))| p == peer && a == address)?
            .0;
        self.extra.remove(&id);
        Some(id)
    }
}

struct Backoff {
    failures: u32,
    next_at: Instant,
}

/// Per-peer exponential backoff for relayed dials, bounded in attempts and in tracked peers.
pub struct DialRetry {
    resilience: RelayResilience,
    peers: HashMap<PeerId, Backoff>,
}

impl DialRetry {
    pub fn new(resilience: RelayResilience) -> Self {
        Self {
            resilience,
            peers: HashMap::new(),
        }
    }

    /// Records a failed relayed dial. `Some(wait)` when a retry is allowed after `wait`; `None`
    /// once the retries are spent. A failure while still backing off (another dial of the same
    /// peer) does not use up a retry.
    pub fn on_failure(&mut self, peer: PeerId, now: Instant) -> Option<Duration> {
        if !self.peers.contains_key(&peer) && self.peers.len() >= MAX_TRACKED_PEERS {
            if let Some(oldest) = self
                .peers
                .iter()
                .min_by_key(|(_, b)| b.next_at)
                .map(|(p, _)| *p)
            {
                self.peers.remove(&oldest);
            }
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
        DialRetry::new(RelayResilience {
            dial_retries: retries,
            dial_retry_base: Duration::from_secs(5),
            ..RelayResilience::default()
        })
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

    #[test]
    fn tracked_peers_are_bounded() {
        let (mut retry, t0) = (retry(3), Instant::now());
        for i in 0..MAX_TRACKED_PEERS as u64 + 10 {
            retry.on_failure(PeerId::random(), t0 + Duration::from_secs(i));
        }
        assert_eq!(retry.peers.len(), MAX_TRACKED_PEERS);
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
        assert!(!is_retryable_relayed_failure(&DialError::Aborted));
    }

    #[test]
    fn an_answered_dialback_yields_its_own_connection_once() {
        let mut conns = DialbackConns::default();
        let (peer, other) = (PeerId::random(), PeerId::random());
        let addr: Multiaddr = "/ip4/10.0.0.1/tcp/4001".parse().unwrap();
        let (first, second) = (
            ConnectionId::new_unchecked(1),
            ConnectionId::new_unchecked(2),
        );
        conns.opened_extra(first, peer, addr.clone());
        conns.opened_extra(second, other, addr.clone());
        assert_eq!(
            conns.answered(&other, &"/ip4/10.0.0.2/tcp/1".parse().unwrap()),
            None
        );
        assert_eq!(conns.answered(&peer, &addr), Some(first));
        assert_eq!(conns.answered(&peer, &addr), None);
        conns.closed(&second);
        assert_eq!(conns.answered(&other, &addr), None);
    }

    #[test]
    fn settings_default_within_their_ceilings() {
        let d = RelayResilience::default();
        assert!(d.private_grace.as_secs() <= MAX_PRIVATE_GRACE_SECS);
        assert!(u64::from(d.dial_retries) <= MAX_DIAL_RETRIES);
        assert!(d.dial_retry_base.as_secs() <= MAX_RETRY_BASE_SECS);
    }
}
