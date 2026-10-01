//! Reachability detection state: what libp2p AutoNAT concluded about whether this node is
//! dialable, the addresses it confirmed, and the AutoNAT settings read from the environment.
//!
//! Detection runs inside the DHT swarm (`crate::dht`) and never blocks startup: until a probe
//! finishes the state is `unknown`. Reachability is a measurement; `connectivity` (see
//! `nodes.md`) is derived from it by [`connectivity_for`].

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use avalon_protocol::connectivity::Connectivity;
use libp2p::autonat;
use serde::Serialize;
use tokio::sync::{watch, Notify};
use tokio::time::Instant;

use crate::outbound_policy::{always_forbidden, OutboundPolicy};

/// Detected dialability of this node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reachability {
    /// No probe has finished, no peer could serve one, or detection is off.
    Unknown,
    /// A peer dialed this node back successfully.
    Public,
    /// Peers were asked to dial this node back and could not.
    Private,
}

impl Reachability {
    pub fn from_nat_status(status: &autonat::NatStatus) -> Self {
        match status {
            autonat::NatStatus::Public(_) => Reachability::Public,
            autonat::NatStatus::Private => Reachability::Private,
            autonat::NatStatus::Unknown => Reachability::Unknown,
        }
    }
}

/// `connectivity` reported for a detected reachability. `public` is `direct`; `private` is
/// `nat_traversed` while a hole-punched direct connection is open, else `relayed` while a relay
/// reservation is held and `outbound_only` otherwise; `unknown` reports nothing.
pub fn connectivity_for(snapshot: &ReachabilitySnapshot) -> Option<Connectivity> {
    match snapshot.reachability {
        Reachability::Public => Some(Connectivity::Direct),
        Reachability::Private if !snapshot.punched_peers.is_empty() => {
            Some(Connectivity::NatTraversed)
        }
        Reachability::Private if !snapshot.relay_reservations.is_empty() => {
            Some(Connectivity::Relayed)
        }
        Reachability::Private => Some(Connectivity::OutboundOnly),
        Reachability::Unknown => None,
    }
}

/// An accepted reservation this node holds on a relay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelayReservation {
    pub relay_peer_id: String,
    /// The `/p2p-circuit` address peers can dial to reach this node through the relay.
    pub relayed_addr: String,
    /// How many times the relay has renewed this reservation since it was accepted.
    pub renewals: u64,
}

/// Waits for the next announce round: `interval` from now, or sooner when the relayed addresses
/// change, but never sooner than `spacing` after `round_started`. Changes arriving while it
/// waits out the spacing are covered by the round it releases.
pub async fn wait_for_announce_round(
    reachability: Option<&ReachabilityHandle>,
    interval: Duration,
    round_started: Instant,
    spacing: Duration,
) {
    let Some(handle) = reachability else {
        tokio::time::sleep(interval).await;
        return;
    };
    tokio::select! {
        _ = tokio::time::sleep(interval) => {}
        _ = handle.relayed_addrs_changed() => {
            tokio::time::sleep_until(round_started + spacing.min(interval)).await;
            let _ = tokio::time::timeout(Duration::ZERO, handle.relayed_addrs_changed()).await;
        }
    }
}

/// Most recent hole-punch outcomes kept, newest last.
pub const MAX_HOLE_PUNCH_OUTCOMES: usize = 32;

/// The result of one hole-punch attempt with a peer. A failed attempt leaves the relayed
/// connection in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HolePunchOutcome {
    pub peer_id: String,
    /// `true` when a direct connection replaced the relayed one.
    pub succeeded: bool,
    /// Why the attempt failed; absent on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Point-in-time detection result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachabilitySnapshot {
    pub reachability: Reachability,
    /// Addresses a peer confirmed by dialing them, as multiaddr strings.
    pub confirmed_addrs: Vec<String>,
    /// Accepted relay reservations only; a requested one is not listed until the relay accepts.
    pub relay_reservations: Vec<RelayReservation>,
    /// Recent hole-punch attempts, oldest first, bounded by [`MAX_HOLE_PUNCH_OUTCOMES`].
    pub hole_punches: Vec<HolePunchOutcome>,
    /// Peers this node currently holds a hole-punched direct connection to.
    pub punched_peers: Vec<String>,
}

impl ReachabilitySnapshot {
    fn unknown() -> Self {
        Self {
            reachability: Reachability::Unknown,
            confirmed_addrs: Vec::new(),
            relay_reservations: Vec::new(),
            hole_punches: Vec::new(),
            punched_peers: Vec::new(),
        }
    }
}

/// Shared, cheaply cloneable view of the detection result. The DHT worker publishes into it;
/// status and announce read it.
#[derive(Debug, Clone)]
pub struct ReachabilityHandle {
    tx: Arc<watch::Sender<ReachabilitySnapshot>>,
    configured_addr: Option<String>,
    /// Set once at startup when this node serves as a relay.
    relay_server: Arc<std::sync::OnceLock<crate::relay::RelayServerView>>,
    /// Wakes the announce loop when the advertised relayed addresses change; a permit is kept
    /// for a change that lands while no one waits, so changes coalesce into one wake.
    relayed_addrs_changed: Arc<Notify>,
}

/// Least time between the starts of two announce rounds when a relay change brings one forward.
pub const MIN_EARLY_ANNOUNCE_SPACING: Duration = Duration::from_secs(30);

impl ReachabilityHandle {
    /// Detection is not running: reachability stays `unknown`.
    pub fn unknown() -> Self {
        Self::new(None)
    }

    /// `configured_addr` is the operator-set `AVALON_LIBP2P_EXTERNAL_ADDR`, if any.
    pub fn new(configured_addr: Option<String>) -> Self {
        Self {
            tx: Arc::new(watch::channel(ReachabilitySnapshot::unknown()).0),
            configured_addr,
            relay_server: Arc::default(),
            relayed_addrs_changed: Arc::default(),
        }
    }

    pub(crate) fn set_relay_server(&self, view: crate::relay::RelayServerView) {
        let _ = self.relay_server.set(view);
    }

    /// Limits and usage of the relay server role; `None` when this node does not relay.
    pub fn relay_server_status(&self) -> Option<crate::relay::RelayServerStatus> {
        self.relay_server.get().map(|v| v.status())
    }

    pub fn snapshot(&self) -> ReachabilitySnapshot {
        self.tx.borrow().clone()
    }

    /// Wakes when the snapshot changes.
    pub fn subscribe(&self) -> watch::Receiver<ReachabilitySnapshot> {
        self.tx.subscribe()
    }

    /// Addresses to put in announce: the confirmed ones, accepted relayed ones, plus the
    /// operator-set external address (stated, not inferred). Listen addresses are never
    /// advertised unconfirmed.
    pub fn advertised_addrs(&self) -> Vec<String> {
        let snap = self.snapshot();
        let mut addrs = snap.confirmed_addrs;
        addrs.extend(snap.relay_reservations.into_iter().map(|r| r.relayed_addr));
        if let Some(configured) = &self.configured_addr {
            if !addrs.contains(configured) {
                addrs.push(configured.clone());
            }
        }
        addrs
    }

    pub(crate) fn set_reachability(&self, reachability: Reachability) {
        self.tx.send_if_modified(|s| {
            let changed = s.reachability != reachability;
            s.reachability = reachability;
            changed
        });
    }

    pub(crate) fn set_relay_reservations(&self, reservations: Vec<RelayReservation>) {
        let mut addrs_changed = false;
        self.tx.send_if_modified(|s| {
            let changed = s.relay_reservations != reservations;
            // A renewal changes the snapshot but not what peers can dial.
            addrs_changed = changed
                && s.relay_reservations
                    .iter()
                    .map(|r| &r.relayed_addr)
                    .ne(reservations.iter().map(|r| &r.relayed_addr));
            s.relay_reservations = reservations;
            changed
        });
        if addrs_changed {
            self.relayed_addrs_changed.notify_one();
        }
    }

    /// Resolves once the advertised relayed addresses have changed since the last wait.
    pub async fn relayed_addrs_changed(&self) {
        self.relayed_addrs_changed.notified().await;
    }

    pub(crate) fn record_hole_punch(&self, outcome: HolePunchOutcome) {
        self.tx.send_modify(|s| {
            s.hole_punches.push(outcome);
            let excess = s.hole_punches.len().saturating_sub(MAX_HOLE_PUNCH_OUTCOMES);
            s.hole_punches.drain(..excess);
        });
    }

    pub(crate) fn set_punched_peers(&self, peers: Vec<String>) {
        self.tx.send_if_modified(|s| {
            let changed = s.punched_peers != peers;
            s.punched_peers = peers;
            changed
        });
    }

    pub(crate) fn confirm_addr(&self, addr: String) {
        self.tx.send_if_modified(|s| {
            if s.confirmed_addrs.contains(&addr) {
                return false;
            }
            s.confirmed_addrs.push(addr);
            true
        });
    }

    pub(crate) fn expire_addr(&self, addr: &str) {
        self.tx.send_if_modified(|s| {
            let before = s.confirmed_addrs.len();
            s.confirmed_addrs.retain(|a| a != addr);
            s.confirmed_addrs.len() != before
        });
    }
}

/// Per-peer dial-backs allowed per rate-limit period.
const PER_PEER_DIALBACKS: usize = 3;
const DIALBACK_PERIOD: Duration = Duration::from_secs(60);
const DEFAULT_DIALBACKS_PER_MINUTE: usize = 30;

/// AutoNAT settings resolved once at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutonatSettings {
    /// `AVALON_AUTONAT_ENABLED` (default true).
    pub enabled: bool,
    /// Whether private and loopback peers may be dialed back and used as servers.
    pub allow_private: bool,
    /// `AVALON_AUTONAT_DIALBACKS_PER_MINUTE`: dial-backs this node performs for others, in total.
    pub dialbacks_per_minute: usize,
    pub boot_delay: Duration,
    pub retry_interval: Duration,
    pub refresh_interval: Duration,
    pub throttle_server_period: Duration,
}

impl Default for AutonatSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_private: false,
            dialbacks_per_minute: DEFAULT_DIALBACKS_PER_MINUTE,
            boot_delay: Duration::from_secs(10),
            retry_interval: Duration::from_secs(30),
            refresh_interval: Duration::from_secs(15 * 60),
            throttle_server_period: Duration::from_secs(30),
        }
    }
}

fn env_flag(name: &str) -> Option<bool> {
    std::env::var(name).ok().map(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        )
    })
}

impl AutonatSettings {
    /// `AVALON_AUTONAT_ENABLED` (default true), `AVALON_AUTONAT_ALLOW_PRIVATE_DIALBACK`
    /// (default false; only takes effect together with `AVALON_ALLOW_PRIVATE_PEERS`) and
    /// `AVALON_AUTONAT_DIALBACKS_PER_MINUTE` (default 30, `0` answers none).
    pub fn from_env(policy: OutboundPolicy) -> Result<Self, String> {
        let enabled = env_flag("AVALON_AUTONAT_ENABLED").unwrap_or(true);
        let wants_private = env_flag("AVALON_AUTONAT_ALLOW_PRIVATE_DIALBACK").unwrap_or(false);
        if wants_private && !policy.allow_private {
            tracing::warn!(
                "avalon-dht: AVALON_AUTONAT_ALLOW_PRIVATE_DIALBACK ignored, the outbound policy \
                 refuses private addresses (AVALON_ALLOW_PRIVATE_PEERS is not true)"
            );
        }
        let dialbacks_per_minute = match std::env::var("AVALON_AUTONAT_DIALBACKS_PER_MINUTE") {
            Ok(raw) => raw.trim().parse().map_err(|_| {
                format!(
                    "AVALON_AUTONAT_DIALBACKS_PER_MINUTE must be a non-negative integer, got {raw:?}"
                )
            })?,
            Err(_) => DEFAULT_DIALBACKS_PER_MINUTE,
        };
        Ok(Self {
            enabled,
            allow_private: wants_private && policy.allow_private,
            dialbacks_per_minute,
            ..Self::default()
        })
    }

    /// The libp2p AutoNAT configuration these settings describe.
    pub fn libp2p_config(&self) -> autonat::Config {
        autonat::Config {
            boot_delay: self.boot_delay,
            retry_interval: self.retry_interval,
            refresh_interval: self.refresh_interval,
            throttle_server_period: self.throttle_server_period,
            throttle_clients_global_max: self.dialbacks_per_minute,
            throttle_clients_peer_max: PER_PEER_DIALBACKS.min(self.dialbacks_per_minute),
            throttle_clients_period: DIALBACK_PERIOD,
            only_global_ips: !self.allow_private,
            ..autonat::Config::default()
        }
    }
}

/// Whether a peer connecting from `ip` may be kept, since AutoNAT dials back to that address.
/// Addresses the outbound policy always refuses are never allowed.
pub fn peer_ip_allowed(ip: IpAddr) -> bool {
    !always_forbidden(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> libp2p::Multiaddr {
        s.parse().unwrap()
    }

    #[test]
    fn nat_status_maps_to_reachability() {
        let public = autonat::NatStatus::Public(addr("/ip4/203.0.113.7/tcp/4001"));
        assert_eq!(Reachability::from_nat_status(&public), Reachability::Public);
        assert_eq!(
            Reachability::from_nat_status(&autonat::NatStatus::Private),
            Reachability::Private
        );
        assert_eq!(
            Reachability::from_nat_status(&autonat::NatStatus::Unknown),
            Reachability::Unknown
        );
    }

    #[test]
    fn connectivity_mapping_never_claims_direct_without_detection() {
        let at = |reachability, reserved: bool| {
            let relay_reservations = reserved
                .then(|| RelayReservation {
                    relay_peer_id: "12D3KooWRelay".into(),
                    relayed_addr: "/ip4/203.0.113.7/tcp/4001/p2p-circuit".into(),
                    renewals: 0,
                })
                .into_iter()
                .collect();
            ReachabilitySnapshot {
                reachability,
                confirmed_addrs: Vec::new(),
                relay_reservations,
                hole_punches: Vec::new(),
                punched_peers: Vec::new(),
            }
        };
        assert_eq!(
            connectivity_for(&at(Reachability::Public, false)),
            Some(Connectivity::Direct)
        );
        assert_eq!(
            connectivity_for(&at(Reachability::Private, false)),
            Some(Connectivity::OutboundOnly)
        );
        assert_eq!(
            connectivity_for(&at(Reachability::Private, true)),
            Some(Connectivity::Relayed)
        );
        assert_eq!(connectivity_for(&at(Reachability::Unknown, false)), None);
        // A reservation never upgrades an undetected node.
        assert_eq!(connectivity_for(&at(Reachability::Unknown, true)), None);
    }

    #[test]
    fn an_open_punched_connection_makes_a_private_node_nat_traversed() {
        let mut snap = ReachabilitySnapshot::unknown();
        snap.reachability = Reachability::Private;
        snap.punched_peers = vec!["12D3KooWPeer".into()];
        assert_eq!(connectivity_for(&snap), Some(Connectivity::NatTraversed));
        // Even with a reservation held, the direct connection is the better path.
        snap.relay_reservations = vec![RelayReservation {
            relay_peer_id: "12D3KooWRelay".into(),
            relayed_addr: "/ip4/203.0.113.7/tcp/4001/p2p-circuit".into(),
            renewals: 0,
        }];
        assert_eq!(connectivity_for(&snap), Some(Connectivity::NatTraversed));
        snap.punched_peers.clear();
        assert_eq!(connectivity_for(&snap), Some(Connectivity::Relayed));
        // A punched connection never upgrades an undetected or public node's report.
        snap.punched_peers = vec!["12D3KooWPeer".into()];
        snap.reachability = Reachability::Unknown;
        assert_eq!(connectivity_for(&snap), None);
        snap.reachability = Reachability::Public;
        assert_eq!(connectivity_for(&snap), Some(Connectivity::Direct));
    }

    fn reservation(addr: &str, renewals: u64) -> RelayReservation {
        RelayReservation {
            relay_peer_id: "12D3KooWRelay".into(),
            relayed_addr: addr.into(),
            renewals,
        }
    }

    const SPACING: Duration = MIN_EARLY_ANNOUNCE_SPACING;
    const INTERVAL: Duration = Duration::from_secs(180);

    /// Virtual time `wait_for_announce_round` takes while `change` runs 5 s into the wait.
    async fn waited(
        handle: &ReachabilityHandle,
        round_age: Duration,
        interval: Duration,
        change: impl FnOnce(&ReachabilityHandle),
    ) -> Duration {
        let started = Instant::now();
        let round_started = started - round_age;
        let h = handle.clone();
        let waiter = tokio::spawn(async move {
            wait_for_announce_round(Some(&h), interval, round_started, SPACING).await;
        });
        tokio::time::sleep(Duration::from_secs(5)).await;
        change(handle);
        waiter.await.unwrap();
        started.elapsed()
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_relayed_address_brings_the_announce_forward_to_the_spacing() {
        let handle = ReachabilityHandle::unknown();
        let took = waited(&handle, Duration::ZERO, INTERVAL, |h| {
            h.set_relay_reservations(vec![reservation("/a", 0)]);
        })
        .await;
        assert_eq!(
            took, SPACING,
            "not the full interval, not before the spacing"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_spacing_counts_from_the_start_of_the_last_round() {
        let handle = ReachabilityHandle::unknown();
        let took = waited(&handle, Duration::from_secs(100), INTERVAL, |h| {
            h.set_relay_reservations(vec![reservation("/a", 0)]);
        })
        .await;
        assert_eq!(
            took,
            Duration::from_secs(5),
            "the round ended long enough ago"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_change_or_on_a_renewal_the_loop_keeps_its_interval() {
        let handle = ReachabilityHandle::unknown();
        handle.set_relay_reservations(vec![reservation("/a", 0)]);
        // Consume the wake of that first change.
        waited(&handle, Duration::ZERO, INTERVAL, |_| {}).await;
        let idle = waited(&handle, Duration::ZERO, INTERVAL, |_| {}).await;
        assert_eq!(idle, INTERVAL);
        let renewed = waited(&handle, Duration::ZERO, INTERVAL, |h| {
            h.set_relay_reservations(vec![reservation("/a", 1)]);
        })
        .await;
        assert_eq!(renewed, INTERVAL, "a renewal is not an address change");
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_of_changes_is_one_early_round() {
        let handle = ReachabilityHandle::unknown();
        let burst = waited(&handle, Duration::ZERO, INTERVAL, |h| {
            for i in 0..50 {
                h.set_relay_reservations(vec![reservation(&format!("/a{i}"), 0)]);
            }
        })
        .await;
        assert_eq!(burst, SPACING);
        let next = waited(&handle, Duration::ZERO, INTERVAL, |_| {}).await;
        assert_eq!(
            next, INTERVAL,
            "the burst did not leave a second wake behind"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn changes_during_the_spacing_wait_are_covered_by_the_round_it_releases() {
        let handle = ReachabilityHandle::unknown();
        let h = handle.clone();
        let started = Instant::now();
        let waiter = tokio::spawn(async move {
            wait_for_announce_round(Some(&h), INTERVAL, started, SPACING).await;
        });
        tokio::time::sleep(Duration::from_secs(5)).await;
        handle.set_relay_reservations(vec![reservation("/a", 0)]);
        tokio::time::sleep(Duration::from_secs(5)).await;
        handle.set_relay_reservations(vec![reservation("/b", 0)]);
        waiter.await.unwrap();
        assert_eq!(started.elapsed(), SPACING);
        let next = waited(&handle, Duration::ZERO, INTERVAL, |_| {}).await;
        assert_eq!(next, INTERVAL);
    }

    #[tokio::test(start_paused = true)]
    async fn an_interval_shorter_than_the_spacing_still_wins() {
        let handle = ReachabilityHandle::unknown();
        let took = waited(&handle, Duration::ZERO, Duration::from_secs(10), |h| {
            h.set_relay_reservations(vec![reservation("/a", 0)]);
        })
        .await;
        assert_eq!(took, Duration::from_secs(10));
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_reachability_handle_it_just_sleeps_the_interval() {
        let started = Instant::now();
        wait_for_announce_round(None, INTERVAL, started, SPACING).await;
        assert_eq!(started.elapsed(), INTERVAL);
    }

    #[test]
    fn hole_punch_outcomes_are_bounded_and_keep_the_newest() {
        let handle = ReachabilityHandle::unknown();
        for i in 0..MAX_HOLE_PUNCH_OUTCOMES + 5 {
            handle.record_hole_punch(HolePunchOutcome {
                peer_id: format!("peer-{i}"),
                succeeded: i % 2 == 0,
                error: (i % 2 != 0).then(|| "attempts exceeded".to_string()),
            });
        }
        let outcomes = handle.snapshot().hole_punches;
        assert_eq!(outcomes.len(), MAX_HOLE_PUNCH_OUTCOMES);
        assert_eq!(outcomes[0].peer_id, "peer-5");
        assert_eq!(
            outcomes.last().unwrap().peer_id,
            format!("peer-{}", MAX_HOLE_PUNCH_OUTCOMES + 4)
        );
        let json = serde_json::to_value(&outcomes[0]).unwrap();
        assert_eq!(json["succeeded"], false);
        assert_eq!(json["error"], "attempts exceeded");
        assert!(serde_json::to_value(&outcomes[1])
            .unwrap()
            .get("error")
            .is_none());
    }

    #[test]
    fn accepted_relayed_addresses_are_advertised_and_dropped_with_the_reservation() {
        let handle = ReachabilityHandle::unknown();
        let addr = "/ip4/203.0.113.7/tcp/4001/p2p/12D3KooWRelay/p2p-circuit/p2p/12D3KooWMe";
        handle.set_relay_reservations(vec![RelayReservation {
            relay_peer_id: "12D3KooWRelay".into(),
            relayed_addr: addr.into(),
            renewals: 0,
        }]);
        assert_eq!(handle.advertised_addrs(), vec![addr]);
        assert!(handle.snapshot().confirmed_addrs.is_empty());
        handle.set_relay_reservations(Vec::new());
        assert!(handle.advertised_addrs().is_empty());
    }

    #[test]
    fn reachability_serializes_as_snake_case() {
        for (r, name) in [
            (Reachability::Unknown, "unknown"),
            (Reachability::Public, "public"),
            (Reachability::Private, "private"),
        ] {
            assert_eq!(serde_json::to_value(r).unwrap(), name);
        }
    }

    #[test]
    fn handle_starts_unknown_and_tracks_confirmed_addresses() {
        let handle = ReachabilityHandle::unknown();
        let snap = handle.snapshot();
        assert_eq!(snap.reachability, Reachability::Unknown);
        assert!(snap.confirmed_addrs.is_empty());
        assert!(handle.advertised_addrs().is_empty());

        handle.set_reachability(Reachability::Public);
        handle.confirm_addr("/ip4/203.0.113.7/tcp/4001".into());
        handle.confirm_addr("/ip4/203.0.113.7/tcp/4001".into());
        assert_eq!(handle.snapshot().reachability, Reachability::Public);
        assert_eq!(handle.advertised_addrs(), vec!["/ip4/203.0.113.7/tcp/4001"]);

        handle.expire_addr("/ip4/203.0.113.7/tcp/4001");
        assert!(handle.advertised_addrs().is_empty());
    }

    #[test]
    fn advertised_addrs_add_the_configured_address_once() {
        let handle = ReachabilityHandle::new(Some("/ip4/198.51.100.1/tcp/4001".into()));
        assert_eq!(
            handle.advertised_addrs(),
            vec!["/ip4/198.51.100.1/tcp/4001"]
        );
        handle.confirm_addr("/ip4/198.51.100.1/tcp/4001".into());
        assert_eq!(
            handle.advertised_addrs(),
            vec!["/ip4/198.51.100.1/tcp/4001"]
        );
        assert!(handle.snapshot().confirmed_addrs.len() == 1);
    }

    #[tokio::test]
    async fn subscribers_are_woken_only_by_real_changes() {
        let handle = ReachabilityHandle::unknown();
        let mut rx = handle.subscribe();
        handle.set_reachability(Reachability::Unknown);
        assert!(!rx.has_changed().unwrap());
        handle.set_reachability(Reachability::Private);
        rx.changed().await.unwrap();
        assert_eq!(rx.borrow().reachability, Reachability::Private);
    }

    #[test]
    fn rate_limit_settings_map_to_the_libp2p_server_limits() {
        let settings = AutonatSettings {
            dialbacks_per_minute: 12,
            ..AutonatSettings::default()
        };
        let cfg = settings.libp2p_config();
        assert_eq!(cfg.throttle_clients_global_max, 12);
        assert_eq!(cfg.throttle_clients_peer_max, 3);
        assert_eq!(cfg.throttle_clients_period, Duration::from_secs(60));

        let tight = AutonatSettings {
            dialbacks_per_minute: 1,
            ..AutonatSettings::default()
        };
        assert_eq!(tight.libp2p_config().throttle_clients_peer_max, 1);
        let none = AutonatSettings {
            dialbacks_per_minute: 0,
            ..AutonatSettings::default()
        };
        assert_eq!(none.libp2p_config().throttle_clients_global_max, 0);
    }

    #[test]
    fn private_dialback_is_off_by_default_and_needs_the_outbound_policy() {
        assert!(AutonatSettings::default().libp2p_config().only_global_ips);
        let allowed = AutonatSettings {
            allow_private: true,
            ..AutonatSettings::default()
        };
        assert!(!allowed.libp2p_config().only_global_ips);
    }

    #[test]
    fn env_defaults_and_overrides() {
        let _env = crate::test_env::guard();
        let clear = || unsafe {
            std::env::remove_var("AVALON_AUTONAT_ENABLED");
            std::env::remove_var("AVALON_AUTONAT_ALLOW_PRIVATE_DIALBACK");
            std::env::remove_var("AVALON_AUTONAT_DIALBACKS_PER_MINUTE");
        };
        clear();
        let defaults = AutonatSettings::from_env(OutboundPolicy::new(true)).unwrap();
        assert!(defaults.enabled);
        assert!(!defaults.allow_private);
        assert_eq!(defaults.dialbacks_per_minute, 30);

        unsafe {
            std::env::set_var("AVALON_AUTONAT_ALLOW_PRIVATE_DIALBACK", "true");
            std::env::set_var("AVALON_AUTONAT_DIALBACKS_PER_MINUTE", "5");
            std::env::set_var("AVALON_AUTONAT_ENABLED", "false");
        }
        let on = AutonatSettings::from_env(OutboundPolicy::new(true)).unwrap();
        assert!(on.allow_private);
        assert!(!on.enabled);
        assert_eq!(on.dialbacks_per_minute, 5);
        // The knob cannot widen the outbound policy.
        let refused = AutonatSettings::from_env(OutboundPolicy::new(false)).unwrap();
        assert!(!refused.allow_private);

        unsafe {
            std::env::set_var("AVALON_AUTONAT_DIALBACKS_PER_MINUTE", "lots");
        }
        assert!(AutonatSettings::from_env(OutboundPolicy::new(true)).is_err());
        clear();
    }

    #[test]
    fn peers_at_always_forbidden_addresses_are_refused() {
        for a in ["169.254.169.254", "fe80::1", "224.0.0.1", "0.0.0.0"] {
            assert!(!peer_ip_allowed(a.parse().unwrap()), "{a}");
        }
        for a in ["203.0.113.7", "192.168.1.5", "127.0.0.1"] {
            assert!(peer_ip_allowed(a.parse().unwrap()), "{a}");
        }
    }
}
