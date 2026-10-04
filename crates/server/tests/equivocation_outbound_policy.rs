//! Conflict confirmation fetches only through the outbound policy. Alone in its own process
//! because it sets `AVALON_ALLOW_PRIVATE_PEERS`, which the clients read when they are built.

use avalon_chain::PostgresSettlementProvider;
use avalon_server::equivocation::confirm_and_record;
use avalon_server::known_list::{KnownListConfig, KnownListHandle};
use avalon_server::nodes::{HeadConflict, HeadGossipTracker, PeerTable};
use sqlx::postgres::PgPoolOptions;
use wiremock::MockServer;

async fn confirm(source: &str) {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://user:pass@127.0.0.1:1/none")
        .expect("a lazy pool never connects while it is built");
    let chain = PostgresSettlementProvider::new(pool, "avalon-test");
    let conflict = HeadConflict {
        shard_id: "core".into(),
        tree_size: 5,
        root_hash_a: hex::encode([1u8; 32]),
        source_a: source.into(),
        root_hash_b: hex::encode([2u8; 32]),
        source_b: source.into(),
    };
    confirm_and_record(
        &chain,
        &HeadGossipTracker::new(),
        &PeerTable::new(),
        &KnownListHandle::load_or_new(KnownListConfig::default(), None),
        conflict,
    )
    .await;
}

#[tokio::test]
async fn a_conflict_source_is_fetched_only_when_the_policy_allows_its_address() {
    let target = MockServer::start().await;

    unsafe { std::env::remove_var("AVALON_ALLOW_PRIVATE_PEERS") };
    confirm(&target.uri()).await;
    assert!(
        target.received_requests().await.unwrap().is_empty(),
        "a loopback source was fetched under the default policy"
    );

    unsafe { std::env::set_var("AVALON_ALLOW_PRIVATE_PEERS", "true") };
    confirm(&format!("{}/?x=", target.uri())).await;
    confirm(&format!("http://user:pw@{}", target.address())).await;
    assert!(
        target.received_requests().await.unwrap().is_empty(),
        "a source with a query or credentials was fetched"
    );

    confirm(&target.uri()).await;
    assert!(
        !target.received_requests().await.unwrap().is_empty(),
        "private peers allowed: a well-formed source must still be fetched"
    );
}
