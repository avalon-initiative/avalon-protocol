//! Hostile peers against relays and relayed connections, on real in-process swarms over
//! loopback: impersonation through a circuit, reservation and circuit exhaustion by many free
//! keypairs, a flood of fake relay candidates, and malformed frames on the relay protocols.
//! No database needed.
//!
//! Tests named `libp2p_*` pin behaviour of the libp2p relay codec that Avalon relies on but
//! does not implement; the others exercise Avalon's own limits, binding rules and bounds.

mod abuse_support;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use abuse_support::*;
use avalon_server::dht::{self, DhtHandle};
use avalon_server::node_http::{p2p_base_url, NodeClient, RemotePeer, SHARED_PEER_ADDR};
use avalon_server::nodes::PeerTable;
use avalon_server::outbound_policy::OutboundPolicy;
use avalon_server::peer_admission::sanitize_libp2p_addrs;
use avalon_server::reachability::{Reachability, ReachabilityHandle};
use avalon_server::relay::{RelayClient, RelayClientSettings, RelayServerSettings, RelaySettings};
use axum::extract::ConnectInfo;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use libp2p::futures::StreamExt;
use libp2p::swarm::dial_opts::DialOpts;
use libp2p::swarm::SwarmEvent;
use libp2p::{identity, noise, relay, tcp, yamux, Multiaddr, PeerId, Swarm, SwarmBuilder};

fn tight_limits() -> RelayServerSettings {
    RelayServerSettings {
        max_reservations: 3,
        max_reservations_per_peer: 1,
        max_circuits: 3,
        max_circuits_per_peer: 1,
        ..RelayServerSettings::default()
    }
}

fn random_peer() -> PeerId {
    identity::Keypair::generate_ed25519().public().to_peer_id()
}

/// A private node (IPv6 loopback only) reserved on `relay` that serves `router` over the node
/// stream protocol; `peers` is its peer table.
async fn private_server(
    relay_addr: &Multiaddr,
    relay: &DhtHandle,
    router: Router,
) -> (DhtHandle, PeerTable, Multiaddr) {
    let peers = PeerTable::new();
    let node = dht::start(
        peers.clone(),
        config(
            "/ip6/::1/tcp/0",
            None,
            RelaySettings {
                client: client_settings(vec![relay_addr.clone()], 1),
                ..RelaySettings::default()
            },
        ),
    )
    .await;
    introduce(&peers, relay);
    node.router_slot.set(router);
    let relayed = reserved_on(&node, relay.peer_id).await;
    (node, peers, relayed)
}

/// A node with no listener that reaches `target` only at `addrs`.
async fn caller(target: PeerId, addrs: Vec<String>) -> (DhtHandle, PeerTable, NodeClient) {
    let peers = PeerTable::new();
    let node = dht::start(
        peers.clone(),
        config("/ip4/127.0.0.1/tcp/0", None, RelaySettings::default()),
    )
    .await;
    let mut info = peer_info(&node, addrs);
    info.libp2p_peer_id = Some(target.to_string());
    info.base_url = format!("http://{target}.test");
    peers.upsert(info);
    let client = NodeClient::new().with_stream(node.node_http.clone());
    (node, peers, client)
}

fn echo_router(hits: Arc<AtomicUsize>) -> Router {
    let seen = hits.clone();
    Router::new()
        .route(
            "/nodes/peers",
            get(
                move |peer: Option<Extension<RemotePeer>>,
                      ConnectInfo(addr): ConnectInfo<SocketAddr>| async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    Json(serde_json::json!({
                        "peer": peer.map(|Extension(RemotePeer(p))| p.to_string()),
                        "addr": addr.to_string(),
                    }))
                },
            ),
        )
        .route(
            "/nodes/status",
            get({
                let hits = hits.clone();
                move || async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    "ok"
                }
            }),
        )
        .route(
            "/nodes/relay",
            post(move || async move {
                hits.fetch_add(1, Ordering::SeqCst);
                "bound"
            }),
        )
}

// ---- impersonation ----

/// A circuit to an id that holds no reservation is refused by the relay; the relay does not
/// route to some other reserved peer instead.
#[tokio::test]
async fn a_circuit_to_a_peer_without_a_reservation_is_refused() {
    let (r, r_addr) = relay_node(tight_limits()).await;
    let mut holder = raw_swarm();
    assert!(raw_reserve(&mut holder, &r_addr).await);

    let victim = random_peer();
    let mut attacker = raw_swarm();
    let target = format!("{r_addr}/p2p-circuit/p2p/{victim}")
        .parse()
        .unwrap();
    assert!(dial_through_relay(&mut attacker, target, victim)
        .await
        .is_err());
    let counts = r.relay_stats.counts();
    assert_eq!(counts.circuits_accepted, 0);
    assert!(counts.circuits_denied >= 1);
}

/// A circuit address that ends at peer E, dialed as peer V, never yields a connection as V:
/// the swarm refuses the mismatch (before the handshake could), and nothing aimed at V
/// reaches E. Characterises libp2p's dial check, which Avalon's dial path relies on.
#[tokio::test]
async fn libp2p_a_circuit_that_ends_at_another_peer_than_the_dialed_one_is_never_used_as_it() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let (r, r_addr) = relay_node(tight_limits()).await;
    let e_hits = Arc::new(AtomicUsize::new(0));
    let (e, _e_peers, e_relayed) = private_server(&r_addr, &r, echo_router(e_hits.clone())).await;

    // A swarm asked to reach V over a circuit that actually ends at E.
    let victim = random_peer();
    let mut dialer = raw_swarm();
    dialer
        .dial(
            DialOpts::peer_id(victim)
                .addresses(vec![e_relayed.clone()])
                .build(),
        )
        .unwrap();
    let mut failed = false;
    let _ = tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            match dialer.select_next_some().await {
                SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                    panic!(
                        "connected as {peer_id} through a circuit that ends at {}",
                        e.peer_id
                    )
                }
                SwarmEvent::OutgoingConnectionError { .. } => {
                    failed = true;
                    return;
                }
                _ => {}
            }
        }
    })
    .await;
    assert!(failed, "the mismatched circuit was not refused");
    assert!(!dialer.is_connected(&victim) && !dialer.is_connected(&e.peer_id));

    // The same through the node's own dial path: a peer table entry for V that carries E's
    // relayed address, as a forged announce would if it slipped past admission.
    let (_x, _x_peers, client) = caller(victim, vec![e_relayed.to_string()]).await;
    let res = client
        .get(format!("{}/nodes/status", p2p_base_url(&victim)))
        .timeout(Duration::from_secs(8))
        .send()
        .await;
    assert!(res.is_err(), "a request for V was answered by E");
    assert_eq!(
        e_hits.load(Ordering::SeqCst),
        0,
        "E's handler saw V's request"
    );
}

/// The handler behind a circuit sees the dialing peer's own authenticated id, whatever the
/// request claims, and the peer-table binding of another peer does not carry over to it.
#[tokio::test]
async fn the_peer_behind_a_circuit_is_identified_by_its_own_handshake() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let (r, r_addr) = relay_node(tight_limits()).await;
    let hits = Arc::new(AtomicUsize::new(0));
    let (a, a_peers, a_relayed) = private_server(&r_addr, &r, echo_router(hits.clone())).await;
    let (m, _m_peers, m_client) = caller(a.peer_id, vec![a_relayed.to_string()]).await;

    // A bound peer V, which M will try to pass itself off as.
    let victim_node = dht::start(
        PeerTable::new(),
        config("/ip4/127.0.0.1/tcp/0", None, RelaySettings::default()),
    )
    .await;
    introduce(&a_peers, &victim_node);
    assert!(a_peers.is_bound_libp2p_peer(&victim_node.peer_id));
    assert!(!a_peers.is_bound_libp2p_peer(&m.peer_id));

    let res = m_client
        .get(format!("{}/nodes/peers", p2p_base_url(&a.peer_id)))
        .header("x-forwarded-for", "10.9.8.7")
        .header("forwarded", "for=10.9.8.7")
        .header("x-real-ip", "10.9.8.7")
        .header("x-avalon-peer", victim_node.peer_id.to_string())
        .send()
        .await
        .expect("M reaches A through the relay");
    let seen: serde_json::Value = res.json().await.unwrap();
    assert_eq!(seen["peer"], m.peer_id.to_string());
    assert_ne!(seen["peer"], victim_node.peer_id.to_string());
    assert_eq!(seen["addr"], SHARED_PEER_ADDR.to_string());
    assert!(r.relay_stats.counts().circuits_accepted >= 1);

    // V's binding does not open the write routes for M.
    let res = m_client
        .post(format!("{}/nodes/relay", p2p_base_url(&a.peer_id)))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

/// Announced addresses that name another peer, or chain relays, are dropped before they can
/// reach a dial list; a hostile announce of thousands of them stays bounded.
#[test]
fn a_hostile_announce_keeps_only_addresses_that_end_at_the_announced_peer() {
    let me = random_peer();
    let other = random_peer();
    let relay = random_peer();
    let policy = OutboundPolicy::new(false);
    let forged = [
        format!("/ip4/198.51.100.9/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{other}"),
        format!("/ip4/198.51.100.9/tcp/4001/p2p/{other}"),
        format!(
            "/ip4/198.51.100.9/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{relay}/p2p-circuit/p2p/{me}"
        ),
        format!("/ip4/10.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{me}"),
        format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{me}"),
        format!("/dns4/relay.example/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{me}"),
        format!("/ip4/198.51.100.9/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{me}/p2p/{other}"),
    ];
    assert!(sanitize_libp2p_addrs(Some(&me.to_string()), &forged, &policy).is_empty());

    let good = format!("/ip4/198.51.100.9/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{me}");
    let mut flood: Vec<String> = (0..5000)
        .map(|i| {
            format!(
                "/ip4/198.51.100.9/tcp/{}/p2p/{relay}/p2p-circuit/p2p/{me}",
                1 + i % 60000
            )
        })
        .collect();
    flood.push("x".repeat(1 << 20));
    flood.insert(0, good.clone());
    let kept = sanitize_libp2p_addrs(Some(&me.to_string()), &flood, &policy);
    assert!(kept.len() <= 8, "{} addresses kept", kept.len());
    assert_eq!(kept[0], good);
}

// ---- exhaustion ----

/// Forty free keypairs reserve and dial through a relay with small limits. The reservation
/// and circuit an honest peer got before the flood are untouched, the relay's active counts
/// never pass its limits, and once the hostile peers leave an honest peer is served again.
#[tokio::test]
async fn a_flood_of_free_keypairs_leaves_the_honest_peer_served_within_the_limits() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let limits = tight_limits();
    let (r, r_addr) = relay_node(limits.clone()).await;
    let honest = private_node(vec![r_addr.clone()], 1, &r).await;
    let relayed = reserved_on(&honest, r.peer_id).await;
    let mut early = raw_swarm();
    dial_through_relay(&mut early, relayed.clone(), honest.peer_id)
        .await
        .expect("the honest peer's circuit is up before the flood");

    let mut hostile = Vec::new();
    for _ in 0..40 {
        let mut h = raw_swarm();
        let _ = raw_reserve(&mut h, &r_addr).await;
        let _ = dial_through_relay(&mut h, relayed.clone(), honest.peer_id).await;
        let status = r.reachability.relay_server_status().unwrap();
        assert!(status.usage.reservations_active <= limits.max_reservations as u64);
        assert!(status.usage.circuits_active <= limits.max_circuits as u64);
        hostile.push(h);
    }

    let counts = r.relay_stats.counts();
    assert_eq!(counts.reservations_accepted, limits.max_reservations as u64);
    assert!(counts.reservations_denied >= 38, "{counts:?}");
    assert_eq!(counts.circuits_accepted, limits.max_circuits as u64);
    assert!(counts.circuits_denied >= 38, "{counts:?}");
    assert_eq!(
        honest.reachability.snapshot().relay_reservations.len(),
        1,
        "the honest reservation survived"
    );
    assert!(early.is_connected(&honest.peer_id));
    assert!(
        !closes_within(&mut early, Duration::from_millis(500)).await,
        "the early honest circuit was cut"
    );

    drop(hostile);
    eventually("the hostile circuits to close", || {
        r.reachability
            .relay_server_status()
            .is_some_and(|s| s.usage.circuits_active <= 1 && s.usage.reservations_active <= 1)
    })
    .await;
    let mut late = raw_swarm();
    dial_through_relay(&mut late, relayed, honest.peer_id)
        .await
        .expect("an honest peer is served once the hostile peers are gone");
}

/// One keypair is held to its per-peer circuit limit, so a single peer cannot take the relay's
/// whole circuit table. libp2p-relay compares with `>`, so the effective per-peer bound is the
/// configured limit plus one; the test pins that bound.
#[tokio::test]
async fn one_keypair_cannot_hold_more_than_its_circuits_per_peer() {
    let (r, r_addr) = relay_node(RelayServerSettings {
        max_reservations: 10,
        max_circuits: 10,
        max_circuits_per_peer: 2,
        ..RelayServerSettings::default()
    })
    .await;
    // Four reserved peers, kept running so each answers a circuit.
    let mut targets = Vec::new();
    for _ in 0..4 {
        let mut h = raw_swarm();
        assert!(raw_reserve(&mut h, &r_addr).await);
        let id = *h.local_peer_id();
        tokio::spawn(async move {
            loop {
                h.select_next_some().await;
            }
        });
        targets.push(id);
    }

    let mut m = raw_swarm();
    m.dial(r_addr.clone()).unwrap();
    tokio::time::timeout(WAIT, async {
        while !matches!(
            m.select_next_some().await,
            SwarmEvent::ConnectionEstablished { .. }
        ) {}
    })
    .await
    .expect("connected to the relay");
    for id in &targets {
        m.dial(
            format!("{r_addr}/p2p-circuit/p2p/{id}")
                .parse::<Multiaddr>()
                .unwrap(),
        )
        .unwrap();
    }
    let _ = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            m.select_next_some().await;
        }
    })
    .await;
    let counts = r.relay_stats.counts();
    assert!(counts.circuits_accepted <= 3, "{counts:?}");
    assert!(counts.circuits_denied >= 1, "{counts:?}");
}

fn client_swarm() -> Swarm<relay::client::Behaviour> {
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
        .with_behaviour(|_, relay_client| relay_client)
        .unwrap()
        .build()
}

/// Thousands of fake relays reported by connected peers cannot crowd out an operator relay and
/// are tried only up to the candidate cap.
#[tokio::test]
async fn a_flood_of_fake_relay_candidates_is_bounded_and_operator_relays_survive() {
    let (r, r_addr) = relay_node(RelayServerSettings::default()).await;
    let mut swarm = client_swarm();
    let local = *swarm.local_peer_id();
    let handle = ReachabilityHandle::new(None);
    let mut client = RelayClient::new(
        RelayClientSettings {
            max_reservations: 8,
            relay_addrs: vec![r_addr],
            allow_private: true,
            retry_backoff: Duration::from_secs(600),
            retry_backoff_max: Duration::from_secs(600),
            pending_timeout: Duration::from_secs(30),
            ..RelayClientSettings::default()
        },
        local,
        handle.clone(),
    );
    for i in 0..5000u32 {
        client.note_hop_relay(
            random_peer(),
            vec![format!("/ip4/127.0.0.1/tcp/{}", 1 + i % 3).parse().unwrap()],
        );
    }

    let mut attempts_failed = 0;
    client.reconcile(&mut swarm, Reachability::Private, Instant::now());
    let _ = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let idle = tokio::time::timeout(Duration::from_millis(1500), swarm.select_next_some());
            match idle.await {
                Err(_) => return,
                Ok(SwarmEvent::Behaviour(relay::client::Event::ReservationReqAccepted {
                    relay_peer_id,
                    renewal,
                    ..
                })) => client.on_accepted(relay_peer_id, renewal),
                Ok(SwarmEvent::ListenerClosed { listener_id, .. }) => {
                    attempts_failed += 1;
                    client.on_listener_closed(listener_id, Instant::now());
                    client.reconcile(&mut swarm, Reachability::Private, Instant::now());
                }
                Ok(_) => {}
            }
        }
    })
    .await;

    let held: Vec<String> = handle
        .snapshot()
        .relay_reservations
        .into_iter()
        .map(|r| r.relay_peer_id)
        .collect();
    assert_eq!(
        held,
        vec![r.peer_id.to_string()],
        "the operator relay is held"
    );
    // 64 candidates at most, the operator relay among them and not a failure.
    assert!(
        attempts_failed <= 63,
        "{attempts_failed} fake relays were tried"
    );
    assert!(
        attempts_failed >= 8,
        "the flood was actually tried: {attempts_failed}"
    );
}

// ---- malformed frames ----

const FLOOD: Duration = Duration::from_secs(2);

fn varint(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

/// The frames a raw peer can put on a length-delimited libp2p protocol.
fn hostile_frames() -> Vec<(&'static str, Hostile, usize)> {
    vec![
        ("garbage bytes", Hostile::bytes(&[0xff; 64]), usize::MAX),
        ("empty stream", Hostile::bytes(&[]), usize::MAX),
        (
            "length prefix with no body",
            Hostile::bytes(&varint(100)),
            usize::MAX,
        ),
        (
            "body shorter than its length prefix",
            Hostile::bytes(&[varint(100), vec![8, 1]].concat()),
            usize::MAX,
        ),
        (
            "invalid protobuf of the declared length",
            Hostile::bytes(&[varint(8), vec![0xff; 8]].concat()),
            usize::MAX,
        ),
        (
            "length beyond any limit then a flood",
            Hostile::flooding(&varint(1 << 30), FLOOD),
            1 << 20,
        ),
        (
            "endless varint then a flood",
            Hostile::flooding(&[0x80; 32], FLOOD),
            1 << 20,
        ),
    ]
}

async fn hit_with_frames(protocol: libp2p::StreamProtocol, target: Multiaddr, target_id: PeerId) {
    let (mut hostile, written) = hostile_swarm(protocol);
    connect(&mut hostile, target, target_id).await;
    for (name, request, max_written) in hostile_frames() {
        let before = written.load(Ordering::SeqCst);
        let started = Instant::now();
        let outcome = send_all(&mut hostile, target_id, vec![request]).await;
        assert!(
            match &outcome[0] {
                Outcome::Answered(b) => b.is_empty(),
                Outcome::Failed(_) => true,
            },
            "{name}: {:?}",
            outcome[0]
        );
        assert!(
            written.load(Ordering::SeqCst) - before < max_written,
            "{name}: the receiver kept reading a refused frame"
        );
        assert!(started.elapsed() < Duration::from_secs(3), "{name}: slow");
    }
    let rapid: Vec<Hostile> = (0..200)
        .map(|i| match i % 2 {
            0 => Hostile::bytes(&[0xff; 16]),
            _ => Hostile::bytes(&varint(1 << 20)),
        })
        .collect();
    let outcomes = send_all(&mut hostile, target_id, rapid).await;
    assert_eq!(outcomes.len(), 200);
}

/// Malformed, truncated and oversized frames on the relay hop protocol are refused, produce no
/// reservation or circuit, and the relay still serves an honest peer. The frame limits belong
/// to the libp2p relay codec.
#[tokio::test]
async fn libp2p_hop_protocol_frames_are_refused_and_the_relay_keeps_serving() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let (r, r_addr) = relay_node(tight_limits()).await;
    hit_with_frames(relay::HOP_PROTOCOL_NAME, r_addr.clone(), r.peer_id).await;
    let counts = r.relay_stats.counts();
    assert_eq!(counts.reservations_accepted, 0, "{counts:?}");
    assert_eq!(counts.circuits_accepted, 0, "{counts:?}");

    let a = private_node(vec![r_addr], 1, &r).await;
    let relayed = reserved_on(&a, r.peer_id).await;
    let mut b = raw_swarm();
    dial_through_relay(&mut b, relayed, a.peer_id)
        .await
        .expect("an honest peer is served after the hostile frames");
    assert_eq!(r.relay_stats.counts().reservations_accepted, 1);
}

/// The same frames on the stop protocol of a relayed node (the side a relay talks to): the node
/// keeps its reservation and stays reachable.
#[tokio::test]
async fn libp2p_stop_protocol_frames_are_refused_and_the_reserved_node_stays_reachable() {
    if !ipv6_available() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let (r, r_addr) = relay_node(tight_limits()).await;
    let a = private_node(vec![r_addr], 1, &r).await;
    let relayed = reserved_on(&a, r.peer_id).await;
    let direct: Multiaddr = a.listen_addrs[0].clone();
    hit_with_frames(relay::STOP_PROTOCOL_NAME, direct, a.peer_id).await;

    assert_eq!(a.reachability.snapshot().relay_reservations.len(), 1);
    let mut b = raw_swarm();
    dial_through_relay(&mut b, relayed, a.peer_id)
        .await
        .expect("still reachable through the relay");
    assert_eq!(r.relay_stats.counts().circuits_accepted, 1);
}
