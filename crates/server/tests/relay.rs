//! Circuit relay v2 between real in-process libp2p swarms on loopback. No database or HTTP
//! server needed. A node listening only on IPv6 loopback is found `private` by AutoNAT (as in
//! `reachability.rs`) and so reserves a slot on the IPv4 relays.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use avalon_protocol::connectivity::{Connectivity, PathType};
use avalon_server::dht::{self, DhtConfig, DhtHandle};
use avalon_server::nodes::{PeerInfo, PeerTable};
use avalon_server::reachability::{
    connectivity_for, AutonatSettings, Reachability, ReachabilitySnapshot,
};
use avalon_server::relay::{RelayClientSettings, RelayServerSettings, RelaySettings};
use libp2p::futures::StreamExt;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{
    dcutr, identify, identity, noise, relay, tcp, yamux, Multiaddr, PeerId, Swarm, SwarmBuilder,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const WAIT: Duration = Duration::from_secs(30);
const NETWORK: &str = "avalon-test";

fn autonat() -> AutonatSettings {
    AutonatSettings {
        allow_private: true,
        boot_delay: Duration::from_millis(100),
        retry_interval: Duration::from_millis(300),
        throttle_server_period: Duration::ZERO,
        ..AutonatSettings::default()
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn ipv6_available() -> bool {
    std::net::TcpListener::bind("[::1]:0").is_ok()
}

/// Whether 127.0.0.2, 127.0.1.1 and 127.0.2.1 are bindable; macOS has only 127.0.0.1 by default.
fn loopback_aliases_available() -> bool {
    ["127.0.0.2", "127.0.1.1", "127.0.2.1"]
        .iter()
        .all(|ip| std::net::TcpListener::bind((*ip, 0)).is_ok())
}

fn config(listen: &str, external: Option<&str>, relay: RelaySettings) -> DhtConfig {
    DhtConfig {
        identity: identity::Keypair::generate_ed25519(),
        network_id: NETWORK.to_string(),
        listen_addr: listen.parse().unwrap(),
        external_addr: external.map(|e| e.parse().unwrap()),
        autonat: autonat(),
        relay,
        relay_resilience: Default::default(),
        bootstrap_scan_interval: Duration::from_millis(300),
    }
}

fn server_settings() -> RelayServerSettings {
    RelayServerSettings::default()
}

/// A dialable relay serving with `limits`, and its direct address ending in `/p2p/<id>`.
async fn relay_node(limits: RelayServerSettings) -> (DhtHandle, Multiaddr) {
    relay_node_at("127.0.0.1", limits).await
}

/// [`relay_node`] on another loopback address, so relays can sit in different /24s.
async fn relay_node_at(ip: &str, limits: RelayServerSettings) -> (DhtHandle, Multiaddr) {
    let addr = format!("/ip4/{ip}/tcp/{}", free_port());
    let relay = RelaySettings {
        server: Some(limits),
        ..RelaySettings::default()
    };
    let node = dht::start(PeerTable::new(), config(&addr, Some(&addr), relay)).await;
    let full = format!("{addr}/p2p/{}", node.peer_id).parse().unwrap();
    (node, full)
}

fn client_settings(relays: Vec<Multiaddr>, max: usize) -> RelayClientSettings {
    RelayClientSettings {
        max_reservations: max,
        relay_addrs: relays,
        allow_private: true,
        retry_backoff: Duration::from_secs(60),
        pending_timeout: Duration::from_secs(10),
        reconcile_interval: Duration::from_millis(200),
        ..RelayClientSettings::default()
    }
}

fn introduce(into: &PeerTable, node: &DhtHandle) {
    into.upsert(PeerInfo {
        identity_bound: false,
        base_url: format!("http://{}.test", node.peer_id),
        roles: vec!["combined".to_string()],
        protocol_version: avalon_server::version::PROTOCOL_VERSION.to_string(),
        network_id: NETWORK.to_string(),
        last_announced_at: time::OffsetDateTime::now_utc(),
        libp2p_peer_id: Some(node.peer_id.to_string()),
        libp2p_listen_addrs: node.listen_addrs.iter().map(|a| a.to_string()).collect(),
        connectivity: None,
        witness: None,
    });
}

/// A `private` node (IPv6 loopback only) that AutoNAT probes through `probe_via` and that
/// reserves on `relays`.
async fn private_node(relays: Vec<Multiaddr>, max: usize, probe_via: &DhtHandle) -> DhtHandle {
    let peers = PeerTable::new();
    let relay = RelaySettings {
        client: client_settings(relays, max),
        ..RelaySettings::default()
    };
    let node = dht::start(peers.clone(), config("/ip6/::1/tcp/0", None, relay)).await;
    introduce(&peers, probe_via);
    node
}

async fn wait_snapshot<T>(
    node: &DhtHandle,
    what: &str,
    mut pick: impl FnMut(&ReachabilitySnapshot) -> Option<T>,
) -> T {
    let mut rx = node.reachability.subscribe();
    tokio::time::timeout(WAIT, async {
        loop {
            if let Some(found) = pick(&rx.borrow_and_update()) {
                return found;
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// The relayed address of the first reservation held on `relay`.
async fn reserved_on(node: &DhtHandle, relay: PeerId) -> Multiaddr {
    wait_snapshot(node, "a reservation", |s| {
        s.relay_reservations
            .iter()
            .find(|r| r.relay_peer_id == relay.to_string())
            .map(|r| r.relayed_addr.parse().unwrap())
    })
    .await
}

#[derive(NetworkBehaviour)]
struct Raw {
    relay_client: relay::client::Behaviour,
    identify: identify::Behaviour,
}

/// A node with no listener that can only reach others through relays.
fn raw_swarm() -> Swarm<Raw> {
    SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .unwrap()
        .with_relay_client(noise::Config::new, yamux::Config::default)
        .unwrap()
        .with_behaviour(|key, relay_client| Raw {
            relay_client,
            identify: identify::Behaviour::new(identify::Config::new(
                format!("/avalon/dht/1.0.0/{NETWORK}"),
                key.public(),
            )),
        })
        .unwrap()
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(120)))
        .build()
}

/// Dials `addr` and waits until `target` is connected over a relayed connection and has
/// identified itself, or the dial fails.
async fn dial_through_relay(
    swarm: &mut Swarm<Raw>,
    addr: Multiaddr,
    target: PeerId,
) -> Result<(), String> {
    swarm.dial(addr).map_err(|e| e.to_string())?;
    let (mut relayed, mut identified) = (false, false);
    tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            match swarm.select_next_some().await {
                SwarmEvent::ConnectionEstablished {
                    peer_id, endpoint, ..
                } if peer_id == target => relayed = endpoint.is_relayed(),
                SwarmEvent::Behaviour(RawEvent::Identify(identify::Event::Received {
                    peer_id,
                    ..
                })) if peer_id == target => identified = true,
                SwarmEvent::OutgoingConnectionError { error, .. } => return Err(error.to_string()),
                _ => {}
            }
            if relayed && identified {
                return Ok(());
            }
        }
    })
    .await
    .map_err(|_| "timed out".to_string())?
}

/// Reserves a slot on `relay` with a bare client and reports whether it was accepted.
async fn raw_reserve(swarm: &mut Swarm<Raw>, relay: &Multiaddr) -> bool {
    swarm
        .listen_on(relay.clone().with(libp2p::multiaddr::Protocol::P2pCircuit))
        .unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            match swarm.select_next_some().await {
                SwarmEvent::Behaviour(RawEvent::RelayClient(
                    relay::client::Event::ReservationReqAccepted { .. },
                )) => return true,
                SwarmEvent::ListenerClosed { .. } => return false,
                _ => {}
            }
        }
    })
    .await
    .expect("reservation neither accepted nor refused")
}

/// Whether a connection of `swarm` closes within `dur`.
async fn closes_within(swarm: &mut Swarm<Raw>, dur: Duration) -> bool {
    tokio::time::timeout(dur, async {
        loop {
            if let SwarmEvent::ConnectionClosed { .. } = swarm.select_next_some().await {
                return;
            }
        }
    })
    .await
    .is_ok()
}

async fn eventually(what: &str, mut cond: impl FnMut() -> bool) {
    tokio::time::timeout(WAIT, async {
        while !cond() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

/// A `private` node reserves a slot, reports `relayed`, advertises only the accepted relayed
/// address, and a node with no other path to it connects through the relay; the connection is
/// authenticated as the node itself, so the relay only carries bytes.
#[tokio::test]
async fn two_nodes_connect_only_through_a_relay() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let (r, r_addr) = relay_node(server_settings()).await;
    let a = private_node(vec![r_addr], 1, &r).await;

    assert_eq!(
        a.reachability.snapshot().relay_reservations.len(),
        0,
        "nothing is reserved or advertised before detection"
    );
    assert!(a.reachability.advertised_addrs().is_empty());

    let relayed = reserved_on(&a, r.peer_id).await;
    let snap = a.reachability.snapshot();
    assert_eq!(snap.reachability, Reachability::Private);
    assert_eq!(connectivity_for(&snap), Some(Connectivity::Relayed));
    assert!(relayed.to_string().contains("/p2p-circuit/p2p/"));
    assert_eq!(a.reachability.advertised_addrs(), vec![relayed.to_string()]);
    assert!(snap.confirmed_addrs.is_empty());

    let mut b = raw_swarm();
    dial_through_relay(&mut b, relayed, a.peer_id)
        .await
        .expect("B reaches A through the relay");
    assert!(b.is_connected(&a.peer_id));
    assert_eq!(r.relay_stats.counts().circuits_accepted, 1);
}

/// A relay reports its limits and what it is carrying; a node that does not relay reports none.
#[tokio::test]
async fn a_relay_reports_its_limits_and_usage_to_operators() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let (r, r_addr) = relay_node(RelayServerSettings {
        max_circuits: 3,
        ..server_settings()
    })
    .await;
    let status = r
        .reachability
        .relay_server_status()
        .expect("a relay reports itself");
    assert_eq!(status.limits.max_circuits, 3);
    assert_eq!(status.usage.reservations_active, 0);

    let a = private_node(vec![r_addr], 1, &r).await;
    reserved_on(&a, r.peer_id).await;
    eventually("the relay to count the reservation", || {
        r.reachability
            .relay_server_status()
            .is_some_and(|s| s.usage.reservations_active == 1)
    })
    .await;
    assert!(
        a.reachability.relay_server_status().is_none(),
        "a node that does not relay reports no relay section"
    );
}

#[derive(NetworkBehaviour)]
struct Punching {
    relay_client: relay::client::Behaviour,
    identify: identify::Behaviour,
    dcutr: dcutr::Behaviour,
}

fn punching_swarm() -> Swarm<Punching> {
    SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .unwrap()
        .with_relay_client(noise::Config::new, yamux::Config::default)
        .unwrap()
        .with_behaviour(|key, relay_client| Punching {
            relay_client,
            identify: identify::Behaviour::new(identify::Config::new(
                format!("/avalon/dht/1.0.0/{NETWORK}"),
                key.public(),
            )),
            dcutr: dcutr::Behaviour::new(key.public().to_peer_id()),
        })
        .unwrap()
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(120)))
        .build()
}

/// A peer B that has been seen by the relay at a loopback address it listens on, so DCUtR has
/// a candidate to offer. Returns B and the id of its listener.
async fn punching_peer(
    relay: &Multiaddr,
) -> (Swarm<Punching>, libp2p::core::transport::ListenerId) {
    let mut b = punching_swarm();
    let listener = b
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .unwrap();
    b.dial(relay.clone()).unwrap();
    let relay_id = relay
        .iter()
        .find_map(|p| match p {
            libp2p::multiaddr::Protocol::P2p(id) => Some(id),
            _ => None,
        })
        .unwrap();
    // Wait until the relay has identified B: only then does B's address become a candidate.
    tokio::time::timeout(WAIT, async {
        let (mut connected, mut identified) = (false, false);
        while !(connected && identified) {
            match b.select_next_some().await {
                SwarmEvent::ConnectionEstablished { peer_id, .. } if peer_id == relay_id => {
                    connected = true
                }
                SwarmEvent::Behaviour(PunchingEvent::Identify(identify::Event::Received {
                    peer_id,
                    ..
                })) if peer_id == relay_id => identified = true,
                _ => {}
            }
        }
    })
    .await
    .expect("B connects to the relay");
    // Give the relay's identify of B time to arrive.
    tokio::time::sleep(Duration::from_millis(500)).await;
    (b, listener)
}

/// Runs B's swarm until `node`'s snapshot yields something. The reserved node starts the punch,
/// so its snapshot is where the outcome shows.
async fn drive_until<T>(
    b: &mut Swarm<Punching>,
    node: &DhtHandle,
    what: &str,
    pick: impl FnMut(&ReachabilitySnapshot) -> Option<T>,
) -> T {
    let watch = wait_snapshot(node, what, pick);
    tokio::pin!(watch);
    loop {
        tokio::select! {
            found = &mut watch => return found,
            _ = b.select_next_some() => {}
        }
    }
}

/// A hole punch with no reachable address fails; the failure is recorded, the relayed
/// connection keeps carrying the peer, and the node still reports `relayed`.
#[tokio::test]
async fn a_failed_hole_punch_leaves_the_relayed_connection_in_use() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let (r, r_addr) = relay_node(server_settings()).await;
    let a = private_node(vec![r_addr.clone()], 1, &r).await;
    let relayed = reserved_on(&a, r.peer_id).await;
    let (mut b, listener) = punching_peer(&r_addr).await;
    // B's only candidate stops listening before the punch, so dialing it is refused.
    assert!(b.remove_listener(listener));

    b.dial(relayed).unwrap();
    let outcome = drive_until(&mut b, &a, "a recorded hole punch", |s| {
        s.hole_punches.first().cloned()
    })
    .await;
    assert!(!outcome.succeeded);
    assert!(outcome.error.is_some());
    assert_eq!(outcome.peer_id, b.local_peer_id().to_string());
    let snap = a.reachability.snapshot();
    assert!(snap.punched_peers.is_empty());
    assert_eq!(connectivity_for(&snap), Some(Connectivity::Relayed));
    assert!(
        b.is_connected(&a.peer_id),
        "the relayed connection stays up"
    );
}

/// AutoNAT probes only over a connection that is still open, and nothing redials a dropped
/// one, so an idle connection has to outlive libp2p's 10s default.
#[tokio::test]
async fn an_idle_connection_outlives_the_default_idle_window() {
    let addr = format!("/ip4/127.0.0.1/tcp/{}", free_port());
    let mut quiet = config(&addr, None, RelaySettings::default());
    quiet.autonat.enabled = false;
    let node = dht::start(PeerTable::new(), quiet).await;

    let mut peer = raw_swarm();
    peer.dial(
        format!("{addr}/p2p/{}", node.peer_id)
            .parse::<Multiaddr>()
            .unwrap(),
    )
    .unwrap();
    tokio::time::timeout(WAIT, async {
        while !matches!(
            peer.select_next_some().await,
            SwarmEvent::ConnectionEstablished { .. }
        ) {}
    })
    .await
    .expect("connected");

    assert!(
        !closes_within(&mut peer, Duration::from_secs(13)).await,
        "the node closed an idle connection"
    );
}

/// With a 5s reservation lifetime the client renews on its own before each expiry and stays
/// reachable past several lifetimes. (libp2p schedules renewals in whole seconds, so very short
/// lifetimes renew too late.)
#[tokio::test]
async fn reservations_renew_before_they_expire() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let (r, r_addr) = relay_node(RelayServerSettings {
        reservation_duration: Duration::from_secs(5),
        ..server_settings()
    })
    .await;
    let a = private_node(vec![r_addr], 1, &r).await;
    let relayed = reserved_on(&a, r.peer_id).await;

    wait_snapshot(&a, "two renewals", |s| {
        s.relay_reservations
            .iter()
            .any(|r| r.renewals >= 2)
            .then_some(())
    })
    .await;
    assert_eq!(r.relay_stats.counts().reservations_timed_out, 0);

    let mut b = raw_swarm();
    dial_through_relay(&mut b, relayed, a.peer_id)
        .await
        .expect("still reachable after renewals");
}

/// Forwards TCP from a local port to `target` until the returned flag is set, then holds all
/// bytes back without closing anything: a client behind it can no longer renew.
async fn pausable_proxy(target: SocketAddr) -> (u16, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let paused = Arc::new(AtomicBool::new(false));
    let flag = paused.clone();
    tokio::spawn(async move {
        loop {
            let (client, _) = listener.accept().await.unwrap();
            let server = TcpStream::connect(target).await.unwrap();
            let (cr, cw) = client.into_split();
            let (sr, sw) = server.into_split();
            tokio::spawn(forward(cr, sw, flag.clone()));
            tokio::spawn(forward(sr, cw, flag.clone()));
        }
    });
    (port, paused)
}

async fn forward(
    mut from: impl AsyncReadExt + Unpin,
    mut to: impl AsyncWriteExt + Unpin,
    paused: Arc<AtomicBool>,
) {
    let mut buf = [0u8; 4096];
    while let Ok(n) = from.read(&mut buf).await {
        if n == 0 {
            break;
        }
        while paused.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if to.write_all(&buf[..n]).await.is_err() {
            break;
        }
    }
}

/// A client that stops renewing loses its reservation when it expires: the relay times it out
/// and refuses circuits to that client.
#[tokio::test]
async fn an_unrenewed_reservation_expires() {
    let (r, r_addr) = relay_node(RelayServerSettings {
        reservation_duration: Duration::from_secs(4),
        ..server_settings()
    })
    .await;
    let relay_sock: SocketAddr = format!("127.0.0.1:{}", tcp_port(&r_addr)).parse().unwrap();
    let (proxy_port, paused) = pausable_proxy(relay_sock).await;
    let via_proxy: Multiaddr = format!("/ip4/127.0.0.1/tcp/{proxy_port}/p2p/{}", r.peer_id)
        .parse()
        .unwrap();

    let mut silent = raw_swarm();
    assert!(raw_reserve(&mut silent, &via_proxy).await);
    let silent_id = *silent.local_peer_id();
    assert_eq!(r.relay_stats.counts().reservations_timed_out, 0);

    paused.store(true, Ordering::Relaxed);
    eventually("the reservation to time out", || {
        r.relay_stats.counts().reservations_timed_out >= 1
    })
    .await;

    let mut b = raw_swarm();
    let target = format!("{r_addr}/p2p-circuit/p2p/{silent_id}")
        .parse()
        .unwrap();
    assert!(dial_through_relay(&mut b, target, silent_id).await.is_err());
    assert_eq!(r.relay_stats.counts().circuits_accepted, 0);
    assert!(r.relay_stats.counts().circuits_denied >= 1);
}

fn tcp_port(addr: &Multiaddr) -> u16 {
    addr.iter()
        .find_map(|p| match p {
            libp2p::multiaddr::Protocol::Tcp(port) => Some(port),
            _ => None,
        })
        .unwrap()
}

/// Losing the relay moves the reservation to the next relay without a restart, and the node is
/// reachable again through it.
#[tokio::test]
async fn losing_a_relay_fails_over_to_another() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let (r1, r1_addr) = relay_node(server_settings()).await;
    let (r2, r2_addr) = relay_node(server_settings()).await;
    let a = private_node(vec![r1_addr, r2_addr], 1, &r1).await;
    let a_id = a.peer_id;

    let first = reserved_on(&a, r1.peer_id).await;
    let mut b = raw_swarm();
    dial_through_relay(&mut b, first, a_id)
        .await
        .expect("reachable through the first relay");

    let r2_id = r2.peer_id;
    drop(r1);
    let second = reserved_on(&a, r2_id).await;
    assert_eq!(a.peer_id, a_id);
    assert_eq!(a.reachability.snapshot().relay_reservations.len(), 1);
    assert_eq!(
        connectivity_for(&a.reachability.snapshot()),
        Some(Connectivity::Relayed)
    );

    let mut b2 = raw_swarm();
    dial_through_relay(&mut b2, second, a_id)
        .await
        .expect("reachable through the second relay");
    drop(r2);
}

/// Up to `max_reservations` are held; a relay with more clients than slots refuses the extra.
#[tokio::test]
async fn reservations_beyond_the_limit_are_refused() {
    let (r, r_addr) = relay_node(RelayServerSettings {
        max_reservations: 1,
        ..server_settings()
    })
    .await;
    let mut first = raw_swarm();
    let mut second = raw_swarm();
    assert!(raw_reserve(&mut first, &r_addr).await);
    assert!(!raw_reserve(&mut second, &r_addr).await);
    let counts = r.relay_stats.counts();
    assert_eq!(counts.reservations_accepted, 1);
    assert!(counts.reservations_denied >= 1);
}

/// Relay peer ids the node currently holds reservations on.
fn held(node: &DhtHandle) -> Vec<String> {
    node.reachability
        .snapshot()
        .relay_reservations
        .into_iter()
        .map(|r| r.relay_peer_id)
        .collect()
}

/// A client asked to hold two reservations holds two, on different relays in different /24s.
#[tokio::test]
async fn a_client_holds_up_to_its_reservation_count() {
    if !ipv6_available() || !loopback_aliases_available() {
        return eprintln!("skipping: no IPv6 loopback or loopback aliases");
    }
    let (r1, r1_addr) = relay_node_at("127.0.0.1", server_settings()).await;
    let (r2, r2_addr) = relay_node_at("127.0.1.1", server_settings()).await;
    let (_r3, r3_addr) = relay_node_at("127.0.2.1", server_settings()).await;
    let a = private_node(vec![r1_addr, r2_addr, r3_addr], 2, &r1).await;
    reserved_on(&a, r1.peer_id).await;
    reserved_on(&a, r2.peer_id).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(held(&a).len(), 2);
}

/// A relay in another /24 is used for the second reservation even when a same-/24 relay is
/// listed before it.
#[tokio::test]
async fn the_second_reservation_avoids_the_first_relays_24() {
    if !ipv6_available() || !loopback_aliases_available() {
        return eprintln!("skipping: no IPv6 loopback or loopback aliases");
    }
    let (r1, r1_addr) = relay_node_at("127.0.0.1", server_settings()).await;
    let (twin, twin_addr) = relay_node_at("127.0.0.2", server_settings()).await;
    let (r3, r3_addr) = relay_node_at("127.0.1.1", server_settings()).await;
    let a = private_node(vec![r1_addr, twin_addr, r3_addr], 2, &r1).await;
    reserved_on(&a, r1.peer_id).await;
    reserved_on(&a, r3.peer_id).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let held = held(&a);
    assert_eq!(held.len(), 2, "{held:?}");
    assert!(!held.contains(&twin.peer_id.to_string()), "{held:?}");
}

/// With every relay in one /24 the node still reaches its reservation count.
#[tokio::test]
async fn one_prefix_still_fills_every_reservation() {
    if !ipv6_available() || !loopback_aliases_available() {
        return eprintln!("skipping: no IPv6 loopback or loopback aliases");
    }
    let (r1, r1_addr) = relay_node_at("127.0.0.1", server_settings()).await;
    let (r2, r2_addr) = relay_node_at("127.0.0.2", server_settings()).await;
    let a = private_node(vec![r1_addr, r2_addr], 2, &r1).await;
    reserved_on(&a, r1.peer_id).await;
    reserved_on(&a, r2.peer_id).await;
    assert_eq!(held(&a).len(), 2);
}

/// A relay that is listed up front but only starts serving later.
struct LateRelay {
    key: identity::Keypair,
    addr: String,
    listed: Multiaddr,
}

impl LateRelay {
    fn at(ip: &str) -> Self {
        let key = identity::Keypair::generate_ed25519();
        let peer = key.public().to_peer_id();
        let addr = format!("/ip4/{ip}/tcp/{}", free_port());
        let listed = format!("{addr}/p2p/{peer}").parse().unwrap();
        Self { key, addr, listed }
    }

    async fn start(&self) -> DhtHandle {
        let relay = RelaySettings {
            server: Some(server_settings()),
            ..RelaySettings::default()
        };
        let mut cfg = config(&self.addr, Some(&self.addr), relay);
        cfg.identity = self.key.clone();
        dht::start(PeerTable::new(), cfg).await
    }
}

const HOLD: Duration = Duration::from_secs(8);

/// Short hold and interval so a move can be seen; the margin rules have unit tests.
fn reselecting(relays: Vec<Multiaddr>, max: usize) -> RelayClientSettings {
    RelayClientSettings {
        retry_backoff: Duration::from_secs(1),
        reselect_hold: HOLD,
        reselect_interval: Duration::from_secs(1),
        ..client_settings(relays, max)
    }
}

async fn private_node_with(client: RelayClientSettings, probe_via: &DhtHandle) -> DhtHandle {
    let peers = PeerTable::new();
    let relay = RelaySettings {
        client,
        ..RelaySettings::default()
    };
    let node = dht::start(peers.clone(), config("/ip6/::1/tcp/0", None, relay)).await;
    introduce(&peers, probe_via);
    node
}

/// Records whether the node ever advertised no reservation after it first had one.
fn watch_for_gaps(node: &DhtHandle) -> Arc<AtomicBool> {
    let gap = Arc::new(AtomicBool::new(false));
    let seen = gap.clone();
    let mut rx = node.reachability.subscribe();
    tokio::spawn(async move {
        let mut had = false;
        loop {
            let empty = rx.borrow_and_update().relay_reservations.is_empty();
            if !empty {
                had = true;
            } else if had {
                seen.store(true, Ordering::SeqCst);
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    });
    gap
}

/// Whether the announce loop was woken by a change of advertised relayed addresses.
async fn announce_woken(node: &DhtHandle) -> bool {
    tokio::time::timeout(
        Duration::from_secs(1),
        node.reachability.relayed_addrs_changed(),
    )
    .await
    .is_ok()
}

/// A listed relay that comes up after the node reserved on a discovered one takes its place, but
/// only once the hold time is over; the node is never without a reservation and the change wakes
/// the announce loop.
#[tokio::test]
async fn a_listed_relay_that_appears_later_replaces_a_discovered_one_after_the_hold() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let (found, _) = relay_node(server_settings()).await;
    let listed = LateRelay::at("127.0.0.1");
    let a = private_node_with(reselecting(vec![listed.listed.clone()], 1), &found).await;
    let gaps = watch_for_gaps(&a);

    reserved_on(&a, found.peer_id).await;
    let reserved_at = std::time::Instant::now();
    assert!(
        announce_woken(&a).await,
        "the first reservation wakes the announce loop"
    );

    let late = listed.start().await;
    tokio::time::sleep(HOLD / 2).await;
    assert_eq!(
        held(&a),
        vec![found.peer_id.to_string()],
        "still inside the hold time"
    );

    let moved = reserved_on(&a, late.peer_id).await;
    assert!(
        reserved_at.elapsed() >= HOLD - Duration::from_secs(1),
        "moved after {:?}",
        reserved_at.elapsed()
    );
    assert_eq!(
        held(&a),
        vec![late.peer_id.to_string()],
        "the old reservation is released"
    );
    assert_eq!(a.reachability.advertised_addrs(), vec![moved.to_string()]);
    assert!(announce_woken(&a).await, "the move wakes the announce loop");
    assert!(!gaps.load(Ordering::SeqCst), "never without a reservation");
    let mut b = raw_swarm();
    dial_through_relay(&mut b, moved, a.peer_id)
        .await
        .expect("reachable through the new relay");
}

/// Two reservations on one /24 are rebalanced onto a relay in another /24 once it is up and the
/// hold time is over.
#[tokio::test]
async fn a_same_prefix_fallback_moves_to_a_diverse_relay_that_appears_later() {
    if !ipv6_available() || !loopback_aliases_available() {
        return eprintln!("skipping: no IPv6 loopback or loopback aliases");
    }
    let (r1, r1_addr) = relay_node_at("127.0.0.1", server_settings()).await;
    let (r2, r2_addr) = relay_node_at("127.0.0.2", server_settings()).await;
    let diverse = LateRelay::at("127.0.1.1");
    let a = private_node_with(
        reselecting(vec![r1_addr, r2_addr, diverse.listed.clone()], 2),
        &r1,
    )
    .await;
    let gaps = watch_for_gaps(&a);
    reserved_on(&a, r1.peer_id).await;
    reserved_on(&a, r2.peer_id).await;
    let reserved_at = std::time::Instant::now();

    let late = diverse.start().await;
    tokio::time::sleep(HOLD / 2).await;
    assert_eq!(held(&a).len(), 2);
    assert!(
        held(&a).contains(&r2.peer_id.to_string()),
        "inside the hold time"
    );

    reserved_on(&a, late.peer_id).await;
    assert!(reserved_at.elapsed() >= HOLD - Duration::from_secs(1));
    eventually("the same-prefix fallback to be released", || {
        let now = held(&a);
        now.len() == 2 && !now.contains(&r2.peer_id.to_string())
    })
    .await;
    assert!(
        held(&a).contains(&r1.peer_id.to_string()),
        "the better-ranked relay stays"
    );
    assert!(!gaps.load(Ordering::SeqCst));
}

/// With `max_circuits` of 1 a second concurrent circuit is refused.
#[tokio::test]
async fn circuits_beyond_the_limit_are_refused() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let (r, r_addr) = relay_node(RelayServerSettings {
        max_circuits: 1,
        max_circuits_per_peer: 1,
        ..server_settings()
    })
    .await;
    let a = private_node(vec![r_addr], 1, &r).await;
    let relayed = reserved_on(&a, r.peer_id).await;

    let mut b1 = raw_swarm();
    dial_through_relay(&mut b1, relayed.clone(), a.peer_id)
        .await
        .expect("first circuit");
    let mut b2 = raw_swarm();
    assert!(dial_through_relay(&mut b2, relayed, a.peer_id)
        .await
        .is_err());
    assert!(r.relay_stats.counts().circuits_denied >= 1);
}

/// A circuit is cut when it outlives `max_circuit_duration`.
#[tokio::test]
async fn circuits_are_cut_at_the_duration_limit() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let (r, r_addr) = relay_node(RelayServerSettings {
        max_circuit_duration: Duration::from_secs(2),
        ..server_settings()
    })
    .await;
    let a = private_node(vec![r_addr], 1, &r).await;
    let relayed = reserved_on(&a, r.peer_id).await;

    let mut b = raw_swarm();
    dial_through_relay(&mut b, relayed, a.peer_id)
        .await
        .unwrap();
    assert!(
        closes_within(&mut b, Duration::from_secs(8)).await,
        "circuit outlived its limit"
    );
    eventually("the relay to record the cut", || {
        r.relay_stats.counts().circuits_closed_with_error >= 1
    })
    .await;
}

/// A circuit is cut when it carries more than `max_circuit_bytes`.
#[tokio::test]
async fn circuits_are_cut_at_the_byte_limit() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let (r, r_addr) = relay_node(RelayServerSettings {
        max_circuit_bytes: 400,
        ..server_settings()
    })
    .await;
    let a = private_node(vec![r_addr], 1, &r).await;
    let relayed = reserved_on(&a, r.peer_id).await;

    let mut b = raw_swarm();
    let outcome = dial_through_relay(&mut b, relayed, a.peer_id).await;
    assert!(outcome.is_err(), "identify completed through a cut circuit");
    eventually("the relay to record the cut", || {
        r.relay_stats.counts().circuits_closed_with_error >= 1
    })
    .await;
}

/// With no operator-listed relay, a connected peer that advertises the relay hop protocol is
/// found through identify and used; a peer that does not serve relaying is never chosen.
#[tokio::test]
async fn relays_are_discovered_through_identify() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let (r, _) = relay_node(server_settings()).await;
    let a = private_node(Vec::new(), 1, &r).await;
    reserved_on(&a, r.peer_id).await;

    let plain = dht::start(
        PeerTable::new(),
        config("/ip4/127.0.0.1/tcp/0", None, RelaySettings::default()),
    )
    .await;
    let lonely = private_node(Vec::new(), 1, &plain).await;
    wait_snapshot(&lonely, "private", |s| {
        (s.reachability == Reachability::Private).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(lonely.reachability.snapshot().relay_reservations.is_empty());
}

/// The protocols `target` reports to a bare node that dials it directly.
async fn protocols_of(target: &Multiaddr, id: PeerId) -> Vec<libp2p::StreamProtocol> {
    let mut b = raw_swarm();
    b.dial(target.clone()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let SwarmEvent::Behaviour(RawEvent::Identify(identify::Event::Received {
                peer_id,
                info,
                ..
            })) = b.select_next_some().await
            {
                if peer_id == id {
                    return info.protocols;
                }
            }
        }
    })
    .await
    .expect("identify from target")
}

/// A node advertises the relay hop protocol only while it is dialable: a relay does, a node
/// found `private` that also enables serving does not.
#[tokio::test]
async fn only_a_dialable_node_advertises_relaying() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let (r, r_addr) = relay_node(server_settings()).await;
    assert!(protocols_of(&r_addr, r.peer_id)
        .await
        .contains(&relay::HOP_PROTOCOL_NAME));

    let peers = PeerTable::new();
    let cfg = config(
        "/ip6/::1/tcp/0",
        None,
        RelaySettings {
            server: Some(server_settings()),
            client: client_settings(Vec::new(), 1),
            ..RelaySettings::default()
        },
    );
    let p = dht::start(peers.clone(), cfg).await;
    introduce(&peers, &r);
    wait_snapshot(&p, "private", |s| {
        (s.reachability == Reachability::Private).then_some(())
    })
    .await;
    let direct: Multiaddr = format!("{}/p2p/{}", p.listen_addrs[0], p.peer_id)
        .parse()
        .unwrap();
    assert!(!protocols_of(&direct, p.peer_id)
        .await
        .contains(&relay::HOP_PROTOCOL_NAME));
}

/// Without a detection result the client reserves nothing, however many relays it knows.
#[tokio::test]
async fn an_undetected_node_reserves_nothing() {
    let (r, r_addr) = relay_node(server_settings()).await;
    let peers = PeerTable::new();
    let mut cfg = config(
        "/ip4/127.0.0.1/tcp/0",
        None,
        RelaySettings {
            client: client_settings(vec![r_addr], 1),
            ..RelaySettings::default()
        },
    );
    cfg.autonat.enabled = false;
    let node = dht::start(peers.clone(), cfg).await;
    introduce(&peers, &r);

    tokio::time::sleep(Duration::from_secs(3)).await;
    let snap = node.reachability.snapshot();
    assert_eq!(snap.reachability, Reachability::Unknown);
    assert!(snap.relay_reservations.is_empty());
    assert_eq!(r.relay_stats.counts().reservations_accepted, 0);
}

/// A node peers can dial directly does not take a relay slot.
#[tokio::test]
async fn a_public_node_reserves_nothing() {
    let (r, r_addr) = relay_node(server_settings()).await;
    let peers = PeerTable::new();
    let relay = RelaySettings {
        client: client_settings(vec![r_addr], 1),
        ..RelaySettings::default()
    };
    let node = dht::start(peers.clone(), config("/ip4/127.0.0.1/tcp/0", None, relay)).await;
    introduce(&peers, &r);

    wait_snapshot(&node, "public", |s| {
        (s.reachability == Reachability::Public).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(node.reachability.snapshot().relay_reservations.is_empty());
    assert_eq!(r.relay_stats.counts().reservations_accepted, 0);
}

/// The settings map onto the libp2p relay limits; the per-peer ones are one less because the
/// library denies only above them.
#[test]
fn server_settings_map_to_relay_limits() {
    let cfg = RelayServerSettings {
        max_reservations: 3,
        max_reservations_per_peer: 2,
        reservation_duration: Duration::from_secs(9),
        max_circuits: 4,
        max_circuits_per_peer: 1,
        max_circuit_duration: Duration::from_secs(7),
        max_circuit_bytes: 1234,
    }
    .libp2p_config();
    assert_eq!(cfg.max_reservations, 3);
    assert_eq!(cfg.max_reservations_per_peer, 1);
    assert_eq!(cfg.reservation_duration, Duration::from_secs(9));
    assert_eq!(cfg.max_circuits, 4);
    assert_eq!(cfg.max_circuits_per_peer, 0);
    assert_eq!(cfg.max_circuit_duration, Duration::from_secs(7));
    assert_eq!(cfg.max_circuit_bytes, 1234);
}

/// A table entry for `id` reachable only through the relay at `relay_addr`.
fn relayed_entry(id: PeerId, relay_addr: &Multiaddr) -> PeerInfo {
    PeerInfo {
        identity_bound: false,
        base_url: format!("http://{id}.test"),
        roles: vec!["combined".to_string()],
        protocol_version: avalon_server::version::PROTOCOL_VERSION.to_string(),
        network_id: NETWORK.to_string(),
        last_announced_at: time::OffsetDateTime::now_utc(),
        libp2p_peer_id: Some(id.to_string()),
        libp2p_listen_addrs: vec![format!("{relay_addr}/p2p-circuit/p2p/{id}")],
        connectivity: None,
        witness: None,
    }
}

/// Node A is told of a node only reachable through a relay before that node holds a
/// reservation, so its first circuit dial fails. Returns whether A ends up connected to it over
/// the relay once the reservation exists, with `retries` retries allowed.
async fn dials_a_relayed_peer_that_reserves_late(
    retries: u32,
    window: Duration,
) -> Option<Duration> {
    let (_r, r_addr) = relay_node(server_settings()).await;
    // A bare target dials no one, so only A's own dials can connect them. The lower peer id
    // dials first, so A needs no crossed-dial grace.
    let mut target = raw_swarm();
    let t_id = *target.local_peer_id();
    let a_key = loop {
        let key = identity::Keypair::generate_ed25519();
        if key.public().to_peer_id().to_bytes() < t_id.to_bytes() {
            break key;
        }
    };
    let a_peers = PeerTable::new();
    a_peers.upsert(relayed_entry(t_id, &r_addr));
    let mut a_config = config("/ip4/127.0.0.1/tcp/0", None, RelaySettings::default());
    a_config.identity = a_key;
    a_config.relay_resilience = avalon_server::relay_resilience::RelayResilience {
        dial_retries: retries,
        dial_retry_base: Duration::from_secs(1),
        ..Default::default()
    };
    let _a = dht::start(a_peers.clone(), a_config).await;

    // Let the first dial fail: the relay has no reservation for the target yet.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(raw_reserve(&mut target, &r_addr).await);
    tokio::spawn(async move {
        loop {
            target.select_next_some().await;
        }
    });

    let started = std::time::Instant::now();
    tokio::time::timeout(window, async {
        while a_peers.paths().path(&t_id) != Some(PathType::Relayed) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .ok()
    .map(|_| started.elapsed())
}

/// A circuit dial that failed because the target had no reservation yet is retried and connects
/// once it has; with retries off it is never dialed again within twice the time the retry took.
#[tokio::test]
async fn a_failed_relayed_dial_is_retried_until_the_peer_is_reachable() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let took = dials_a_relayed_peer_that_reserves_late(5, Duration::from_secs(30))
        .await
        .expect("the retried dial connects");
    assert!(
        dials_a_relayed_peer_that_reserves_late(0, took * 2 + Duration::from_secs(2))
            .await
            .is_none(),
        "without retries the failed dial is not repeated"
    );
}
/// A serving relay whose one AutoNAT probe fails: its only prober allows a single connection
/// per peer, so the dial-back is denied and the relay turns `private`. Returns whether it still
/// advertises the hop protocol then, with `grace` as the private grace.
async fn relay_after_one_failed_probe_advertises_hop(grace: Duration) -> bool {
    let addr = format!("/ip4/127.0.0.1/tcp/{}", free_port());
    let r_peers = PeerTable::new();
    let mut r_config = config(
        &addr,
        Some(&addr),
        RelaySettings {
            server: Some(server_settings()),
            ..RelaySettings::default()
        },
    );
    r_config.relay_resilience = avalon_server::relay_resilience::RelayResilience {
        private_grace: grace,
        ..Default::default()
    };
    let r = dht::start(r_peers.clone(), r_config).await;
    let r_addr: Multiaddr = format!("{addr}/p2p/{}", r.peer_id).parse().unwrap();

    let prober = dht::start_with_node_http(
        PeerTable::new(),
        config("/ip4/127.0.0.1/tcp/0", None, RelaySettings::default()),
        avalon_server::node_http::NodeHttpSettings {
            max_connections_per_peer: 1,
            ..Default::default()
        },
    )
    .await;
    introduce(&r_peers, &prober);
    wait_snapshot(&r, "private", |s| {
        (s.reachability == Reachability::Private).then_some(())
    })
    .await;
    // Lets the verdict reach the protocols the relay advertises.
    tokio::time::sleep(Duration::from_secs(1)).await;
    protocols_of(&r_addr, r.peer_id)
        .await
        .contains(&relay::HOP_PROTOCOL_NAME)
}

/// One failed probe does not take a serving relay out of service; with no grace it does.
#[tokio::test]
async fn a_relay_keeps_serving_through_a_brief_private_verdict() {
    assert!(
        relay_after_one_failed_probe_advertises_hop(Duration::from_secs(60)).await,
        "the relay stops serving on one failed probe"
    );
    assert!(
        !relay_after_one_failed_probe_advertises_hop(Duration::ZERO).await,
        "without a grace the private verdict stops the relay"
    );
}

/// Every AutoNAT dial-back opens a connection to the probed node; the prober allows two
/// connections per peer, so unless each is closed once answered the second probe is denied and
/// the node is told `private`. The probes must also have run, and the prober's connection count
/// to the node must stay within its limit throughout.
#[tokio::test]
async fn repeated_probes_do_not_exhaust_the_probers_connection_limit() {
    let addr = format!("/ip4/127.0.0.1/tcp/{}", free_port());
    let r_peers = PeerTable::new();
    let r = dht::start(
        r_peers.clone(),
        config(&addr, Some(&addr), RelaySettings::default()),
    )
    .await;
    let p_peers = PeerTable::new();
    let prober = dht::start_with_node_http(
        p_peers.clone(),
        config("/ip4/127.0.0.1/tcp/0", None, RelaySettings::default()),
        avalon_server::node_http::NodeHttpSettings {
            max_connections_per_peer: 2,
            ..Default::default()
        },
    )
    .await;
    introduce(&r_peers, &prober);
    wait_snapshot(&r, "a successful probe", |s| {
        (s.reachability == Reachability::Public && !s.confirmed_addrs.is_empty()).then_some(())
    })
    .await;
    let mut most = 0;
    for _ in 0..50 {
        let snap = r.reachability.snapshot();
        assert_eq!(snap.reachability, Reachability::Public);
        most = most.max(p_peers.paths().connections(&r.peer_id));
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        (1..=2).contains(&most),
        "the prober held {most} connections"
    );
    drop(prober);
}
