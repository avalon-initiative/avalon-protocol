//! A witness's cosignature over its unchanged head is refreshed in place by the re-attestation
//! pass, never replaced by a conflicting one. Real Postgres. Gated `--ignored`.
//! A witness key change at an unchanged head rotates the stored cosignature and the
//! checkpoint's key id.

use avalon_chain::mirror;
use avalon_chain::PostgresSettlementProvider;
use avalon_protocol::sth;
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
    let chain = PostgresSettlementProvider::new_core_shard(pool.clone(), network_id.clone());
    let key = SigningKey::generate(&mut rand::rng());
    let key_id = hex::encode(key.verifying_key().to_bytes());
    let root = "ab".repeat(32);
    let author_created_at = OffsetDateTime::now_utc() - time::Duration::hours(1);
    let first_seen = OffsetDateTime::now_utc() - time::Duration::minutes(30);

    let head = |root: &str| {
        sth::sign_tree_head(&key, "author-key", 4, root, &network_id, author_created_at).unwrap()
    };
    let original = sign_witness_cosignature(&key, &key_id, &head(&root), first_seen).unwrap();
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
        &head(&other_root),
        now + time::Duration::minutes(5),
    )
    .unwrap();
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

#[tokio::test]
#[ignore = "needs Postgres"]
async fn reattestation_rotates_the_witness_key_then_refreshes_the_new_key() {
    avalon_devenv::load();
    let pool = PgPoolOptions::new()
        .connect(&std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"))
        .await
        .expect("Postgres must be reachable");

    let network_id = format!("reattest-rotation-{}", Uuid::new_v4());
    let shard = "core";
    let chain = PostgresSettlementProvider::new_core_shard(pool.clone(), network_id.clone());
    let old_key = SigningKey::generate(&mut rand::rng());
    let old_key_id = hex::encode(old_key.verifying_key().to_bytes());
    let new_key = SigningKey::generate(&mut rand::rng());
    let new_key_id = hex::encode(new_key.verifying_key().to_bytes());
    let root = "ef".repeat(32);
    let now = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap();
    let first_seen = now - time::Duration::minutes(30);
    let head = sth::sign_tree_head(
        &old_key,
        "author-key",
        19,
        &root,
        &network_id,
        now - time::Duration::hours(1),
    )
    .unwrap();
    let original = sign_witness_cosignature(&old_key, &old_key_id, &head, first_seen).unwrap();
    chain
        .store_witness_cosignature(shard, &original)
        .await
        .unwrap();
    mirror::record_witness_checkpoint(&pool, &network_id, shard, 19, &root, &old_key_id)
        .await
        .unwrap();

    let config = WitnessCosignConfig::new(new_key.clone(), new_key_id.clone());
    let gossip = HeadGossipTracker::new();
    reattest_once(&chain, &pool, &config, &gossip, now).await;

    let stored = chain
        .list_witness_cosignatures(&network_id, shard, 19)
        .await
        .unwrap();
    let rotated = stored
        .iter()
        .find(|cosig| cosig.witness_key_id == new_key_id)
        .expect("rotation must store a cosignature under the new witness key")
        .clone();
    assert_eq!(rotated.root_hash, root);
    assert!(verify_witness_cosignature(
        &new_key.verifying_key(),
        &rotated
    ));
    assert_eq!(rotated.observed_at, now);
    let old = stored
        .iter()
        .find(|cosig| cosig.witness_key_id == old_key_id)
        .expect("rotation must preserve the old witness's cosignature");
    assert_eq!(old.signature, original.signature);
    assert_eq!(old.observed_at, first_seen);
    assert_eq!(stored.len(), 2);

    let checkpoint = mirror::witness_checkpoint_for(&pool, &network_id, shard)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint.witness_key_id, new_key_id);
    assert_eq!(checkpoint.tree_size, 19);
    assert_eq!(checkpoint.root_hash, root);

    let later = now + time::Duration::minutes(5);
    reattest_once(&chain, &pool, &config, &gossip, later).await;
    let stored = chain
        .list_witness_cosignatures(&network_id, shard, 19)
        .await
        .unwrap();
    let refreshed = stored
        .iter()
        .find(|cosig| cosig.witness_key_id == new_key_id)
        .unwrap();
    assert!(refreshed.observed_at > rotated.observed_at);
    assert_eq!(refreshed.observed_at, later);
    assert_eq!(refreshed.root_hash, root);
    assert!(verify_witness_cosignature(
        &new_key.verifying_key(),
        refreshed
    ));
    assert_eq!(stored.len(), 2);
}
