//! Circuit relay v2 between real in-process libp2p swarms on loopback. No database or HTTP
//! server needed. A node listening only on IPv6 loopback is found `private` by AutoNAT (as in
//! `reachability.rs`) and so reserves a slot on the IPv4 relays.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use avalon_protocol::connectivity::Connectivity;
use avalon_server::dht::{self, DhtConfig, DhtHandle};
use avalon_server::nodes::{PeerInfo, PeerTable};
use avalon_server::reachability::{
    connectivity_for, AutonatSettings, Reachability, ReachabilitySnapshot,
};
use avalon_server::relay::{RelayClientSettings, RelayServerSettings, RelaySettings};
use libp2p::futures::StreamExt;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{
    identify, identity, noise, relay, tcp, yamux, Multiaddr, PeerId, Swarm, SwarmBuilder,
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

fn config(listen: &str, external: Option<&str>, relay: RelaySettings) -> DhtConfig {
    DhtConfig {
        identity: identity::Keypair::generate_ed25519(),
        network_id: NETWORK.to_string(),
        listen_addr: listen.parse().unwrap(),
        external_addr: external.map(|e| e.parse().unwrap()),
        autonat: autonat(),
        relay,
        bootstrap_scan_interval: Duration::from_millis(300),
    }
}

fn server_settings() -> RelayServerSettings {
    RelayServerSettings::default()
}

/// A dialable relay serving with `limits`, and its direct address ending in `/p2p/<id>`.
async fn relay_node(limits: RelayServerSettings) -> (DhtHandle, Multiaddr) {
    let addr = format!("/ip4/127.0.0.1/tcp/{}", free_port());
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
        base_url: format!("http://{}.test", node.peer_id),
        roles: vec!["combined".to_string()],
        protocol_version: avalon_server::version::PROTOCOL_VERSION.to_string(),
        network_id: NETWORK.to_string(),
        last_announced_at: time::OffsetDateTime::now_utc(),
        libp2p_peer_id: Some(node.peer_id.to_string()),
        libp2p_listen_addrs: node.listen_addrs.iter().map(|a| a.to_string()).collect(),
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

/// A client asked to hold two reservations holds two, on different relays.
#[tokio::test]
async fn a_client_holds_up_to_its_reservation_count() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let (r1, r1_addr) = relay_node(server_settings()).await;
    let (r2, r2_addr) = relay_node(server_settings()).await;
    let (_r3, r3_addr) = relay_node(server_settings()).await;
    let a = private_node(vec![r1_addr, r2_addr, r3_addr], 2, &r1).await;
    reserved_on(&a, r1.peer_id).await;
    reserved_on(&a, r2.peer_id).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(a.reachability.snapshot().relay_reservations.len(), 2);
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

/// The settings map onto the libp2p relay limits one to one.
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
    assert_eq!(cfg.max_reservations_per_peer, 2);
    assert_eq!(cfg.reservation_duration, Duration::from_secs(9));
    assert_eq!(cfg.max_circuits, 4);
    assert_eq!(cfg.max_circuits_per_peer, 1);
    assert_eq!(cfg.max_circuit_duration, Duration::from_secs(7));
    assert_eq!(cfg.max_circuit_bytes, 1234);
}
