//! Exercises issue #520's new mirror read-helpers against real Postgres:
//! `mirrored_entries_since`, `mirrored_entry_hashes_up_to`,
//! `mirrored_leaf_index_for_seq`, and `observed_sth_matching_root` — the
//! building blocks `crates/server/src/settlement.rs`'s mirror-backed
//! fallback handlers assemble into a served response. Gated `--ignored`,
//! same convention as `crates/chain/tests/mirror_recovery.rs`.
//!
//! What's *not* covered here: the HTTP-route-level branch in
//! `settlement.rs` (`state.chain.entry_count() == 0` deciding whether a
//! request is served from `state.chain` or from these mirror tables). This
//! sandbox's single, long-lived `avalon-server` always has authored
//! history of its own (`entry_count() > 0`), so that branch can never
//! actually be reached against it — the same "needs two real
//! `avalon-server` processes" limitation `crates/server/tests/mirror_watcher.rs`'s
//! own module doc already calls out. That branch is exercised instead
//! against a real second node in the two-node LAN sandbox (see
//! `docs/architecture/nodes.md`'s "Today in the repo" section).

use avalon_chain::merkle;
use avalon_chain::mirror::{self, MirroredEntry, ObservedSth};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

async fn test_pool() -> PgPool {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

fn unique_network_id(label: &str) -> String {
    format!("avalon-test-{label}-{}", Uuid::new_v4())
}

fn mirrored_entry(network_id: &str, seq: i64, entry_hash: &str, subject: &str) -> MirroredEntry {
    MirroredEntry {
        source_url: "http://peer".to_string(),
        network_id: network_id.to_string(),
        seq,
        event_id: Uuid::new_v4(),
        kind: "identity.created".to_string(),
        issuer: "identity:11111111-1111-1111-1111-111111111111:self:created".to_string(),
        subject: subject.to_string(),
        payload: Some(serde_json::json!({"note": "mirror serving test"})),
        event_timestamp: OffsetDateTime::UNIX_EPOCH,
        version: 1,
        prev_hash: "aa".repeat(32),
        entry_hash: entry_hash.to_string(),
        batch_id: Uuid::new_v4(),
        verified_tree_size: seq,
    }
}

fn leaf_hash_hex(n: u8) -> String {
    hex::encode([n; 32])
}

/// `mirrored_entries_since` returns entries strictly after `since_seq`,
/// oldest first, and respects a `subject` filter exactly like the
/// authority-side `list_entries_since_for_subject` it mirrors.
#[tokio::test]
#[ignore]
async fn mirrored_entries_since_paginates_and_filters_by_subject() {
    let pool = test_pool().await;
    let network_id = unique_network_id("entries-since");
    let subject_a = "identity:11111111-1111-1111-1111-111111111111:self:created";
    let subject_b = "identity:22222222-2222-2222-2222-222222222222:self:created";

    for (seq, subject) in [(1i64, subject_a), (2, subject_b), (3, subject_a)] {
        mirror::insert_mirrored_entry(
            &pool,
            &mirrored_entry(&network_id, seq, &leaf_hash_hex(seq as u8), subject),
        )
        .await
        .expect("insert_mirrored_entry failed");
    }

    let all = mirror::mirrored_entries_since(&pool, &network_id, 0, 100, None)
        .await
        .expect("mirrored_entries_since failed");
    assert_eq!(all.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2, 3]);

    let since_one = mirror::mirrored_entries_since(&pool, &network_id, 1, 100, None)
        .await
        .expect("mirrored_entries_since failed");
    assert_eq!(
        since_one.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![2, 3],
        "since_seq must exclude the row at exactly that seq"
    );

    let only_a = mirror::mirrored_entries_since(&pool, &network_id, 0, 100, Some(subject_a))
        .await
        .expect("mirrored_entries_since failed");
    assert_eq!(
        only_a.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![1, 3],
        "subject filter must scope to that subject's own entries only"
    );

    let capped = mirror::mirrored_entries_since(&pool, &network_id, 0, 1, None)
        .await
        .expect("mirrored_entries_since failed");
    assert_eq!(capped.len(), 1, "limit must cap the returned rows");
    assert_eq!(capped[0].seq, 1);
}

/// A network no mirrored entries have ever been written for returns an
/// empty list, not an error.
#[tokio::test]
#[ignore]
async fn mirrored_entries_since_on_an_empty_network_is_an_empty_list() {
    let pool = test_pool().await;
    let network_id = unique_network_id("entries-since-empty");

    let entries = mirror::mirrored_entries_since(&pool, &network_id, 0, 100, None)
        .await
        .expect("mirrored_entries_since failed");
    assert!(entries.is_empty());
}

/// `mirrored_entry_hashes_up_to` returns exactly the first `tree_size`
/// entries' hashes in seq order, and the root built from them matches an
/// independently-computed root over the same leaves — proving this helper
/// reproduces the exact tree a mirror already verified each entry's
/// inclusion against while backfilling.
#[tokio::test]
#[ignore]
async fn mirrored_entry_hashes_up_to_reproduces_the_verified_tree() {
    let pool = test_pool().await;
    let network_id = unique_network_id("hashes-up-to");
    let hashes: Vec<String> = (1..=5u8).map(leaf_hash_hex).collect();

    for (i, hash) in hashes.iter().enumerate() {
        let seq = (i + 1) as i64;
        mirror::insert_mirrored_entry(
            &pool,
            &mirrored_entry(
                &network_id,
                seq,
                hash,
                "identity:11111111-1111-1111-1111-111111111111:self:created",
            ),
        )
        .await
        .expect("insert_mirrored_entry failed");
    }

    let up_to_three = mirror::mirrored_entry_hashes_up_to(&pool, &network_id, 3)
        .await
        .expect("mirrored_entry_hashes_up_to failed");
    assert_eq!(up_to_three, hashes[..3]);

    let up_to_all = mirror::mirrored_entry_hashes_up_to(&pool, &network_id, 5)
        .await
        .expect("mirrored_entry_hashes_up_to failed");
    assert_eq!(up_to_all, hashes);

    let expected_root =
        merkle::mth_of_hex_hashes(&hashes[..3]).expect("valid hex hashes should hash cleanly");
    let actual_root =
        merkle::mth_of_hex_hashes(&up_to_three).expect("valid hex hashes should hash cleanly");
    assert_eq!(
        expected_root, actual_root,
        "root over the mirror-served leaves must match an independently computed root"
    );
}

/// `mirrored_leaf_index_for_seq` ranks a mirrored entry among all mirrored
/// entries for its network ordered by `seq` — 0-indexed, matching the
/// authority-side `leaf_index_for_seq` convention exactly.
#[tokio::test]
#[ignore]
async fn mirrored_leaf_index_for_seq_ranks_entries_zero_indexed() {
    let pool = test_pool().await;
    let network_id = unique_network_id("leaf-index");

    for seq in [10i64, 20, 30] {
        mirror::insert_mirrored_entry(
            &pool,
            &mirrored_entry(
                &network_id,
                seq,
                &leaf_hash_hex(seq as u8),
                "identity:11111111-1111-1111-1111-111111111111:self:created",
            ),
        )
        .await
        .expect("insert_mirrored_entry failed");
    }

    assert_eq!(
        mirror::mirrored_leaf_index_for_seq(&pool, &network_id, 10)
            .await
            .expect("query failed"),
        Some(0)
    );
    assert_eq!(
        mirror::mirrored_leaf_index_for_seq(&pool, &network_id, 20)
            .await
            .expect("query failed"),
        Some(1)
    );
    assert_eq!(
        mirror::mirrored_leaf_index_for_seq(&pool, &network_id, 30)
            .await
            .expect("query failed"),
        Some(2)
    );
    assert_eq!(
        mirror::mirrored_leaf_index_for_seq(&pool, &network_id, 999)
            .await
            .expect("query failed"),
        None,
        "a seq with no mirrored entry must be None, not a fabricated rank"
    );
}

/// `observed_sth_matching_root` finds the one observation whose root_hash
/// matches exactly, and — the invariant issue #520's serving path leans
/// on — never returns a *different*, disagreeing observation recorded at
/// the same network_id/tree_size (e.g. a stale/dishonest peer's claim that
/// was never actually used to back this node's own mirrored entries).
#[tokio::test]
#[ignore]
async fn observed_sth_matching_root_ignores_disagreeing_observations_at_the_same_size() {
    let pool = test_pool().await;
    let network_id = unique_network_id("sth-matching-root");
    let good_root = "11".repeat(32);
    let bad_root = "22".repeat(32);

    mirror::insert_observation(
        &pool,
        &ObservedSth {
            source_url: "peer-a".to_string(),
            network_id: network_id.clone(),
            tree_size: 10,
            root_hash: good_root.clone(),
            signature: "sig-a".to_string(),
            signing_key_id: "key-a".to_string(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            observed_at: OffsetDateTime::now_utc(),
        },
    )
    .await
    .expect("insert_observation failed");
    mirror::insert_observation(
        &pool,
        &ObservedSth {
            source_url: "peer-b".to_string(),
            network_id: network_id.clone(),
            tree_size: 10,
            root_hash: bad_root,
            signature: "sig-b".to_string(),
            signing_key_id: "key-b".to_string(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            observed_at: OffsetDateTime::now_utc(),
        },
    )
    .await
    .expect("insert_observation failed");

    let matched = mirror::observed_sth_matching_root(&pool, &network_id, 10, &good_root)
        .await
        .expect("observed_sth_matching_root failed")
        .expect("the matching observation should be found");
    assert_eq!(matched.source_url, "peer-a");
    assert_eq!(matched.root_hash, good_root);

    let no_match = mirror::observed_sth_matching_root(&pool, &network_id, 10, &"33".repeat(32))
        .await
        .expect("observed_sth_matching_root failed");
    assert!(
        no_match.is_none(),
        "a root nobody actually reported must never match"
    );
}

/// `ObservedSth`/`SignedTreeHead` round-trip: the `created_at` a
/// mirror re-serves must be the STH's own signed `created_at`, never the
/// moment this node happened to poll it (`observed_at`) — the exact gap
/// #520 fixed in `observed_sths`' schema (see migration
/// `0064_observed_sths_created_at`).
#[tokio::test]
#[ignore]
async fn observed_sth_round_trips_the_signed_created_at_not_the_observed_at() {
    let pool = test_pool().await;
    let network_id = unique_network_id("created-at-roundtrip");
    let signed_created_at = OffsetDateTime::from_unix_timestamp(1_000_000_000).unwrap();
    let observed_at = OffsetDateTime::now_utc();

    mirror::insert_observation(
        &pool,
        &ObservedSth {
            source_url: "peer-a".to_string(),
            network_id: network_id.clone(),
            tree_size: 1,
            root_hash: "44".repeat(32),
            signature: "sig".to_string(),
            signing_key_id: "key".to_string(),
            created_at: signed_created_at,
            observed_at,
        },
    )
    .await
    .expect("insert_observation failed");

    let fetched = mirror::observed_sth_matching_root(&pool, &network_id, 1, &"44".repeat(32))
        .await
        .expect("observed_sth_matching_root failed")
        .expect("row should exist");
    assert_eq!(fetched.created_at, signed_created_at);
    assert_ne!(
        fetched.created_at, fetched.observed_at,
        "this test is only meaningful if the two timestamps actually differ"
    );

    let sth: avalon_chain::sth::SignedTreeHead = fetched.into();
    assert_eq!(
        sth.created_at, signed_created_at,
        "the served SignedTreeHead must carry the originally-signed created_at"
    );
}
