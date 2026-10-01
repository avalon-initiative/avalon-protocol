//! Probe and trace over libp2p streams between real in-process swarms: the result names the
//! path type of the connection that carried it. No database needed.

use std::time::Duration;

use avalon_protocol::connectivity::{Connectivity, PathType};
use avalon_server::dht::{self, DhtConfig, DhtHandle};
use avalon_server::node_http::{install_stream_handle, p2p_base_url, NodeClient};
use avalon_server::nodes::{PeerInfo, PeerTable};
use avalon_server::outbound_policy::OutboundPolicy;
use avalon_server::overlay_routing::OverlayNode;
use avalon_server::reachability::{AutonatSettings, ReachabilitySnapshot};
use avalon_server::relay::{RelayClientSettings, RelayServerSettings, RelaySettings};
use avalon_server::topology_probe::measure;
use avalon_server::topology_trace::{
    ForwardOutcome, Forwarder, HttpForwarder, TraceHop, TraceRequest, TraceResponse,
};
use axum::routing::{get, post};
use axum::{Json, Router};
use libp2p::{identity, Multiaddr};

const WAIT: Duration = Duration::from_secs(30);
const NETWORK: &str = "avalon-test";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn config(listen: &str, external: Option<&str>, relay: RelaySettings) -> DhtConfig {
    DhtConfig {
        identity: identity::Keypair::generate_ed25519(),
        network_id: NETWORK.to_string(),
        listen_addr: listen.parse().unwrap(),
        external_addr: external.map(|e| e.parse().unwrap()),
        autonat: AutonatSettings {
            allow_private: true,
            boot_delay: Duration::from_millis(100),
            retry_interval: Duration::from_millis(300),
            throttle_server_period: Duration::ZERO,
            ..AutonatSettings::default()
        },
        relay,
        relay_resilience: Default::default(),
        bootstrap_scan_interval: Duration::from_millis(300),
    }
}

/// Answers `/nodes/status` and `/nodes/trace` like a node that is the trace target.
fn router() -> Router {
    Router::new()
        .route("/nodes/status", get(|| async { r#"{"ok":true}"# }))
        .route(
            "/nodes/trace",
            post(|Json(req): Json<TraceRequest>| async move {
                Json(TraceResponse {
                    trace_id: req.trace_id.unwrap_or_default(),
                    target: req.target,
                    reached: true,
                    stopped_reason: None,
                    detail: None,
                    total_ms: 1.0,
                    hops: vec![TraceHop {
                        index: 0,
                        base_url: "http://far.test".to_string(),
                        roles: vec!["combined".to_string()],
                        protocol_version: "1".to_string(),
                        processing_ms: 1.0,
                        to_next_ms: None,
                        path_to_next: None,
                    }],
                })
            }),
        )
}

fn info_for(node: &DhtHandle, addrs: Vec<String>, connectivity: Connectivity) -> PeerInfo {
    PeerInfo {
        identity_bound: true,
        base_url: format!("http://{}.invalid", node.peer_id),
        roles: vec!["combined".to_string()],
        protocol_version: avalon_server::version::PROTOCOL_VERSION.to_string(),
        network_id: NETWORK.to_string(),
        last_announced_at: time::OffsetDateTime::now_utc(),
        libp2p_peer_id: Some(node.peer_id.to_string()),
        libp2p_listen_addrs: addrs,
        connectivity: Some(connectivity),
        witness: None,
    }
}

async fn trace_over(
    forwarder: &HttpForwarder,
    node: &DhtHandle,
    base_url: &str,
) -> Option<PathType> {
    let neighbor = OverlayNode {
        base_url: base_url.to_string(),
        network_id: NETWORK.to_string(),
        libp2p_peer_id: Some(node.peer_id.to_string()),
    };
    let request = TraceRequest {
        target: "http://far.test".to_string(),
        ttl: Some(1),
        trace_id: Some(uuid::Uuid::nil()),
        visited: None,
        budget_ms: Some(5000),
    };
    match forwarder
        .forward(&neighbor, &request, Duration::from_secs(5))
        .await
    {
        ForwardOutcome::Response(r, path) => {
            assert!(r.reached);
            path
        }
        other => panic!("trace leg failed: {other:?}"),
    }
}

/// One client node reaches a direct peer and a peer behind a relay; each round trip is
/// labeled with the path that carried it, relayed never as direct.
#[tokio::test]
async fn probe_and_trace_label_direct_and_relayed_paths() {
    if std::net::TcpListener::bind("[::1]:0").is_err() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    let relay_addr = format!("/ip4/127.0.0.1/tcp/{}", free_port());
    let relay = dht::start(
        PeerTable::new(),
        config(
            &relay_addr,
            Some(&relay_addr),
            RelaySettings {
                server: Some(RelayServerSettings::default()),
                ..RelaySettings::default()
            },
        ),
    )
    .await;
    let relay_full: Multiaddr = format!("{relay_addr}/p2p/{}", relay.peer_id)
        .parse()
        .unwrap();

    // Listens on IPv6 loopback only, so the IPv4 client has no direct path to it.
    let private_peers = PeerTable::new();
    let private = dht::start(
        private_peers.clone(),
        config(
            "/ip6/::1/tcp/0",
            None,
            RelaySettings {
                client: RelayClientSettings {
                    max_reservations: 1,
                    relay_addrs: vec![relay_full],
                    allow_private: true,
                    retry_backoff: Duration::from_secs(60),
                    pending_timeout: Duration::from_secs(10),
                    reconcile_interval: Duration::from_millis(200),
                    ..RelayClientSettings::default()
                },
                ..RelaySettings::default()
            },
        ),
    )
    .await;
    private_peers.upsert(info_for(
        &relay,
        relay.listen_addrs.iter().map(|a| a.to_string()).collect(),
        Connectivity::Direct,
    ));
    private.router_slot.set(router());
    let relayed_addr = tokio::time::timeout(WAIT, async {
        let mut rx = private.reachability.subscribe();
        loop {
            let found = |s: &ReachabilitySnapshot| {
                s.relay_reservations
                    .iter()
                    .find(|r| r.relay_peer_id == relay.peer_id.to_string())
                    .map(|r| r.relayed_addr.clone())
            };
            if let Some(addr) = found(&rx.borrow_and_update()) {
                return addr;
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .expect("the private node reserves a slot on the relay");

    let direct_peers = PeerTable::new();
    let direct = dht::start(
        direct_peers,
        config(
            &format!("/ip4/127.0.0.1/tcp/{}", free_port()),
            None,
            RelaySettings::default(),
        ),
    )
    .await;
    direct.router_slot.set(router());

    let client_peers = PeerTable::new();
    let client_node = dht::start(
        client_peers.clone(),
        config("/ip4/127.0.0.1/tcp/0", None, RelaySettings::default()),
    )
    .await;
    client_peers.upsert(info_for(
        &private,
        vec![relayed_addr],
        Connectivity::Relayed,
    ));
    client_peers.upsert(info_for(
        &direct,
        direct.listen_addrs.iter().map(|a| a.to_string()).collect(),
        Connectivity::OutboundOnly,
    ));
    let paths = client_peers.paths().clone();
    let client = NodeClient::new().with_stream(client_node.node_http.clone());

    for (node, want) in [(&private, PathType::Relayed), (&direct, PathType::Direct)] {
        let result = measure(
            &client,
            &p2p_base_url(&node.peer_id),
            Some(node.peer_id),
            &paths,
            2,
        )
        .await;
        assert!(result.ok, "{:?}", result.error);
        assert_eq!(result.samples_ms.len(), 2);
        assert_eq!(result.path, Some(want));
    }

    // Trace legs go through the process-wide stream handle.
    install_stream_handle(client_node.node_http.clone());
    let forwarder = HttpForwarder {
        policy: OutboundPolicy::new(true),
        peers: Some(client_peers),
    };
    let relayed_url = info_for(&private, vec![], Connectivity::Relayed).base_url;
    let direct_url = info_for(&direct, vec![], Connectivity::OutboundOnly).base_url;
    assert_eq!(
        trace_over(&forwarder, &private, &relayed_url).await,
        Some(PathType::Relayed)
    );
    assert_eq!(
        trace_over(&forwarder, &direct, &direct_url).await,
        Some(PathType::Direct)
    );
}
