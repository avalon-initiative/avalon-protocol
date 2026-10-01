//! AutoNAT reachability detection between real in-process libp2p swarms on loopback. No
//! database or HTTP server needed.

use std::time::Duration;

use avalon_server::dht::{self, DhtConfig, DhtHandle};
use avalon_server::nodes::{PeerInfo, PeerTable};
use avalon_server::reachability::{AutonatSettings, Reachability};
use libp2p::futures::StreamExt;
use libp2p::swarm::SwarmEvent;
use libp2p::{autonat, identity, noise, tcp, yamux, Multiaddr, SwarmBuilder};

const WAIT: Duration = Duration::from_secs(30);

fn fast_settings() -> AutonatSettings {
    AutonatSettings {
        allow_private: true,
        boot_delay: Duration::from_millis(100),
        retry_interval: Duration::from_millis(300),
        throttle_server_period: Duration::ZERO,
        ..AutonatSettings::default()
    }
}

fn config(listen: &str, autonat: AutonatSettings) -> DhtConfig {
    DhtConfig {
        identity: identity::Keypair::generate_ed25519(),
        network_id: "avalon-test".to_string(),
        listen_addr: listen.parse().unwrap(),
        external_addr: None,
        autonat,
        relay: Default::default(),
        relay_resilience: Default::default(),
        bootstrap_scan_interval: Duration::from_millis(300),
    }
}

fn introduce(into: &PeerTable, node: &DhtHandle, base_url: &str) {
    into.upsert(PeerInfo {
        identity_bound: false,
        base_url: base_url.to_string(),
        roles: vec!["combined".to_string()],
        protocol_version: avalon_server::version::PROTOCOL_VERSION.to_string(),
        network_id: "avalon-test".to_string(),
        last_announced_at: time::OffsetDateTime::now_utc(),
        libp2p_peer_id: Some(node.peer_id.to_string()),
        libp2p_listen_addrs: node.listen_addrs.iter().map(|a| a.to_string()).collect(),
        connectivity: None,
        witness: None,
    });
}

async fn settle(node: &DhtHandle) -> Reachability {
    let mut rx = node.reachability.subscribe();
    tokio::time::timeout(WAIT, async {
        loop {
            let seen = rx.borrow_and_update().reachability;
            if seen != Reachability::Unknown {
                return seen;
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .expect("reachability never left unknown")
}

/// A node listening on IPv4 is dialed back and becomes `public` with a confirmed address; a
/// node listening only on IPv6 loopback cannot be dialed at the address the server observes,
/// so it becomes `private` and advertises nothing.
#[tokio::test]
async fn reachable_node_becomes_public_and_unreachable_node_private() {
    let server_peers = PeerTable::new();
    let reachable_peers = PeerTable::new();
    let unreachable_peers = PeerTable::new();

    let server = dht::start(
        server_peers.clone(),
        config("/ip4/127.0.0.1/tcp/0", fast_settings()),
    )
    .await;
    let reachable = dht::start(
        reachable_peers.clone(),
        config("/ip4/127.0.0.1/tcp/0", fast_settings()),
    )
    .await;
    if dht_can_bind_ipv6().is_err() {
        eprintln!("skipping: no IPv6 loopback");
        return;
    }
    let unreachable = dht::start(
        unreachable_peers.clone(),
        config("/ip6/::1/tcp/0", fast_settings()),
    )
    .await;

    for node in [&reachable, &unreachable] {
        assert_eq!(
            node.reachability.snapshot().reachability,
            Reachability::Unknown
        );
        assert!(node.reachability.advertised_addrs().is_empty());
    }

    introduce(&reachable_peers, &server, "http://server.test");
    introduce(&unreachable_peers, &server, "http://server.test");

    assert_eq!(settle(&reachable).await, Reachability::Public);
    let snap = reachable.reachability.snapshot();
    assert!(!snap.confirmed_addrs.is_empty());
    assert_eq!(
        reachable.reachability.advertised_addrs(),
        snap.confirmed_addrs
    );
    assert!(snap.confirmed_addrs[0].starts_with("/ip4/127.0.0.1/tcp/"));

    assert_eq!(settle(&unreachable).await, Reachability::Private);
    assert!(unreachable
        .reachability
        .snapshot()
        .confirmed_addrs
        .is_empty());
    assert!(unreachable.reachability.advertised_addrs().is_empty());
}

fn dht_can_bind_ipv6() -> std::io::Result<()> {
    std::net::TcpListener::bind("[::1]:0").map(|_| ())
}

/// With private dial-back off (the default) a loopback-only network never leaves `unknown`:
/// the server refuses to dial a private observed address.
#[tokio::test]
async fn private_dialback_is_refused_by_default() {
    let mut refuse = fast_settings();
    refuse.allow_private = false;
    let server_peers = PeerTable::new();
    let client_peers = PeerTable::new();
    let server = dht::start(server_peers, config("/ip4/127.0.0.1/tcp/0", refuse.clone())).await;
    let client = dht::start(client_peers.clone(), config("/ip4/127.0.0.1/tcp/0", refuse)).await;
    introduce(&client_peers, &server, "http://server.test");

    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(
        client.reachability.snapshot().reachability,
        Reachability::Unknown
    );
    assert!(client.reachability.advertised_addrs().is_empty());
}

/// A node with detection disabled stays `unknown` even when a server is available.
#[tokio::test]
async fn disabled_detection_stays_unknown() {
    let mut off = fast_settings();
    off.enabled = false;
    let server_peers = PeerTable::new();
    let client_peers = PeerTable::new();
    let server = dht::start(
        server_peers,
        config("/ip4/127.0.0.1/tcp/0", fast_settings()),
    )
    .await;
    let client = dht::start(client_peers.clone(), config("/ip4/127.0.0.1/tcp/0", off)).await;
    introduce(&client_peers, &server, "http://server.test");

    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        client.reachability.snapshot().reachability,
        Reachability::Unknown
    );
}

fn raw_swarm(settings: &AutonatSettings) -> libp2p::Swarm<autonat::Behaviour> {
    SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .unwrap()
        .with_behaviour(|key| {
            autonat::Behaviour::new(key.public().to_peer_id(), settings.libp2p_config())
        })
        .unwrap()
        .build()
}

/// A server configured for one dial-back per minute answers the first probe and refuses
/// the rest, even though the client keeps asking.
#[tokio::test]
async fn server_refuses_dialbacks_beyond_the_configured_rate() {
    let mut server_settings = fast_settings();
    server_settings.dialbacks_per_minute = 1;
    let mut server = raw_swarm(&server_settings);
    let mut client = raw_swarm(&fast_settings());

    server
        .listen_on("/ip4/127.0.0.1/tcp/0".parse::<Multiaddr>().unwrap())
        .unwrap();
    let server_addr = loop {
        if let SwarmEvent::NewListenAddr { address, .. } = server.select_next_some().await {
            break address;
        }
    };
    client
        .listen_on("/ip4/127.0.0.1/tcp/0".parse::<Multiaddr>().unwrap())
        .unwrap();
    client.dial(server_addr).unwrap();

    let (mut answered, mut refused) = (0, 0);
    let outcome = tokio::time::timeout(WAIT, async {
        loop {
            tokio::select! {
                _ = client.select_next_some() => {}
                event = server.select_next_some() => {
                    if let SwarmEvent::Behaviour(autonat::Event::InboundProbe(probe)) = event {
                        match probe {
                            autonat::InboundProbeEvent::Response { .. } => answered += 1,
                            autonat::InboundProbeEvent::Error {
                                error: autonat::InboundProbeError::Response(
                                    autonat::ResponseError::DialRefused,
                                ),
                                ..
                            } => refused += 1,
                            _ => {}
                        }
                        if answered >= 1 && refused >= 2 {
                            return;
                        }
                    }
                }
            }
        }
    })
    .await;
    assert!(
        outcome.is_ok(),
        "expected one answered and refused repeats, got answered={answered} refused={refused}"
    );
    assert_eq!(answered, 1);
}
