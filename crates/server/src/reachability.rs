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
use tokio::sync::watch;

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
/// `relayed` while a relay reservation is held and `outbound_only` otherwise; `unknown` reports
/// nothing. `nat_traversed` needs hole punching, which does not exist yet.
pub fn connectivity_for(snapshot: &ReachabilitySnapshot) -> Option<Connectivity> {
    match snapshot.reachability {
        Reachability::Public => Some(Connectivity::Direct),
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

/// Point-in-time detection result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachabilitySnapshot {
    pub reachability: Reachability,
    /// Addresses a peer confirmed by dialing them, as multiaddr strings.
    pub confirmed_addrs: Vec<String>,
    /// Accepted relay reservations only; a requested one is not listed until the relay accepts.
    pub relay_reservations: Vec<RelayReservation>,
}

impl ReachabilitySnapshot {
    fn unknown() -> Self {
        Self {
            reachability: Reachability::Unknown,
            confirmed_addrs: Vec::new(),
            relay_reservations: Vec::new(),
        }
    }
}

/// Shared, cheaply cloneable view of the detection result. The DHT worker publishes into it;
/// status and announce read it.
#[derive(Debug, Clone)]
pub struct ReachabilityHandle {
    tx: Arc<watch::Sender<ReachabilitySnapshot>>,
    configured_addr: Option<String>,
}

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
        }
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
        self.tx.send_if_modified(|s| {
            let changed = s.relay_reservations != reservations;
            s.relay_reservations = reservations;
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
