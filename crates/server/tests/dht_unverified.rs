//! A peer known only from gossip, with no reachable URL, is dialed over libp2p and moves into
//! the peer table once the authenticated connection confirms it. No database or HTTP server.

use std::time::Duration;

use avalon_server::dht::{self, DhtConfig, DhtHandle};
use avalon_server::nodes::{PeerInfo, PeerTable};
use avalon_server::reachability::AutonatSettings;
use avalon_server::relay::RelaySettings;
use libp2p::identity;

const NETWORK: &str = "avalon-test";
const WAIT: Duration = Duration::from_secs(20);

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn node(peers: PeerTable) -> (DhtHandle, String) {
    let addr = format!("/ip4/127.0.0.1/tcp/{}", free_port());
    let config = DhtConfig {
        identity: identity::Keypair::generate_ed25519(),
        network_id: NETWORK.to_string(),
        listen_addr: addr.parse().unwrap(),
        external_addr: Some(addr.parse().unwrap()),
        autonat: AutonatSettings {
            enabled: false,
            ..AutonatSettings::default()
        },
        relay: RelaySettings::default(),
        bootstrap_scan_interval: Duration::from_millis(200),
    };
    let handle = dht::start(peers, config).await;
    (handle, addr)
}

fn gossiped(url: &str, peer: &DhtHandle, addr: &str) -> PeerInfo {
    PeerInfo {
        base_url: url.to_string(),
        roles: vec!["combined".to_string()],
        protocol_version: avalon_server::version::PROTOCOL_VERSION.to_string(),
        network_id: NETWORK.to_string(),
        last_announced_at: time::OffsetDateTime::now_utc(),
        libp2p_peer_id: Some(peer.peer_id.to_string()),
        libp2p_listen_addrs: vec![addr.to_string()],
        connectivity: None,
        witness: None,
    }
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

#[tokio::test]
async fn a_pooled_peer_is_dialed_and_promoted_once_libp2p_confirms_it() {
    let (b, b_addr) = node(PeerTable::new()).await;
    let a_peers = PeerTable::new();
    let (_a, _) = node(a_peers.clone()).await;
    // B's HTTP URL is unreachable; only its libp2p address works.
    a_peers.insert_unverified(gossiped("http://unreachable.invalid", &b, &b_addr));

    eventually("the pooled peer to be promoted", || {
        a_peers
            .list_all()
            .iter()
            .any(|p| p.base_url == "http://unreachable.invalid")
    })
    .await;
    assert!(a_peers.list_unverified().is_empty());
}

#[tokio::test]
async fn a_pooled_peer_that_cannot_be_reached_stays_unverified() {
    let (b, _) = node(PeerTable::new()).await;
    let a_peers = PeerTable::new();
    let (_a, _) = node(a_peers.clone()).await;
    // A real peer id, but an address where nothing listens.
    let dead = format!("/ip4/127.0.0.1/tcp/{}", free_port());
    a_peers.insert_unverified(gossiped("http://unreachable.invalid", &b, &dead));

    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(a_peers.list_all().is_empty());
    assert_eq!(a_peers.list_unverified().len(), 1);
}
