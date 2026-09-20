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
//!
//! `redis_fast_path_answers_a_lookup_even_when_the_dht_channel_is_dead`
//! (issue #585) additionally needs a real, reachable Redis —
//! `AVALON_REDIS_URL=redis://<host>:<port> cargo test -p avalon-server
//! --test interest_dht -- --ignored redis_fast_path`. Panics with a clear
//! message rather than silently skipping if unset, matching this file's
//! own `require_peer_server_url`-style convention elsewhere in this crate.

use std::time::Duration;

use avalon_server::dht::{self, DhtConfig};
use avalon_server::interest::{self, InterestRegistry, InterestScope};
use avalon_server::nodes::{PeerInfo, PeerTable};
use uuid::Uuid;

/// Short enough that the test doesn't take minutes, long enough that a
/// couple of scan ticks reliably happen within the test's own wait budget.
const TEST_SCAN_INTERVAL: Duration = Duration::from_millis(500);

fn test_config() -> DhtConfig {
    test_config_for_network("avalon-dev-local")
}

fn test_config_for_network(network_id: &str) -> DhtConfig {
    DhtConfig {
        identity: libp2p::identity::Keypair::generate_ed25519(),
        network_id: network_id.to_string(),
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

    let (registry, _newly_active) = InterestRegistry::new();
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

    let found = interest::lookup(&node_b.commands, scope, None).await;
    assert_eq!(
        found,
        vec!["http://node-a.test".to_string()],
        "node B's lookup should find node A's real, live-put interest record"
    );

    // A lookup for a channel nobody ever registered interest in finds
    // nothing — the negative case, proving this isn't just always
    // returning *something*.
    let never_registered = interest::lookup(
        &node_b.commands,
        InterestScope::Channel(Uuid::new_v4()),
        None,
    )
    .await;
    assert!(never_registered.is_empty());
}

/// Epic #623, issue #635's own live DHT-level acceptance test: an
/// `InterestScope::Identity` record round-trips through a real two-swarm
/// `put`/`get` exactly like a `Channel` scope already does above — proving
/// the new scope kind actually works at the DHT layer in isolation, before
/// `crates/server/tests/identity_locator.rs` exercises the full
/// worker+HTTP path against real Postgres.
#[tokio::test]
#[ignore]
async fn a_registered_identity_locator_scope_is_found_by_a_lookup_from_a_different_node() {
    let node_a_peers = PeerTable::new();
    let node_b_peers = PeerTable::new();

    let node_a = dht::start(node_a_peers.clone(), test_config()).await;
    let node_b = dht::start(node_b_peers.clone(), test_config()).await;

    cross_register(&node_b_peers, &node_a, "http://node-a.test");
    cross_register(&node_a_peers, &node_b, "http://node-b.test");

    tokio::time::sleep(TEST_SCAN_INTERVAL * 6).await;

    let identity_id = Uuid::new_v4();
    let scope = InterestScope::Identity(identity_id);
    node_a
        .commands
        .send(avalon_server::dht::DhtCommand::PutRecord {
            key: scope.dht_key(),
            value: b"http://node-a.test".to_vec(),
            ttl: Duration::from_secs(60),
        })
        .await
        .expect("dht command channel should still be open");

    tokio::time::sleep(Duration::from_secs(1)).await;

    let found = interest::lookup(&node_b.commands, scope, None).await;
    assert_eq!(
        found,
        vec!["http://node-a.test".to_string()],
        "node B's lookup should find node A's real, live-put identity-locator record"
    );

    let never_registered = interest::lookup(
        &node_b.commands,
        InterestScope::Identity(Uuid::new_v4()),
        None,
    )
    .await;
    assert!(never_registered.is_empty());
}

/// Issue #608's own live acceptance test: two real swarms configured for
/// *different* `network_id`s never see each other's interest records, even
/// when each is explicitly told the other's real DHT identity/address (the
/// same `cross_register` setup the same-network test above uses) — proving
/// the Kademlia protocol-id mismatch actually prevents the RPC exchange
/// itself, not just that nobody happened to dial. `PutRecord`'s own local
/// store write always "succeeds" (it's local-first), so the negative
/// assertion has to be on the *other* node's lookup finding nothing, not on
/// the put failing.
#[tokio::test]
#[ignore]
async fn nodes_on_different_networks_never_see_each_others_interest_records() {
    let node_a_peers = PeerTable::new();
    let node_b_peers = PeerTable::new();

    let node_a = dht::start(
        node_a_peers.clone(),
        test_config_for_network("avalon-alpha"),
    )
    .await;
    let node_b = dht::start(node_b_peers.clone(), test_config_for_network("avalon-beta")).await;

    cross_register(&node_b_peers, &node_a, "http://node-a.test");
    cross_register(&node_a_peers, &node_b, "http://node-b.test");

    // Give both sides' bootstrap-scan workers a real chance to attempt a
    // dial — the point of this test is that even a dial attempt (or a
    // connection that gets as far as `identify`) never yields a working
    // Kademlia RPC path, not that nothing ever tries to connect at all.
    tokio::time::sleep(TEST_SCAN_INTERVAL * 6).await;

    let channel_id = Uuid::new_v4();
    let scope = InterestScope::Channel(channel_id);
    node_a
        .commands
        .send(avalon_server::dht::DhtCommand::PutRecord {
            key: scope.dht_key(),
            value: b"http://node-a.test".to_vec(),
            ttl: Duration::from_secs(60),
        })
        .await
        .expect("dht command channel should still be open");

    tokio::time::sleep(Duration::from_secs(1)).await;

    let found = interest::lookup(&node_b.commands, scope, None).await;
    assert!(
        found.is_empty(),
        "node B (network avalon-beta) should never be able to resolve a record node A \
         (network avalon-alpha) put — differently-networked swarms must not interoperate at \
         the DHT layer at all, per issue #608"
    );
}

/// Issue #585's own live acceptance test, against a real Redis
/// (`AVALON_REDIS_URL`, required — panics with a clear message if unset
/// rather than silently skipping). Proves the fast path actually answers
/// on its own, not just alongside a working DHT: `run_worker` registers
/// real interest (writing to both Redis and a real DHT swarm, as
/// production does), but the *lookup* half is deliberately handed a dead
/// DHT command channel (its receiver dropped before the call) — any
/// attempt to actually fall through to the DHT would find nobody home. If
/// this still finds the right answer, Redis alone must have supplied it.
#[tokio::test]
#[ignore]
async fn redis_fast_path_answers_a_lookup_even_when_the_dht_channel_is_dead() {
    let redis_fast_path = interest::RedisFastPath::from_env()
        .await
        .expect("AVALON_REDIS_URL must be set (and reachable) to run this test");

    let node_peers = PeerTable::new();
    let node = dht::start(node_peers, test_config()).await;

    let (registry, newly_active) = InterestRegistry::new();
    tokio::spawn(interest::run_worker(
        registry.clone(),
        newly_active,
        node.commands.clone(),
        Some("http://node-a.test".to_string()),
        Some(redis_fast_path.clone()),
    ));

    let scope = InterestScope::Channel(Uuid::new_v4());
    let _guard = registry.register(scope);

    // Real network round trip for run_worker's immediate-on-registration
    // put (both the Redis SADD and the DHT put_record) to actually land.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let (dead_commands, dead_receiver) = tokio::sync::mpsc::channel(1);
    drop(dead_receiver);

    let found = interest::lookup(&dead_commands, scope, Some(&redis_fast_path)).await;
    assert_eq!(
        found,
        vec!["http://node-a.test".to_string()],
        "the Redis fast path should answer on its own even though the DHT command \
         channel handed to this lookup is already dead"
    );
}
