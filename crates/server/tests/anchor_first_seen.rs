//! An entry's anchor is the first-seen time of its cosignatures: a refresh never moves it, an older
//! cosignature arriving later lowers it, and stores that saw the same cosignatures in any order agree.
//! Real Postgres. Gated `--ignored`.

use std::sync::Arc;

use avalon_chain::PostgresSettlementProvider;
use avalon_indexer::head_attestor::{HeadAttestor, SqlHeadAttestor};
use avalon_protocol::sth;
use avalon_protocol::witness::sign_witness_cosignature;
use ed25519_dalek::SigningKey;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

const SHARD: &str = "s";

async fn pool() -> PgPool {
    avalon_devenv::load();
    PgPoolOptions::new()
        .connect(&std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"))
        .await
        .expect("Postgres must be reachable")
}

/// A network with one mirrored entry (of `event_id`) covered by a head of size 1.
async fn network(pool: &PgPool, event_id: Uuid) -> (String, String) {
    let network = format!("anchor-{}", Uuid::new_v4());
    let root = "ab".repeat(32);
    sqlx::query(
        "INSERT INTO mirrored_entries (source_url, network_id, shard_id, seq, event_id, kind, \
         issuer, subject, payload, payload_hash, event_timestamp, version, prev_hash, entry_hash, \
         batch_id, verified_tree_size, layout_version, rules_version, hash_algo, extensions) \
         VALUES ('http://peer.invalid', $1, $2, 1, $3, 'profile.updated', 'a', 'b', '{}', \
                 repeat('0', 64), now(), 1, 'p', $4, $5, 1, 1, 1, 1, '\\x0000'::bytea)",
    )
    .bind(&network)
    .bind(SHARD)
    .bind(event_id)
    .bind(format!("entry-{event_id}"))
    .bind(Uuid::new_v4())
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO observed_sths (source_url, network_id, shard_id, tree_size, root_hash, \
         signature, signing_key_id, created_at, observed_at, layout_version, rules_version, \
         hash_algo, extensions) \
         VALUES ('http://peer.invalid', $1, $2, 1, $3, repeat('0', 128), 'k', now(), now(), 1, 1, 1, \
                 '\\x0000'::bytea)",
    )
    .bind(&network)
    .bind(SHARD)
    .bind(&root)
    .execute(pool)
    .await
    .unwrap();
    (network, root)
}

async fn cosign(
    chain: &PostgresSettlementProvider,
    key: &SigningKey,
    network: &str,
    root: &str,
    at: OffsetDateTime,
) {
    let created = OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_700_000_000);
    let head = sth::sign_tree_head(key, "author-key", 1, root, network, created).unwrap();
    let id = hex::encode(key.verifying_key().to_bytes());
    let cosig = sign_witness_cosignature(key, &id, &head, at).unwrap();
    chain
        .store_witness_cosignature(SHARD, &cosig)
        .await
        .unwrap();
}

async fn times(
    pool: &PgPool,
    network: &str,
    event_id: Uuid,
    known: Vec<String>,
) -> (Option<OffsetDateTime>, Option<OffsetDateTime>) {
    let attestor = SqlHeadAttestor::new(
        network,
        "own",
        Arc::new(|| 1),
        Arc::new(move || known.clone()),
    );
    let copies = attestor
        .by_event_id(&mut pool.acquire().await.unwrap(), event_id)
        .await
        .unwrap();
    (copies[0].anchor_at, copies[0].reference_at)
}

fn id_of(key: &SigningKey) -> String {
    hex::encode(key.verifying_key().to_bytes())
}

fn close(a: Option<OffsetDateTime>, b: OffsetDateTime) -> bool {
    a.is_some_and(|a| (a - b).abs() < Duration::milliseconds(1500))
}

#[tokio::test]
#[ignore = "needs Postgres"]
async fn a_refresh_does_not_move_the_anchor() {
    let pool = pool().await;
    let event_id = Uuid::new_v4();
    let (network, root) = network(&pool, event_id).await;
    let chain = PostgresSettlementProvider::new_core_shard(pool.clone(), network.clone());
    let key = SigningKey::generate(&mut rand::rng());
    let first = OffsetDateTime::now_utc() - Duration::hours(3);
    cosign(&chain, &key, &network, &root, first).await;
    cosign(&chain, &key, &network, &root, first + Duration::hours(2)).await;
    let (anchor, reference) = times(&pool, &network, event_id, vec![id_of(&key)]).await;
    assert!(
        close(anchor, first),
        "anchor drifted with the refresh: {anchor:?}"
    );
    assert!(
        close(reference, first + Duration::hours(2)),
        "{reference:?}"
    );
}

#[tokio::test]
#[ignore = "needs Postgres"]
async fn an_older_cosignature_arriving_later_lowers_the_anchor() {
    let pool = pool().await;
    let event_id = Uuid::new_v4();
    let (network, root) = network(&pool, event_id).await;
    let chain = PostgresSettlementProvider::new_core_shard(pool.clone(), network.clone());
    let key = SigningKey::generate(&mut rand::rng());
    let now = OffsetDateTime::now_utc();
    cosign(&chain, &key, &network, &root, now - Duration::hours(2)).await;
    cosign(&chain, &key, &network, &root, now - Duration::hours(5)).await;
    let (anchor, reference) = times(&pool, &network, event_id, vec![id_of(&key)]).await;
    assert!(close(anchor, now - Duration::hours(5)), "{anchor:?}");
    assert!(close(reference, now - Duration::hours(2)), "{reference:?}");
}

#[tokio::test]
#[ignore = "needs Postgres"]
async fn stores_that_saw_the_same_cosignatures_in_any_order_agree() {
    let pool = pool().await;
    let key = SigningKey::generate(&mut rand::rng());
    let now = OffsetDateTime::now_utc();
    let stamps = [
        now - Duration::hours(2),
        now - Duration::hours(5),
        now - Duration::hours(1),
    ];
    let mut seen = Vec::new();
    for order in [[0, 1, 2], [2, 1, 0], [1, 2, 0], [2, 0, 1]] {
        let event_id = Uuid::new_v4();
        let (network, root) = network(&pool, event_id).await;
        let chain = PostgresSettlementProvider::new_core_shard(pool.clone(), network.clone());
        for i in order {
            cosign(&chain, &key, &network, &root, stamps[i]).await;
        }
        seen.push(times(&pool, &network, event_id, vec![id_of(&key)]).await);
    }
    for pair in seen.windows(2) {
        assert_eq!(pair[0], pair[1]);
    }
    assert!(close(seen[0].0, now - Duration::hours(5)));
}
