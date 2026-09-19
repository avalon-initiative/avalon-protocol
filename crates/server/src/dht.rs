//! Interest-scoped realtime routing via a libp2p Kademlia DHT — issue #582,
//! part of epic #580 implementing #542's decision. This module owns exactly
//! two things: this node's libp2p identity, and bootstrapping that identity
//! into a live DHT routing table using #362's *existing* HTTP peer table
//! (`crate::nodes`) as the seed/bootstrap list, rather than inventing a
//! second, separate discovery mechanism. No interest registration/lookup
//! logic lives here yet — that's #583.
//!
//! **A libp2p `PeerId` is a brand-new identity domain, not a reuse of any
//! existing key.** This codebase already has three separate key domains
//! (player keys #73, issuer keys #80/#84, the settlement log operator's key
//! #39 — see `avalon_chain::sth`) and none of them fit: player/issuer keys
//! are about attestation/authorship, never held by a server process at all,
//! and the settlement key's lifecycle (rotatable, tied to STH-signing) is
//! semantically unrelated to peer-transport identity. A node now holds a
//! *fourth*, independent identity purely for DHT transport — see
//! [`load_or_generate_identity_from_env`].
//!
//! **Bootstrap reuses #362, it doesn't replace it.** `PeerInfo` (extended by
//! this issue with `libp2p_peer_id`/`libp2p_listen_addrs`) is gossiped
//! through the exact same `POST /nodes/announce` mechanism `crate::nodes`
//! already runs — a peer's DHT identity just rides along with everything
//! else it already announces. [`run_worker`] here does no announcing of its
//! own; it only watches `PeerTable` (already kept fresh by
//! `nodes::run_worker`) for peers whose DHT identity it hasn't dialed yet.
//!
//! Gated behind `AVALON_DHT_ENABLED` (default off) — unlike #362's
//! announce worker (always on, since serving the peer table is cheap and
//! has no attack surface beyond what already exists), running an actual
//! libp2p swarm is new, unproven-at-scale code with its own listen port and
//! dependency footprint; every existing deployment should see zero behavior
//! change until an operator opts in.

use std::collections::HashSet;
use std::time::Duration;

use libp2p::futures::StreamExt;
use libp2p::kad::{self, store::MemoryStore, Mode};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identify, identity, noise, tcp, yamux, Multiaddr, PeerId, Swarm, SwarmBuilder};

use crate::nodes::{PeerInfo, PeerTable};

/// Sent as part of libp2p's `identify` exchange — purely informational
/// (never version-gates anything the way `crate::version` does for the
/// HTTP peer table), but namespaced so this is never confused with some
/// other project's libp2p protocol on the wire.
const IDENTIFY_PROTOCOL_VERSION: &str = "/avalon/dht/1.0.0";

/// How often [`run_worker`] re-scans `PeerTable` for DHT identities it
/// hasn't dialed yet. `PeerTable` itself already refreshes on
/// `AVALON_ANNOUNCE_INTERVAL_SECS` (default 180s); scanning noticeably
/// faster than that just means a newly-announced peer's DHT identity is
/// picked up sooner without needing its own separate signal.
const BOOTSTRAP_SCAN_INTERVAL: Duration = Duration::from_secs(30);

/// How long [`start`] waits, at most, to observe this node's own listen
/// addresses before returning — binding `0.0.0.0` normally yields one
/// `NewListenAddr` event per local interface within milliseconds, so this
/// is a generous ceiling, not an expected wait.
const INITIAL_LISTEN_COLLECTION_WINDOW: Duration = Duration::from_secs(2);

#[derive(NetworkBehaviour)]
struct DhtBehaviour {
    kad: kad::Behaviour<MemoryStore>,
    identify: identify::Behaviour,
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityLoadError {
    #[error("AVALON_LIBP2P_IDENTITY_KEY is not valid hex: {0}")]
    InvalidHex(hex::FromHexError),
    #[error("AVALON_LIBP2P_IDENTITY_KEY must decode to exactly 32 bytes, got {0}")]
    WrongLength(usize),
}

/// Loads this node's libp2p identity keypair from
/// `AVALON_LIBP2P_IDENTITY_KEY` (a raw 32-byte Ed25519 seed, hex-encoded —
/// the same convention `avalon_chain::sth`'s `AVALON_SETTLEMENT_SIGNING_KEY`
/// already establishes), or generates a fresh one when unset.
///
/// Unlike the settlement key, an unset value is **not** an error: this is a
/// brand-new identity domain with no existing deployment depending on it,
/// so an ephemeral per-restart identity is a reasonable dev default — the
/// worst case is other nodes needing to re-learn this node's `PeerId` after
/// a restart, a reversible availability blip, never a security gate (the
/// same posture #368's protocol-version floor already takes for peer
/// admission). An operator who wants a stable `PeerId` sets the env var.
pub fn load_or_generate_identity_from_env() -> Result<identity::Keypair, IdentityLoadError> {
    match std::env::var("AVALON_LIBP2P_IDENTITY_KEY") {
        Ok(hex_value) => {
            let bytes = hex::decode(&hex_value).map_err(IdentityLoadError::InvalidHex)?;
            let len = bytes.len();
            let mut seed: [u8; 32] = bytes
                .try_into()
                .map_err(|_| IdentityLoadError::WrongLength(len))?;
            Ok(identity::Keypair::ed25519_from_bytes(&mut seed)
                .expect("a 32-byte seed is always a valid Ed25519 key"))
        }
        Err(_) => {
            tracing::warn!(
                "avalon-dht: AVALON_LIBP2P_IDENTITY_KEY unset — generating an ephemeral libp2p \
                 identity for this run; this node's PeerId will change on every restart. Set \
                 AVALON_LIBP2P_IDENTITY_KEY for a stable identity peers don't need to relearn."
            );
            Ok(identity::Keypair::generate_ed25519())
        }
    }
}

/// `AVALON_DHT_ENABLED`/`AVALON_LIBP2P_LISTEN_ADDR`/
/// `AVALON_LIBP2P_EXTERNAL_ADDR` resolved once at startup. `None` (via
/// [`from_env`](Self::from_env)) means #582/#580's DHT work doesn't run at
/// all — every pre-#582 deployment's behavior, unchanged.
pub struct DhtConfig {
    pub identity: identity::Keypair,
    pub listen_addr: Multiaddr,
    /// Issue #582, discovered live against the two-node LAN sandbox's
    /// actual Docker-deployed shape: a containerized node's own
    /// `NewListenAddr` events only ever report its container-internal
    /// bridge/loopback addresses, never its host's LAN-reachable one —
    /// the exact same problem `AVALON_NODE_URL` already exists to solve
    /// for the HTTP peer table (`crate::nodes::AnnounceConfig::own_base_url`
    /// is likewise never auto-detected). `Some` overrides
    /// [`start`]'s observed listen addresses entirely for announcing
    /// purposes — the local bind still happens on `listen_addr` as normal,
    /// this only changes what other peers are told to dial.
    pub external_addr: Option<Multiaddr>,
}

impl DhtConfig {
    /// `Ok(None)` when `AVALON_DHT_ENABLED` isn't truthy — the expected
    /// state for every deployment until #580 is ready to roll out.
    /// `AVALON_LIBP2P_LISTEN_ADDR` defaults to `/ip4/0.0.0.0/tcp/0` (an
    /// ephemeral port on every interface), matching this repo's existing
    /// "sane default, explicit override" convention (e.g.
    /// `AVALON_SERVER_ADDR`). `AVALON_LIBP2P_EXTERNAL_ADDR` is unset by
    /// default (native, non-containerized deployments don't need it — see
    /// `external_addr`'s own doc comment).
    pub fn from_env() -> Result<Option<Self>, String> {
        let enabled = std::env::var("AVALON_DHT_ENABLED")
            .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
            .unwrap_or(false);
        if !enabled {
            return Ok(None);
        }

        let identity = load_or_generate_identity_from_env().map_err(|e| e.to_string())?;
        let listen_addr = std::env::var("AVALON_LIBP2P_LISTEN_ADDR")
            .unwrap_or_else(|_| "/ip4/0.0.0.0/tcp/0".to_string())
            .parse::<Multiaddr>()
            .map_err(|e| format!("AVALON_LIBP2P_LISTEN_ADDR is not a valid multiaddr: {e}"))?;
        let external_addr = std::env::var("AVALON_LIBP2P_EXTERNAL_ADDR")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.parse::<Multiaddr>().map_err(|e| {
                    format!("AVALON_LIBP2P_EXTERNAL_ADDR is not a valid multiaddr: {e}")
                })
            })
            .transpose()?;

        Ok(Some(Self {
            identity,
            listen_addr,
            external_addr,
        }))
    }
}

/// This node's own DHT identity, as observed once at startup — handed to
/// `crate::nodes::run_worker` so it rides along on every outbound announce
/// (see this module's own doc comment), and to callers needing
/// `PeerId::to_string()`/dialable addresses for anything else later (#583).
pub struct DhtHandle {
    pub peer_id: PeerId,
    pub listen_addrs: Vec<Multiaddr>,
}

fn build_swarm(identity: identity::Keypair) -> Swarm<DhtBehaviour> {
    let peer_id = PeerId::from(identity.public());
    SwarmBuilder::with_existing_identity(identity)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .expect("TCP/noise/yamux transport construction is infallible for these fixed configs")
        .with_behaviour(|key| DhtBehaviour {
            kad: kad::Behaviour::new(peer_id, MemoryStore::new(peer_id)),
            identify: identify::Behaviour::new(identify::Config::new(
                IDENTIFY_PROTOCOL_VERSION.to_string(),
                key.public(),
            )),
        })
        .expect("behaviour construction from a fixed, valid config is infallible")
        .build()
}

/// Builds the DHT swarm, binds `config.listen_addr`, and spawns the
/// long-running worker that keeps it fed from `peers` (#362's peer table).
/// Returns as soon as this node's own listen addresses are known (bounded
/// by [`INITIAL_LISTEN_COLLECTION_WINDOW`]) so the caller can include them
/// in this node's own outbound announces from the very first one — see
/// `main.rs`'s call site.
pub async fn start(peers: PeerTable, config: DhtConfig) -> DhtHandle {
    let external_addr = config.external_addr.clone();
    let mut swarm = build_swarm(config.identity);
    let local_peer_id = *swarm.local_peer_id();
    swarm.behaviour_mut().kad.set_mode(Some(Mode::Server));
    swarm
        .listen_on(config.listen_addr.clone())
        .unwrap_or_else(|e| panic!("avalon-dht: failed to bind {}: {e}", config.listen_addr));

    let mut listen_addrs = Vec::new();
    let collection_deadline = tokio::time::sleep(INITIAL_LISTEN_COLLECTION_WINDOW);
    tokio::pin!(collection_deadline);
    loop {
        tokio::select! {
            _ = &mut collection_deadline => break,
            event = swarm.select_next_some() => {
                if let SwarmEvent::NewListenAddr { address, .. } = event {
                    tracing::info!(%address, peer_id = %local_peer_id, "avalon-dht: listening");
                    listen_addrs.push(address);
                }
            }
        }
    }

    // #582, discovered live against a real Docker-deployed node: an
    // observed listen address is only ever container-internal in that
    // shape — never what a LAN/WAN peer should actually dial. An operator
    // who sets AVALON_LIBP2P_EXTERNAL_ADDR is telling us so explicitly;
    // trust that over whatever `NewListenAddr` reported, the same way
    // `AVALON_NODE_URL` already overrides HTTP self-announcement.
    let announced_addrs = if let Some(external_addr) = external_addr {
        tracing::info!(%external_addr, "avalon-dht: announcing explicit external address instead of observed listen addresses");
        vec![external_addr]
    } else {
        if listen_addrs.is_empty() {
            tracing::warn!(
                "avalon-dht: no listen address observed within {:?} — this node's DHT identity \
                 will be announced without any dialable address until one appears",
                INITIAL_LISTEN_COLLECTION_WINDOW
            );
        }
        listen_addrs
    };

    tokio::spawn(run_worker(swarm, peers));

    DhtHandle {
        peer_id: local_peer_id,
        listen_addrs: announced_addrs,
    }
}

/// Parses `info`'s DHT identity (if any) into a dialable `(PeerId,
/// Vec<Multiaddr>)`, skipping peers already in `known` — pure and
/// unit-testable independent of a real swarm, same "pure function behind
/// the real worker" split `crate::nodes::resolve_bootstrap_peers` already
/// established. A peer with an unparseable `libp2p_peer_id`/address (never
/// expected from this node's own `nodes::announce_to`, but this table can
/// also be populated by a misbehaving or buggy remote peer) is skipped
/// rather than treated as fatal — the same "don't let one bad peer take
/// down peer processing" posture `admit_if_supported` already takes for a
/// version-floor mismatch.
fn new_dht_peer(info: &PeerInfo, known: &HashSet<PeerId>) -> Option<(PeerId, Vec<Multiaddr>)> {
    let peer_id: PeerId = info.libp2p_peer_id.as_ref()?.parse().ok()?;
    if known.contains(&peer_id) {
        return None;
    }
    let addrs: Vec<Multiaddr> = info
        .libp2p_listen_addrs
        .iter()
        .filter_map(|a| a.parse().ok())
        .collect();
    Some((peer_id, addrs))
}

/// Never returns. Handles `identify` responses (feeding a directly-dialed
/// peer's own reported listen addresses into `kad` — without this, a fresh
/// connection never actually populates the DHT routing table; #581's spike
/// hit exactly this) and, on [`BOOTSTRAP_SCAN_INTERVAL`], scans `peers` for
/// any DHT identity not yet dialed.
async fn run_worker(mut swarm: Swarm<DhtBehaviour>, peers: PeerTable) {
    let mut known_peers: HashSet<PeerId> = HashSet::new();
    let mut scan_interval = tokio::time::interval(BOOTSTRAP_SCAN_INTERVAL);
    // The first tick fires immediately; bootstrap from whatever #362
    // already knows about right away rather than waiting a full interval.

    loop {
        tokio::select! {
            _ = scan_interval.tick() => {
                for info in peers.list_all() {
                    if let Some((peer_id, addrs)) = new_dht_peer(&info, &known_peers) {
                        if addrs.is_empty() {
                            // Known to the network but not yet dialable — try
                            // again on a later scan once it reports a real
                            // address; not added to `known_peers` so this
                            // isn't a one-shot miss.
                            continue;
                        }
                        tracing::info!(%peer_id, base_url = %info.base_url, "avalon-dht: bootstrapping peer from peer table");
                        for addr in &addrs {
                            swarm.behaviour_mut().kad.add_address(&peer_id, addr.clone());
                        }
                        if let Some(addr) = addrs.into_iter().next() {
                            if let Err(e) = swarm.dial(addr) {
                                tracing::warn!(%peer_id, "avalon-dht: dial failed: {e}");
                            }
                        }
                        known_peers.insert(peer_id);
                    }
                }
            }
            event = swarm.select_next_some() => {
                if let SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } = event {
                    // Seed the routing table from the connection's own
                    // address immediately, same fix #581's spike needed —
                    // otherwise a peer dialed directly (not yet known to
                    // `kad`) can't be looked up until `identify` completes
                    // its own round trip.
                    swarm
                        .behaviour_mut()
                        .kad
                        .add_address(&peer_id, endpoint.get_remote_address().clone());
                    known_peers.insert(peer_id);
                } else if let SwarmEvent::OutgoingConnectionError { peer_id, error, .. } = event {
                    // Otherwise a dial that fails asynchronously (bad
                    // multiaddr, handshake mismatch, unreachable host)
                    // fails completely silently — the scan branch above
                    // only logs the *attempt*, never its outcome. Not
                    // removed from `known_peers`: a persistently
                    // unreachable peer just doesn't get retried until it
                    // re-announces (matching #362's own peer table's
                    // pruning-based recovery, not an independent retry
                    // policy here).
                    tracing::warn!(?peer_id, "avalon-dht: outgoing connection failed: {error}");
                } else if let SwarmEvent::Behaviour(DhtBehaviourEvent::Identify(
                    identify::Event::Received { peer_id, info, .. },
                )) = event
                {
                    for addr in info.listen_addrs {
                        swarm.behaviour_mut().kad.add_address(&peer_id, addr);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer_info(base_url: &str, peer_id: Option<&str>, addrs: Vec<&str>) -> PeerInfo {
        PeerInfo {
            base_url: base_url.to_string(),
            roles: vec!["combined".to_string()],
            protocol_version: "0.1.0".to_string(),
            network_id: "avalon-dev-local".to_string(),
            last_announced_at: time::OffsetDateTime::now_utc(),
            libp2p_peer_id: peer_id.map(|s| s.to_string()),
            libp2p_listen_addrs: addrs.into_iter().map(|s| s.to_string()).collect(),
        }
    }

    fn sample_peer_id() -> PeerId {
        identity::Keypair::generate_ed25519().public().into()
    }

    #[test]
    fn a_peer_with_no_libp2p_identity_is_not_a_dht_candidate() {
        let info = peer_info("http://a", None, vec![]);
        assert!(new_dht_peer(&info, &HashSet::new()).is_none());
    }

    #[test]
    fn a_known_peer_is_not_returned_again() {
        let peer_id = sample_peer_id();
        let info = peer_info(
            "http://a",
            Some(&peer_id.to_string()),
            vec!["/ip4/127.0.0.1/tcp/4001"],
        );
        let mut known = HashSet::new();
        known.insert(peer_id);
        assert!(new_dht_peer(&info, &known).is_none());
    }

    #[test]
    fn a_new_peer_with_a_valid_identity_and_address_is_returned() {
        let peer_id = sample_peer_id();
        let info = peer_info(
            "http://a",
            Some(&peer_id.to_string()),
            vec!["/ip4/127.0.0.1/tcp/4001"],
        );
        let (found_peer_id, addrs) = new_dht_peer(&info, &HashSet::new()).unwrap();
        assert_eq!(found_peer_id, peer_id);
        assert_eq!(addrs.len(), 1);
    }

    #[test]
    fn an_unparseable_peer_id_is_skipped_not_fatal() {
        let info = peer_info("http://a", Some("not-a-real-peer-id"), vec![]);
        assert!(new_dht_peer(&info, &HashSet::new()).is_none());
    }

    #[test]
    fn unparseable_addresses_are_dropped_but_dont_block_the_peer() {
        let peer_id = sample_peer_id();
        let info = peer_info(
            "http://a",
            Some(&peer_id.to_string()),
            vec!["not-a-multiaddr", "/ip4/127.0.0.1/tcp/4001"],
        );
        let (_, addrs) = new_dht_peer(&info, &HashSet::new()).unwrap();
        assert_eq!(addrs.len(), 1);
    }

    #[test]
    fn dht_config_from_env_is_none_when_not_enabled() {
        // SAFETY-of-intent note: process-global env var, same posture
        // `crate::nodes`'s own `node_roles_defaults_to_combined_when_unset`
        // test already takes.
        unsafe {
            std::env::remove_var("AVALON_DHT_ENABLED");
        }
        assert!(DhtConfig::from_env().unwrap().is_none());
    }
}
