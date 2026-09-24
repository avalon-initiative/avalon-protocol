//! This node's active announce/exchange set and its observer-relative
//! round-trip statistics per active neighbor.
//!
//! Measurements are application-level round trips (network plus the peer's
//! request handling) taken by this node alone. They are never gossiped and
//! never influence peer admission, pruning, the version floor, or any trust
//! decision. State is in-memory and bounded by the active-set cap.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};

use serde::Serialize;

use crate::network_coordinates::{self, Coordinate};
use time::OffsetDateTime;

/// Smoothing factor of the exponentially weighted moving average.
pub const EWMA_ALPHA: f64 = 0.2;
/// Number of most recent successful round trips kept for `min_ms` and `jitter_ms`.
pub const RTT_WINDOW: usize = 20;
/// Number of most recent attempts kept for the loss ratio.
pub const LOSS_WINDOW: usize = 20;

/// Rolling round-trip statistics for one neighbor, as observed by this node.
#[derive(Debug, Clone, Serialize)]
pub struct RoundTripStats {
    /// Always `"application_round_trip"`: announce request to parsed response.
    pub measurement: &'static str,
    /// Round trip of the most recent successful announce, in milliseconds.
    pub last_ms: Option<f64>,
    /// Exponentially weighted moving average of successful round trips.
    pub ewma_ms: Option<f64>,
    /// Minimum over the last successful round trips.
    pub min_ms: Option<f64>,
    /// Mean absolute deviation over the last successful round trips.
    pub jitter_ms: Option<f64>,
    /// Successful round trips recorded since the peer became active.
    pub samples: u64,
    /// Failed attempts in the last window of attempts.
    pub failed_recent: u32,
    /// Attempts in the last window (at most the window size).
    pub attempts_recent: u32,
    /// Fraction of recent attempts that failed, 0.0 when none were made.
    pub loss_ratio: f64,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_success_at: Option<OffsetDateTime>,
}

#[derive(Debug, Default)]
struct PeerRtt {
    last_ms: Option<f64>,
    ewma_ms: Option<f64>,
    window: VecDeque<f64>,
    samples: u64,
    attempts: VecDeque<bool>,
    last_success_at: Option<OffsetDateTime>,
}

impl PeerRtt {
    fn push_attempt(&mut self, ok: bool) {
        if self.attempts.len() == LOSS_WINDOW {
            self.attempts.pop_front();
        }
        self.attempts.push_back(ok);
    }

    fn record_success(&mut self, ms: f64, at: OffsetDateTime) {
        self.push_attempt(true);
        self.last_ms = Some(ms);
        self.ewma_ms = Some(match self.ewma_ms {
            Some(prev) => EWMA_ALPHA * ms + (1.0 - EWMA_ALPHA) * prev,
            None => ms,
        });
        if self.window.len() == RTT_WINDOW {
            self.window.pop_front();
        }
        self.window.push_back(ms);
        self.samples += 1;
        self.last_success_at = Some(at);
    }

    fn snapshot(&self) -> RoundTripStats {
        let n = self.window.len();
        let min_ms = self.window.iter().cloned().reduce(f64::min);
        let jitter_ms = (n > 0).then(|| {
            let mean = self.window.iter().sum::<f64>() / n as f64;
            self.window.iter().map(|v| (v - mean).abs()).sum::<f64>() / n as f64
        });
        let attempts = self.attempts.len() as u32;
        let failed = self.attempts.iter().filter(|ok| !**ok).count() as u32;
        RoundTripStats {
            measurement: "application_round_trip",
            last_ms: self.last_ms,
            ewma_ms: self.ewma_ms,
            min_ms,
            jitter_ms,
            samples: self.samples,
            failed_recent: failed,
            attempts_recent: attempts,
            loss_ratio: if attempts == 0 {
                0.0
            } else {
                failed as f64 / attempts as f64
            },
            last_success_at: self.last_success_at,
        }
    }
}

#[derive(Debug, Default)]
struct Inner {
    active: Vec<String>,
    bootstrap: Vec<String>,
    own_libp2p_peer_id: Option<String>,
    stats: HashMap<String, PeerRtt>,
    own_coordinate: Coordinate,
    /// Last valid coordinate each active neighbor reported for itself.
    neighbor_coordinates: HashMap<String, Coordinate>,
    coordinate_updates_rejected: u64,
}

/// Shared, cheaply cloneable view of the announce worker's active set and
/// the per-neighbor statistics. Written by the announce worker only.
#[derive(Clone, Default)]
pub struct NeighborTable {
    inner: Arc<RwLock<Inner>>,
}

impl NeighborTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records this node's own libp2p peer id for the topology read model.
    pub fn set_own_libp2p_peer_id(&self, peer_id: Option<String>) {
        self.inner
            .write()
            .expect("neighbor table lock poisoned")
            .own_libp2p_peer_id = peer_id;
    }

    pub fn own_libp2p_peer_id(&self) -> Option<String> {
        self.inner
            .read()
            .expect("neighbor table lock poisoned")
            .own_libp2p_peer_id
            .clone()
    }

    /// Replaces the active set and drops statistics of peers that left it.
    pub fn set_active(&self, active: &[String], bootstrap: &[String]) {
        let mut inner = self.inner.write().expect("neighbor table lock poisoned");
        inner.stats.retain(|url, _| active.contains(url));
        inner
            .neighbor_coordinates
            .retain(|url, _| active.contains(url));
        inner.active = active.to_vec();
        inner.bootstrap = bootstrap.to_vec();
    }

    /// Base URLs in the active set or the bootstrap list; the peer table never evicts these.
    pub fn protected_urls(&self) -> std::collections::HashSet<String> {
        let inner = self.inner.read().expect("neighbor table lock poisoned");
        inner
            .active
            .iter()
            .chain(inner.bootstrap.iter())
            .cloned()
            .collect()
    }

    /// Records a successful round trip; ignored for peers outside the active set.
    pub fn record_success(&self, peer: &str, rtt: std::time::Duration) {
        let mut inner = self.inner.write().expect("neighbor table lock poisoned");
        if inner.active.iter().any(|p| p == peer) {
            inner
                .stats
                .entry(peer.to_string())
                .or_default()
                .record_success(rtt.as_secs_f64() * 1000.0, OffsetDateTime::now_utc());
        }
    }

    /// This node's own network coordinate; the only coordinate it publishes.
    pub fn own_coordinate(&self) -> Coordinate {
        self.inner
            .read()
            .expect("neighbor table lock poisoned")
            .own_coordinate
    }

    /// Updates the own coordinate from a successful round trip to `peer`
    /// whose self-reported coordinate is `remote`. An invalid coordinate is
    /// counted and ignored; peers outside the active set are ignored.
    pub fn observe_coordinate(&self, peer: &str, remote: &Coordinate, rtt: std::time::Duration) {
        let mut inner = self.inner.write().expect("neighbor table lock poisoned");
        if !inner.active.iter().any(|p| p == peer) {
            return;
        }
        match network_coordinates::update(
            &inner.own_coordinate,
            remote,
            rtt.as_secs_f64() * 1000.0,
            network_coordinates::seed_for(peer),
        ) {
            Ok(next) => {
                inner.own_coordinate = next;
                inner.neighbor_coordinates.insert(peer.to_string(), *remote);
            }
            Err(reason) => {
                inner.coordinate_updates_rejected += 1;
                tracing::debug!(peer = %peer, ?reason, "ignored network coordinate");
            }
        }
    }

    /// Number of coordinate updates ignored because of invalid input.
    pub fn coordinate_updates_rejected(&self) -> u64 {
        self.inner
            .read()
            .expect("neighbor table lock poisoned")
            .coordinate_updates_rejected
    }

    /// Records a failed or timed-out attempt as loss; never as latency.
    pub fn record_failure(&self, peer: &str) {
        let mut inner = self.inner.write().expect("neighbor table lock poisoned");
        if inner.active.iter().any(|p| p == peer) {
            inner
                .stats
                .entry(peer.to_string())
                .or_default()
                .push_attempt(false);
        }
    }

    /// Active peers in worker order with bootstrap flag and stats, if any yet.
    pub fn snapshot(&self) -> Vec<NeighborSnapshot> {
        let inner = self.inner.read().expect("neighbor table lock poisoned");
        inner
            .active
            .iter()
            .map(|url| NeighborSnapshot {
                base_url: url.clone(),
                bootstrap: inner.bootstrap.contains(url),
                round_trip: inner.stats.get(url).map(PeerRtt::snapshot),
                coordinate: inner.neighbor_coordinates.get(url).copied(),
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct NeighborSnapshot {
    pub base_url: String,
    pub bootstrap: bool,
    pub round_trip: Option<RoundTripStats>,
    /// The neighbor's own coordinate as it last reported it.
    pub coordinate: Option<Coordinate>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn urls(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn ewma_seeds_then_smooths() {
        let t = NeighborTable::new();
        t.set_active(&urls(&["a"]), &[]);
        t.record_success("a", Duration::from_millis(100));
        t.record_success("a", Duration::from_millis(200));
        let s = t.snapshot()[0].round_trip.clone().unwrap();
        assert!((s.ewma_ms.unwrap() - 120.0).abs() < 1e-6);
        assert!((s.last_ms.unwrap() - 200.0).abs() < 1e-6);
        assert_eq!(s.samples, 2);
    }

    #[test]
    fn min_and_jitter_use_a_bounded_window() {
        let t = NeighborTable::new();
        t.set_active(&urls(&["a"]), &[]);
        t.record_success("a", Duration::from_millis(1));
        for _ in 0..RTT_WINDOW {
            t.record_success("a", Duration::from_millis(50));
        }
        let s = t.snapshot()[0].round_trip.clone().unwrap();
        assert!((s.min_ms.unwrap() - 50.0).abs() < 1e-6);
        assert!(s.jitter_ms.unwrap().abs() < 1e-6);
        assert_eq!(s.samples, RTT_WINDOW as u64 + 1);
    }

    #[test]
    fn jitter_is_mean_absolute_deviation() {
        let t = NeighborTable::new();
        t.set_active(&urls(&["a"]), &[]);
        t.record_success("a", Duration::from_millis(10));
        t.record_success("a", Duration::from_millis(30));
        let s = t.snapshot()[0].round_trip.clone().unwrap();
        assert!((s.jitter_ms.unwrap() - 10.0).abs() < 1e-6);
    }

    #[test]
    fn failures_count_as_loss_and_never_as_latency() {
        let t = NeighborTable::new();
        t.set_active(&urls(&["a"]), &[]);
        t.record_success("a", Duration::from_millis(40));
        t.record_failure("a");
        let s = t.snapshot()[0].round_trip.clone().unwrap();
        assert_eq!(s.samples, 1);
        assert!((s.ewma_ms.unwrap() - 40.0).abs() < 1e-6);
        assert_eq!((s.failed_recent, s.attempts_recent), (1, 2));
        assert!((s.loss_ratio - 0.5).abs() < 1e-9);
    }

    #[test]
    fn only_failures_leave_latency_empty() {
        let t = NeighborTable::new();
        t.set_active(&urls(&["a"]), &[]);
        t.record_failure("a");
        let s = t.snapshot()[0].round_trip.clone().unwrap();
        assert!(s.last_ms.is_none() && s.ewma_ms.is_none() && s.min_ms.is_none());
        assert_eq!(s.loss_ratio, 1.0);
        assert!(s.last_success_at.is_none());
    }

    #[test]
    fn loss_window_is_bounded() {
        let t = NeighborTable::new();
        t.set_active(&urls(&["a"]), &[]);
        for _ in 0..LOSS_WINDOW {
            t.record_failure("a");
        }
        for _ in 0..LOSS_WINDOW {
            t.record_success("a", Duration::from_millis(5));
        }
        let s = t.snapshot()[0].round_trip.clone().unwrap();
        assert_eq!(s.attempts_recent as usize, LOSS_WINDOW);
        assert_eq!(s.failed_recent, 0);
    }

    #[test]
    fn stats_are_dropped_when_a_peer_leaves_the_active_set() {
        let t = NeighborTable::new();
        t.set_active(&urls(&["a", "b"]), &urls(&["a"]));
        t.record_success("a", Duration::from_millis(5));
        t.record_success("b", Duration::from_millis(5));
        t.set_active(&urls(&["a"]), &urls(&["a"]));
        assert_eq!(t.snapshot().len(), 1);
        t.set_active(&urls(&["a", "b"]), &urls(&["a"]));
        let snap = t.snapshot();
        assert!(snap[0].round_trip.is_some());
        assert!(snap[1].round_trip.is_none());
        assert!(snap[0].bootstrap && !snap[1].bootstrap);
    }

    #[test]
    fn coordinate_moves_on_valid_reports_and_counts_invalid_ones() {
        let t = NeighborTable::new();
        t.set_active(&urls(&["a"]), &[]);
        let start = t.own_coordinate();
        let remote = Coordinate::default();
        t.observe_coordinate("a", &remote, Duration::from_millis(40));
        assert_ne!(t.own_coordinate(), start);
        assert_eq!(t.snapshot()[0].coordinate, Some(remote));

        let moved = t.own_coordinate();
        let bad = Coordinate {
            error: f64::NAN,
            ..remote
        };
        t.observe_coordinate("a", &bad, Duration::from_millis(40));
        t.observe_coordinate("zzz", &remote, Duration::from_millis(40));
        assert_eq!(t.own_coordinate(), moved);
        assert_eq!(t.coordinate_updates_rejected(), 1);
        assert_eq!(t.snapshot()[0].coordinate, Some(remote));

        t.set_active(&[], &[]);
        t.set_active(&urls(&["a"]), &[]);
        assert!(t.snapshot()[0].coordinate.is_none());
    }

    #[test]
    fn measurements_for_non_active_peers_are_ignored() {
        let t = NeighborTable::new();
        t.set_active(&urls(&["a"]), &[]);
        t.record_success("z", Duration::from_millis(5));
        t.record_failure("z");
        assert_eq!(t.snapshot().len(), 1);
    }
}
