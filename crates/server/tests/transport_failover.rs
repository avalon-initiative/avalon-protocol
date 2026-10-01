//! Transport failover between real in-process swarms: a request that cannot connect over one
//! transport continues over the other, and an application refusal never changes transport.
//! No database needed.

use std::time::Duration;

use avalon_protocol::connectivity::Connectivity;
use avalon_server::dht::{self, DhtConfig, DhtHandle};
use avalon_server::node_http::{p2p_base_url, NodeClient};
use avalon_server::nodes::{PeerInfo, PeerTable};
use avalon_server::reachability::AutonatSettings;
use avalon_server::relay::RelaySettings;
use avalon_server::transport_stats::Transport;
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use libp2p::identity;
use tokio::task::JoinHandle;

const NETWORK: &str = "avalon-test";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn config() -> DhtConfig {
    DhtConfig {
        identity: identity::Keypair::generate_ed25519(),
        network_id: NETWORK.to_string(),
        listen_addr: format!("/ip4/127.0.0.1/tcp/{}", free_port())
            .parse()
            .unwrap(),
        external_addr: None,
        autonat: AutonatSettings {
            allow_private: true,
            boot_delay: Duration::from_millis(100),
            retry_interval: Duration::from_millis(300),
            throttle_server_period: Duration::ZERO,
            ..AutonatSettings::default()
        },
        relay: RelaySettings::default(),
        bootstrap_scan_interval: Duration::from_millis(300),
    }
}

/// The router a node answers with over both transports; the body names the transport.
fn router(label: &'static str, status: StatusCode) -> Router {
    Router::new().route("/nodes/status", get(move || async move { (status, label) }))
}

/// A node's real HTTP listener, returning its base URL and the task to abort.
async fn serve_http(app: Router) -> (String, JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (url, task)
}

fn entry(base_url: &str, peer: &DhtHandle, c: Connectivity) -> PeerInfo {
    PeerInfo {
        identity_bound: true,
        base_url: base_url.to_string(),
        roles: vec!["combined".to_string()],
        protocol_version: avalon_server::version::PROTOCOL_VERSION.to_string(),
        network_id: NETWORK.to_string(),
        last_announced_at: time::OffsetDateTime::now_utc(),
        libp2p_peer_id: Some(peer.peer_id.to_string()),
        libp2p_listen_addrs: peer.listen_addrs.iter().map(|a| a.to_string()).collect(),
        connectivity: Some(c),
        witness: None,
    }
}

#[tokio::test]
async fn failover_runs_both_ways_over_real_swarms() {
    let table = PeerTable::new();
    let client_node = dht::start(table.clone(), config()).await;
    let client = NodeClient::new().with_stream(client_node.node_http.clone());
    let stats = table.transport_stats().clone();

    // The server's stream side answers "stream"; its HTTP side "http".
    let server = dht::start(PeerTable::new(), config()).await;
    server.router_slot.set(router("stream", StatusCode::OK));
    let (http_url, http_task) = serve_http(router("http", StatusCode::OK)).await;
    table.upsert(entry(&http_url, &server, Connectivity::Direct));
    let id = server.peer_id;

    // Direct hint, URL answering: HTTP is used and nothing is demoted.
    let res = client
        .get(format!("{http_url}/nodes/status"))
        .send()
        .await
        .unwrap();
    assert!(!res.via_stream());
    assert_eq!(res.text().await.unwrap(), "http");
    assert!(!stats.is_demoted(&id, Transport::Http));

    // The URL dies while the hint still says direct: the request continues over the stream.
    http_task.abort();
    // Give the aborted listener a moment to close its socket.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let res = client
        .get(format!("{http_url}/nodes/status"))
        .send()
        .await
        .unwrap();
    assert!(res.via_stream());
    assert_eq!(res.text().await.unwrap(), "stream");
    assert!(stats.is_demoted(&id, Transport::Http));
    // Later URL choices for the peer follow the stream.
    assert_eq!(table.transport_url(&http_url), p2p_base_url(&id));

    // The reverse: a peer hinted stream-only whose stream cannot connect (nothing listens at
    // that id) but whose URL answers.
    let ghost = dht::start(PeerTable::new(), config()).await;
    let (live_url, _live) = serve_http(router("http", StatusCode::OK)).await;
    let mut ghost_entry = entry(&live_url, &ghost, Connectivity::Relayed);
    ghost_entry.libp2p_listen_addrs = vec![format!("/ip4/127.0.0.1/tcp/{}", free_port())];
    table.upsert(ghost_entry);
    let res = client
        .get(format!("{}/nodes/status", p2p_base_url(&ghost.peer_id)))
        .send()
        .await
        .unwrap();
    assert!(!res.via_stream());
    assert_eq!(res.text().await.unwrap(), "http");
    assert!(stats.is_demoted(&ghost.peer_id, Transport::Stream));
}

#[tokio::test]
async fn a_refusal_over_either_transport_is_returned_not_retried_elsewhere() {
    let table = PeerTable::new();
    let client_node = dht::start(table.clone(), config()).await;
    let client = NodeClient::new().with_stream(client_node.node_http.clone());
    let stats = table.transport_stats().clone();

    let server = dht::start(PeerTable::new(), config()).await;
    server
        .router_slot
        .set(router("stream-refused", StatusCode::FORBIDDEN));
    let (http_url, _http) = serve_http(router("http", StatusCode::OK)).await;
    table.upsert(entry(&http_url, &server, Connectivity::Relayed));

    // Stream-preferred peer whose stream answers 403: the 403 comes back and HTTP, which would
    // have said 200, is not tried.
    let res = client
        .get(format!("{}/nodes/status", p2p_base_url(&server.peer_id)))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(res.via_stream());
    assert!(!stats.is_demoted(&server.peer_id, Transport::Stream));

    // And an HTTP 503 never moves a request onto the stream, which would have said 403.
    let other = dht::start(PeerTable::new(), config()).await;
    other.router_slot.set(router("stream", StatusCode::OK));
    let (down_url, _down) = serve_http(router("http-down", StatusCode::SERVICE_UNAVAILABLE)).await;
    table.upsert(entry(&down_url, &other, Connectivity::Direct));
    let res = client
        .get(format!("{down_url}/nodes/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(!res.via_stream());
    assert!(!stats.is_demoted(&other.peer_id, Transport::Http));
}
