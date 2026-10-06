//! Stored tree heads and cosignatures this node cannot read (an envelope above its range) are
//! skipped in lists and give the typed result on a single read, never a generic failure.
//! Gated `--ignored`, like the other Postgres tests.

use avalon_chain::{PostgresSettlementProvider, SettlementError};
use avalon_protocol::signing_bytes::{tags, Envelope, VersionKind};
use avalon_protocol::sth::sign_tree_head;
use avalon_protocol::witness::sign_witness_cosignature;
use ed25519_dalek::SigningKey;
use rand::RngExt;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

async fn test_pool() -> PgPool {
    avalon_devenv::load();
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new().connect(&url).await.expect("connect")
}

fn fresh_size() -> i64 {
    rand::rng().random_range(1_000_000_000..2_000_000_000)
}

async fn insert_head(pool: &PgPool, network: &str, size: i64, rules: i32) {
    sqlx::query(
        "INSERT INTO signed_tree_heads (tree_size, root_hash, network_id, signing_key_id, signature, \
         created_at, layout_version, rules_version, hash_algo, extensions) \
         VALUES ($1, $2, $3, 'k', $4, now(), 1, $5, 1, '\\x0000'::bytea)",
    )
    .bind(size)
    .bind("ab".repeat(32))
    .bind(network)
    .bind("00".repeat(64))
    .bind(rules)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
#[ignore]
async fn an_unreadable_stored_head_is_skipped_in_lists_and_typed_when_read_alone() {
    let pool = test_pool().await;
    let network = format!("avalon-test-unreadable-{}", Uuid::new_v4());
    let chain = PostgresSettlementProvider::new_core_shard(pool.clone(), network.clone());
    let (good, bad) = (fresh_size(), fresh_size() + 2_000_000_000);
    insert_head(&pool, &network, good, 1).await;
    insert_head(&pool, &network, bad, 2).await;

    let heads = chain.list_signed_tree_heads().await.unwrap();
    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0].tree_size, good);

    let err = chain.signed_tree_head_at(bad).await.unwrap_err();
    assert!(matches!(
        err,
        SettlementError::NeedsNewerVersion {
            what: VersionKind::Rules,
            required: 2
        }
    ));
    assert!(chain.signed_tree_head_at(good).await.unwrap().is_some());
}

#[tokio::test]
#[ignore]
async fn an_unreadable_stored_cosignature_is_skipped_and_the_rest_are_kept() {
    let pool = test_pool().await;
    let network = format!("avalon-test-unreadable-cos-{}", Uuid::new_v4());
    let chain = PostgresSettlementProvider::new_core_shard(pool.clone(), network.clone());
    let size = fresh_size();
    let author = SigningKey::from_bytes(&[3u8; 32]);
    let head = sign_tree_head(
        &author,
        "k",
        size,
        &"cd".repeat(32),
        &network,
        OffsetDateTime::UNIX_EPOCH,
    )
    .unwrap();
    for (i, rules) in [(0u8, 1), (1u8, 2)] {
        let key = SigningKey::from_bytes(&[20 + i; 32]);
        let id = hex::encode(key.verifying_key().to_bytes());
        let cosig = sign_witness_cosignature(&key, &id, &head, OffsetDateTime::UNIX_EPOCH).unwrap();
        chain
            .store_witness_cosignature("core", &cosig)
            .await
            .unwrap();
        if rules == 2 {
            sqlx::query(
                "UPDATE witness_cosignatures SET rules_version = 2 \
                 WHERE network_id = $1 AND tree_size = $2 AND witness_key_id = $3",
            )
            .bind(&network)
            .bind(size)
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
        }
    }
    let listed = chain
        .list_witness_cosignatures(&network, "core", size)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].envelope, Envelope::current(tags::WITNESS_COSIGN));
}
