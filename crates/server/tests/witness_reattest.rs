//! A witness's cosignature over its unchanged head is refreshed in place by the re-attestation
//! pass, never replaced by a conflicting one. Real Postgres. Gated `--ignored`.

use avalon_chain::mirror;
use avalon_chain::PostgresSettlementProvider;
use avalon_protocol::witness::{sign_witness_cosignature, verify_witness_cosignature};
use avalon_server::nodes::HeadGossipTracker;
use avalon_server::witness_cosign::{reattest_once, WitnessCosignConfig};
use ed25519_dalek::SigningKey;
use sqlx::postgres::PgPoolOptions;
use time::OffsetDateTime;
use uuid::Uuid;

#[tokio::test]
#[ignore = "needs Postgres"]
async fn reattestation_refreshes_the_same_head_and_never_a_different_one() {
    avalon_devenv::load();
    let pool = PgPoolOptions::new()
        .connect(&std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"))
        .await
        .expect("Postgres must be reachable");

    let network_id = format!("reattest-{}", Uuid::new_v4());
    let shard = "core";
    let chain = PostgresSettlementProvider::new(pool.clone(), network_id.clone());
    let key = SigningKey::generate(&mut rand::rng());
    let key_id = hex::encode(key.verifying_key().to_bytes());
    let root = "ab".repeat(32);
    let author_created_at = OffsetDateTime::now_utc() - time::Duration::hours(1);
    let first_seen = OffsetDateTime::now_utc() - time::Duration::minutes(30);

    let original = sign_witness_cosignature(
        &key,
        &key_id,
        4,
        &root,
        &network_id,
        author_created_at,
        first_seen,
    );
    chain
        .store_witness_cosignature(shard, &original)
        .await
        .unwrap();
    mirror::record_witness_checkpoint(&pool, &network_id, shard, 4, &root, &key_id)
        .await
        .unwrap();

    let config = WitnessCosignConfig::new(key.clone(), key_id.clone());
    let now = OffsetDateTime::now_utc();
    let refreshed = reattest_once(&chain, &pool, &config, &HeadGossipTracker::new(), now).await;
    assert!(refreshed >= 1);

    let stored = chain
        .list_witness_cosignatures(&network_id, shard, 4)
        .await
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert!(stored[0].observed_at > first_seen);
    assert_eq!(stored[0].root_hash, root);
    assert!(verify_witness_cosignature(&key.verifying_key(), &stored[0]));

    let other_root = "cd".repeat(32);
    let conflicting = sign_witness_cosignature(
        &key,
        &key_id,
        4,
        &other_root,
        &network_id,
        author_created_at,
        now + time::Duration::minutes(5),
    );
    assert!(!chain
        .refresh_witness_cosignature(shard, &conflicting)
        .await
        .unwrap());
    let stored = chain
        .list_witness_cosignatures(&network_id, shard, 4)
        .await
        .unwrap();
    assert_eq!(stored[0].root_hash, root);
}
