//! Recent transport outcomes per peer, so a transport that keeps failing is tried second.
//! A record only ever demotes: it orders the transports a peer's hint already allows and never
//! adds one, relaxes verification, or vouches for anything.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use libp2p::PeerId;

use crate::outbound_policy::CheckedTarget;

/// Peers tracked at once; the least recently touched is dropped past this.
pub const MAX_TRACKED_PEERS: usize = 4096;
/// Demotion after the first failure; doubles per consecutive failure up to [`BACKOFF_MAX`].
pub const BACKOFF_BASE: Duration = Duration::from_secs(10);
pub const BACKOFF_MAX: Duration = Duration::from_secs(300);

/// How long a policy check of a peer's http URL, passed or refused, is reused.
pub const VET_TTL: Duration = Duration::from_secs(30);
/// Cached checks kept at once.
pub const MAX_VET_ENTRIES: usize = 1024;

/// A peer-table URL that passed the outbound policy, with the pinned client to dial it by.
#[derive(Clone)]
pub struct Vetted {
    pub target: CheckedTarget,
    pub client: reqwest::Client,
}

type VetKey = (PeerId, String, bool);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Transport {
    Http,
    Stream,
}

impl Transport {
    pub fn other(self) -> Self {
        match self {
            Self::Http => Self::Stream,
            Self::Stream => Self::Http,
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// What this node has seen of one transport to one peer.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub last_success: Option<Instant>,
    pub last_failure: Option<Instant>,
    pub consecutive_failures: u32,
    pub rtt: Option<Duration>,
    backoff_until: Option<Instant>,
}

#[derive(Default)]
struct Record {
    outcomes: [Outcome; 2],
    touched: u64,
}

#[derive(Default)]
struct Inner {
    peers: HashMap<PeerId, Record>,
    tick: u64,
    /// Policy checks by peer, base URL and whether private ranges were allowed; `None` is a refusal.
    vetted: HashMap<VetKey, (Instant, Option<Vetted>)>,
}

/// Shared by every client of one peer table; cheap to clone.
#[derive(Clone, Default)]
pub struct TransportStats {
    inner: Arc<Mutex<Inner>>,
}

fn backoff_for(failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(16);
    BACKOFF_BASE.saturating_mul(1u32 << shift).min(BACKOFF_MAX)
}

impl TransportStats {
    pub fn record_success(&self, peer: PeerId, transport: Transport, rtt: Duration) {
        self.record_success_at(peer, transport, rtt, Instant::now());
    }

    pub fn record_failure(&self, peer: PeerId, transport: Transport) {
        self.record_failure_at(peer, transport, Instant::now());
    }

    /// Whether `transport` to `peer` is inside its backoff window.
    pub fn is_demoted(&self, peer: &PeerId, transport: Transport) -> bool {
        self.is_demoted_at(peer, transport, Instant::now())
    }

    /// The transport to try first: `preferred` unless it is demoted and `other_available`
    /// names a usable alternative that is not. Both demoted keeps the hint.
    pub fn choose(&self, peer: &PeerId, preferred: Transport, other_available: bool) -> Transport {
        let now = Instant::now();
        if other_available
            && self.is_demoted_at(peer, preferred, now)
            && !self.is_demoted_at(peer, preferred.other(), now)
        {
            preferred.other()
        } else {
            preferred
        }
    }

    /// A copy of what is known about `transport` to `peer`.
    pub fn outcome(&self, peer: &PeerId, transport: Transport) -> Option<Outcome> {
        let inner = self.lock();
        inner
            .peers
            .get(peer)
            .map(|r| r.outcomes[transport.index()].clone())
    }

    /// A still-fresh policy check for `base_url`: `Some(None)` is a remembered refusal.
    pub(crate) fn cached_vet(
        &self,
        peer: PeerId,
        base_url: &str,
        allow_private: bool,
        now: Instant,
    ) -> Option<Option<Vetted>> {
        let inner = self.lock();
        let (at, v) = inner
            .vetted
            .get(&(peer, base_url.to_string(), allow_private))?;
        (now.duration_since(*at) < VET_TTL).then(|| v.clone())
    }

    pub(crate) fn store_vet(
        &self,
        peer: PeerId,
        base_url: &str,
        allow_private: bool,
        vetted: Option<Vetted>,
        now: Instant,
    ) {
        let mut inner = self.lock();
        inner
            .vetted
            .retain(|_, (at, _)| now.duration_since(*at) < VET_TTL);
        if inner.vetted.len() >= MAX_VET_ENTRIES {
            let oldest = inner
                .vetted
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                inner.vetted.remove(&k);
            }
        }
        inner
            .vetted
            .insert((peer, base_url.to_string(), allow_private), (now, vetted));
    }

    pub fn tracked(&self) -> usize {
        self.lock().peers.len()
    }

    pub(crate) fn record_success_at(
        &self,
        peer: PeerId,
        transport: Transport,
        rtt: Duration,
        now: Instant,
    ) {
        self.with_record(
            peer,
            |o| {
                o.last_success = Some(now);
                o.consecutive_failures = 0;
                o.backoff_until = None;
                o.rtt = Some(match o.rtt {
                    // EWMA with weight 1/8 on the new sample.
                    Some(old) => old.mul_f64(0.875) + rtt.mul_f64(0.125),
                    None => rtt,
                });
            },
            transport,
        );
    }

    pub(crate) fn record_failure_at(&self, peer: PeerId, transport: Transport, now: Instant) {
        self.with_record(
            peer,
            |o| {
                o.last_failure = Some(now);
                o.consecutive_failures = o.consecutive_failures.saturating_add(1);
                o.backoff_until = Some(now + backoff_for(o.consecutive_failures));
            },
            transport,
        );
    }

    pub(crate) fn is_demoted_at(&self, peer: &PeerId, transport: Transport, now: Instant) -> bool {
        self.lock()
            .peers
            .get(peer)
            .and_then(|r| r.outcomes[transport.index()].backoff_until)
            .is_some_and(|until| now < until)
    }

    fn with_record(&self, peer: PeerId, f: impl FnOnce(&mut Outcome), transport: Transport) {
        let mut inner = self.lock();
        inner.tick += 1;
        let tick = inner.tick;
        if !inner.peers.contains_key(&peer) && inner.peers.len() >= MAX_TRACKED_PEERS {
            let oldest = inner
                .peers
                .iter()
                .min_by_key(|(_, r)| r.touched)
                .map(|(p, _)| *p);
            if let Some(oldest) = oldest {
                inner.peers.remove(&oldest);
            }
        }
        let record = inner.peers.entry(peer).or_default();
        record.touched = tick;
        f(&mut record.outcomes[transport.index()]);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("transport stats lock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> PeerId {
        PeerId::random()
    }

    #[test]
    fn backoff_doubles_to_a_cap() {
        assert_eq!(backoff_for(1), BACKOFF_BASE);
        assert_eq!(backoff_for(2), BACKOFF_BASE * 2);
        assert_eq!(backoff_for(3), BACKOFF_BASE * 4);
        assert_eq!(backoff_for(40), BACKOFF_MAX);
        assert_eq!(backoff_for(u32::MAX), BACKOFF_MAX);
    }

    #[test]
    fn a_failure_demotes_until_the_backoff_expires() {
        let (s, p, t0) = (TransportStats::default(), id(), Instant::now());
        assert!(!s.is_demoted_at(&p, Transport::Http, t0));
        s.record_failure_at(p, Transport::Http, t0);
        assert!(s.is_demoted_at(&p, Transport::Http, t0));
        assert!(s.is_demoted_at(
            &p,
            Transport::Http,
            t0 + BACKOFF_BASE - Duration::from_millis(1)
        ));
        assert!(!s.is_demoted_at(&p, Transport::Http, t0 + BACKOFF_BASE));
        // The other transport is untouched.
        assert!(!s.is_demoted_at(&p, Transport::Stream, t0));
    }

    #[test]
    fn consecutive_failures_lengthen_the_backoff_and_a_success_clears_it() {
        let (s, p, t0) = (TransportStats::default(), id(), Instant::now());
        s.record_failure_at(p, Transport::Stream, t0);
        s.record_failure_at(p, Transport::Stream, t0);
        assert!(s.is_demoted_at(
            &p,
            Transport::Stream,
            t0 + BACKOFF_BASE * 2 - Duration::from_millis(1)
        ));
        assert!(!s.is_demoted_at(&p, Transport::Stream, t0 + BACKOFF_BASE * 2));
        s.record_success_at(p, Transport::Stream, Duration::from_millis(5), t0);
        assert!(!s.is_demoted_at(&p, Transport::Stream, t0));
        let o = s.outcome(&p, Transport::Stream).unwrap();
        assert_eq!(o.consecutive_failures, 0);
        assert!(o.last_success.is_some() && o.last_failure.is_some());
    }

    #[test]
    fn rtt_is_a_moving_average() {
        let (s, p, t0) = (TransportStats::default(), id(), Instant::now());
        s.record_success_at(p, Transport::Http, Duration::from_millis(80), t0);
        assert_eq!(
            s.outcome(&p, Transport::Http).unwrap().rtt,
            Some(Duration::from_millis(80))
        );
        s.record_success_at(p, Transport::Http, Duration::from_millis(160), t0);
        assert_eq!(
            s.outcome(&p, Transport::Http).unwrap().rtt,
            Some(Duration::from_millis(90))
        );
    }

    #[test]
    fn choose_only_reorders_among_available_transports() {
        let (s, p) = (TransportStats::default(), id());
        assert_eq!(s.choose(&p, Transport::Http, true), Transport::Http);
        s.record_failure(p, Transport::Http);
        assert_eq!(s.choose(&p, Transport::Http, true), Transport::Stream);
        // No usable alternative: the demotion never invents one.
        assert_eq!(s.choose(&p, Transport::Http, false), Transport::Http);
        // Both demoted: the hint stands.
        s.record_failure(p, Transport::Stream);
        assert_eq!(s.choose(&p, Transport::Http, true), Transport::Http);
        assert_eq!(s.choose(&p, Transport::Stream, true), Transport::Stream);
    }

    #[test]
    fn tracking_is_bounded_and_drops_the_least_recently_touched() {
        let s = TransportStats::default();
        let first = id();
        s.record_failure(first, Transport::Http);
        for _ in 0..MAX_TRACKED_PEERS * 2 {
            s.record_failure(id(), Transport::Http);
        }
        assert_eq!(s.tracked(), MAX_TRACKED_PEERS);
        assert!(s.outcome(&first, Transport::Http).is_none());
    }

    #[test]
    fn touching_a_peer_keeps_it_through_eviction() {
        let s = TransportStats::default();
        let keep = id();
        s.record_failure(keep, Transport::Http);
        for i in 0..MAX_TRACKED_PEERS * 2 {
            s.record_failure(id(), Transport::Http);
            if i % 100 == 0 {
                s.record_failure(keep, Transport::Http);
            }
        }
        assert!(s.outcome(&keep, Transport::Http).is_some());
    }

    #[test]
    fn vet_results_expire_and_stay_bounded() {
        let (s, t0) = (TransportStats::default(), Instant::now());
        let p = id();
        assert!(s.cached_vet(p, "http://a", false, t0).is_none());
        s.store_vet(p, "http://a", false, None, t0);
        assert!(matches!(s.cached_vet(p, "http://a", false, t0), Some(None)));
        // The policy and URL are part of the key.
        assert!(s.cached_vet(p, "http://a", true, t0).is_none());
        assert!(s.cached_vet(p, "http://b", false, t0).is_none());
        assert!(s.cached_vet(p, "http://a", false, t0 + VET_TTL).is_none());
        for i in 0..MAX_VET_ENTRIES * 2 {
            s.store_vet(id(), &format!("http://h{i}"), false, None, t0);
        }
        assert!(s.lock().vetted.len() <= MAX_VET_ENTRIES);
    }
}
