//! Live tests (`--ignored`, migrated Postgres) for the head attestor: an entry's time is the
//! witness-signed quorum time of the cosigned heads covering it, never a shard's own head time.

use std::sync::Arc;

use avalon_indexer::head_attestor::{HeadAttestor, SqlHeadAttestor};
use avalon_protocol::recovery_rules::delay_elapsed;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new().connect(&url).await.unwrap()
}

fn hours(h: i64) -> Duration {
    Duration::hours(h)
}

/// One mirrored entry of `event_id` on `shard`, at seq 1.
async fn entry(pool: &PgPool, network: &str, shard: &str, event_id: Uuid) {
    sqlx::query(
        "INSERT INTO mirrored_entries (source_url, network_id, shard_id, seq, event_id, kind, \
         issuer, subject, payload, payload_hash, event_timestamp, version, prev_hash, entry_hash, \
         batch_id, verified_tree_size, layout_version, rules_version, hash_algo, extensions) \
         VALUES ('http://peer.invalid', $1, $2, 1, $3, 'profile.updated', 'a', 'b', '{}', \
                 repeat('0', 64), now(), 1, 'p', $4, $5, 1, 1, 1, 1, '\\x0000'::bytea)",
    )
    .bind(network)
    .bind(shard)
    .bind(event_id)
    .bind(format!("entry-{shard}-{event_id}"))
    .bind(Uuid::new_v4())
    .execute(pool)
    .await
    .unwrap();
}

/// A head of `shard` with `size` leaves, stamped `created_at` by the shard, cosigned by witnesses
/// at the given times.
async fn head(
    pool: &PgPool,
    network: &str,
    shard: &str,
    size: i64,
    created_at: OffsetDateTime,
    cosigned_at: &[OffsetDateTime],
) {
    let named: Vec<(String, OffsetDateTime)> = cosigned_at
        .iter()
        .enumerate()
        .map(|(i, at)| (format!("witness-{i}"), *at))
        .collect();
    head_by(pool, network, shard, size, created_at, &named).await;
}

/// Like [`head`], with the cosigning witnesses named.
async fn head_by(
    pool: &PgPool,
    network: &str,
    shard: &str,
    size: i64,
    created_at: OffsetDateTime,
    cosigned_at: &[(String, OffsetDateTime)],
) {
    let root = format!("root-{shard}-{size}");
    sqlx::query(
        "INSERT INTO observed_sths (source_url, network_id, shard_id, tree_size, root_hash, \
         signature, signing_key_id, created_at, observed_at, layout_version, rules_version, \
         hash_algo, extensions) \
         VALUES ('http://peer.invalid', $1, $2, $3, $4, repeat('0', 128), 'k', $5, now(), 1, 1, 1, \
                 '\\x0000'::bytea)",
    )
    .bind(network)
    .bind(shard)
    .bind(size)
    .bind(&root)
    .bind(created_at)
    .execute(pool)
    .await
    .unwrap();
    for (witness, at) in cosigned_at {
        sqlx::query(
            "INSERT INTO witness_cosignatures (network_id, shard_id, tree_size, witness_key_id, \
             root_hash, author_created_at, author_key_id, author_signature, observed_at, signature, \
             layout_version, rules_version, hash_algo, extensions, first_seen_at) \
             VALUES ($1, $2, $3, $4, $5, $6, 'k', repeat('0', 128), $7, repeat('0', 128), 1, 1, 1, \
                     '\\x0000'::bytea, $7)",
        )
        .bind(network)
        .bind(shard)
        .bind(size)
        .bind(witness)
        .bind(&root)
        .bind(created_at)
        .bind(*at)
        .execute(pool)
        .await
        .unwrap();
    }
}

fn attestor(network: &str, quorum: usize) -> SqlHeadAttestor {
    let known: Vec<String> = (0..8).map(|i| format!("witness-{i}")).collect();
    SqlHeadAttestor::new(
        network,
        "own",
        Arc::new(move || quorum),
        Arc::new(move || known.clone()),
    )
}

#[tokio::test]
#[ignore]
async fn a_backdated_head_does_not_make_an_entry_old_enough() {
    let pool = pool().await;
    let network = Uuid::new_v4().to_string();
    let event_id = Uuid::new_v4();
    let now = OffsetDateTime::now_utc();
    entry(&pool, &network, "hostile", event_id).await;
    // The hostile shard stamps its head 100 hours back; witnesses cosigned it only just now.
    head(
        &pool,
        &network,
        "hostile",
        1,
        now - hours(100),
        &[now - hours(2), now - hours(1), now],
    )
    .await;
    // A later head, cosigned now, sets the reference time.
    head(&pool, &network, "hostile", 2, now, &[now, now]).await;

    let copies = attestor(&network, 2)
        .by_event_id(&mut pool.acquire().await.unwrap(), event_id)
        .await
        .unwrap();
    let [copy] = copies.as_slice() else {
        panic!("one copy expected")
    };
    let (anchor, reference) = (copy.anchor_at.unwrap(), copy.reference_at.unwrap());
    assert!(
        !delay_elapsed(anchor, reference),
        "the delay passed on the shard's own head time"
    );
}

#[tokio::test]
#[ignore]
async fn the_quorum_time_is_the_quorum_th_earliest_cosignature() {
    let pool = pool().await;
    let network = Uuid::new_v4().to_string();
    let event_id = Uuid::new_v4();
    let now = OffsetDateTime::now_utc();
    entry(&pool, &network, "s", event_id).await;
    head(
        &pool,
        &network,
        "s",
        1,
        now - hours(10),
        &[
            now - hours(5),
            now - hours(4),
            now - hours(3),
            now - hours(1),
        ],
    )
    .await;
    let copies = attestor(&network, 3)
        .by_event_id(&mut pool.acquire().await.unwrap(), event_id)
        .await
        .unwrap();
    let anchor = copies[0].anchor_at.unwrap();
    // Three witnesses had cosigned by the third earliest time.
    assert!(
        (anchor - (now - hours(3))).abs() < Duration::seconds(1),
        "{anchor}"
    );
}

#[tokio::test]
#[ignore]
async fn witnesses_outside_the_known_list_cannot_complete_a_quorum() {
    let pool = pool().await;
    let network = Uuid::new_v4().to_string();
    let event_id = Uuid::new_v4();
    let now = OffsetDateTime::now_utc();
    entry(&pool, &network, "s", event_id).await;
    // Two known witnesses and six announced ones that are not on the known list.
    let mut cosigned: Vec<(String, OffsetDateTime)> = (0..2)
        .map(|i| (format!("witness-{i}"), now - hours(5)))
        .collect();
    cosigned.extend((0..6).map(|i| (format!("sybil-{i}"), now - hours(5))));
    head_by(&pool, &network, "s", 1, now - hours(6), &cosigned).await;
    let copies = attestor(&network, 3)
        .by_event_id(&mut pool.acquire().await.unwrap(), event_id)
        .await
        .unwrap();
    assert_eq!(
        copies[0].anchor_at, None,
        "non-members completed the quorum"
    );
}

#[tokio::test]
#[ignore]
async fn a_cosignature_dated_far_ahead_cannot_push_the_reference_time() {
    let pool = pool().await;
    let network = Uuid::new_v4().to_string();
    let event_id = Uuid::new_v4();
    let now = OffsetDateTime::now_utc();
    entry(&pool, &network, "s", event_id).await;
    head(
        &pool,
        &network,
        "s",
        1,
        now - hours(100),
        &[now - hours(60), now - hours(60)],
    )
    .await;
    // A later head whose cosignatures are dated a year ahead.
    head(
        &pool,
        &network,
        "s",
        2,
        now,
        &[now + hours(24 * 365), now + hours(24 * 365)],
    )
    .await;
    let copies = attestor(&network, 2)
        .by_event_id(&mut pool.acquire().await.unwrap(), event_id)
        .await
        .unwrap();
    let reference = copies[0].reference_at.unwrap();
    assert!(
        reference < now + Duration::minutes(2),
        "reference pushed to {reference}"
    );
}

#[tokio::test]
#[ignore]
async fn an_entry_hash_held_on_several_shards_is_timed_by_the_earliest_copy() {
    let pool = pool().await;
    let network = Uuid::new_v4().to_string();
    let now = OffsetDateTime::now_utc();
    let hash = format!("shared-{}", Uuid::new_v4());
    for shard in ["a-late", "z-early"] {
        sqlx::query(
            "INSERT INTO mirrored_entries (source_url, network_id, shard_id, seq, event_id, kind, \
             issuer, subject, payload, payload_hash, event_timestamp, version, prev_hash, entry_hash, \
             batch_id, verified_tree_size, layout_version, rules_version, hash_algo, extensions) \
             VALUES ('http://peer.invalid', $1, $2, 1, $3, 'profile.updated', 'a', 'b', '{}', \
                     repeat('0', 64), now(), 1, 'p', $4, $5, 1, 1, 1, 1, '\\x0000'::bytea)",
        )
        .bind(&network)
        .bind(shard)
        .bind(Uuid::new_v4())
        .bind(&hash)
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();
    }
    // The "late" copy is listed first and was cosigned a day ago; the "early" one 50 hours ago.
    head(
        &pool,
        &network,
        "a-late",
        1,
        now - hours(30),
        &[now - hours(24), now - hours(24)],
    )
    .await;
    head(
        &pool,
        &network,
        "z-early",
        1,
        now - hours(60),
        &[now - hours(50), now - hours(50)],
    )
    .await;
    let found = attestor(&network, 2)
        .by_entry_hash(&mut pool.acquire().await.unwrap(), &hash)
        .await
        .unwrap()
        .unwrap();
    let anchor = found.anchor_at.unwrap();
    assert!(
        (anchor - (now - hours(50))).abs() < Duration::seconds(2),
        "{anchor}"
    );
}
