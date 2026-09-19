//! Issue #583's own live acceptance test: a local subscriber's interest,
//! registered as a real DHT `put_record` on one node, is found by a real
//! `get_record` lookup from a second, separately-bootstrapped node — the
//! exact primitive #584 will call from the relay path, verified in
//! isolation here. Fully self-contained (two real `dht::start()` swarms in
//! this one process, no Postgres/HTTP/auth needed — `crate::dht`/
//! `crate::interest` are plain async functions), but still real sockets
//! and a real noise handshake, so `--ignored` like every other live test in
//! this crate.
//!
//! ```text
//! cargo test -p avalon-server --test interest_dht -- --ignored
//! ```

use std::time::Duration;

use avalon_server::dht::{self, DhtConfig};
use avalon_server::interest::{self, InterestRegistry, InterestScope};
use avalon_server::nodes::{PeerInfo, PeerTable};
use uuid::Uuid;

/// Short enough that the test doesn't take minutes, long enough that a
/// couple of scan ticks reliably happen within the test's own wait budget.
const TEST_SCAN_INTERVAL: Duration = Duration::from_millis(500);

fn test_config() -> DhtConfig {
    DhtConfig {
        identity: libp2p::identity::Keypair::generate_ed25519(),
        listen_addr: "/ip4/127.0.0.1/tcp/0".parse().unwrap(),
        external_addr: None,
        bootstrap_scan_interval: TEST_SCAN_INTERVAL,
    }
}

/// Wires `a`'s real DHT identity into `b`'s peer table (and vice versa) —
/// standing in for what a real `POST /nodes/announce` round trip
/// (`crate::nodes`) would otherwise populate; this test isn't exercising
/// that HTTP path, already covered live by #582's own verification.
fn cross_register(a_peers: &PeerTable, a_handle: &dht::DhtHandle, b_base_url: &str) {
    a_peers.upsert(PeerInfo {
        base_url: b_base_url.to_string(),
        roles: vec!["combined".to_string()],
        protocol_version: avalon_server::version::PROTOCOL_VERSION.to_string(),
        network_id: "avalon-test".to_string(),
        last_announced_at: time::OffsetDateTime::now_utc(),
        libp2p_peer_id: Some(a_handle.peer_id.to_string()),
        libp2p_listen_addrs: a_handle
            .listen_addrs
            .iter()
            .map(|a| a.to_string())
            .collect(),
    });
}

#[tokio::test]
#[ignore]
async fn a_registered_channel_interest_is_found_by_a_lookup_from_a_different_node() {
    let node_a_peers = PeerTable::new();
    let node_b_peers = PeerTable::new();

    let node_a = dht::start(node_a_peers.clone(), test_config()).await;
    let node_b = dht::start(node_b_peers.clone(), test_config()).await;

    // Each side learns the other's real DHT identity, as #582's own
    // `nodes::announce` handler would populate it live.
    cross_register(&node_b_peers, &node_a, "http://node-a.test");
    cross_register(&node_a_peers, &node_b, "http://node-b.test");

    // Give both sides' bootstrap-scan workers time to actually dial and
    // connect — bounded well above `TEST_SCAN_INTERVAL` so this isn't
    // racing the very first tick.
    tokio::time::sleep(TEST_SCAN_INTERVAL * 6).await;

    let registry = InterestRegistry::new();
    let channel_id = Uuid::new_v4();
    let scope = InterestScope::Channel(channel_id);
    let _guard = registry.register(scope);

    // What `interest::run_worker` would do on its own refresh tick — called
    // directly here rather than spawning the worker, so this test controls
    // exactly when the put happens instead of waiting out a real refresh
    // interval.
    node_a
        .commands
        .send(avalon_server::dht::DhtCommand::PutRecord {
            key: scope.dht_key(),
            value: b"http://node-a.test".to_vec(),
            ttl: Duration::from_secs(60),
        })
        .await
        .expect("dht command channel should still be open");

    // Real network propagation delay for the put to actually land before
    // node B's get_record can find it.
    tokio::time::sleep(Duration::from_secs(1)).await;

    let found = interest::lookup(&node_b.commands, scope).await;
    assert_eq!(
        found,
        vec!["http://node-a.test".to_string()],
        "node B's lookup should find node A's real, live-put interest record"
    );

    // A lookup for a channel nobody ever registered interest in finds
    // nothing — the negative case, proving this isn't just always
    // returning *something*.
    let never_registered =
        interest::lookup(&node_b.commands, InterestScope::Channel(Uuid::new_v4())).await;
    assert!(never_registered.is_empty());
}
