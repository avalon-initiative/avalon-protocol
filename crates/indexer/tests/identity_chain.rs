//! Live tests for per-identity chain resolution in the indexer, run with
//! `--ignored` against a migrated Postgres. Two nodes are simulated by
//! applying the same event set to one database in different orders and
//! comparing the resulting state.

use avalon_indexer::identity_chain_store;
use avalon_indexer::postgres::PostgresIndexer;
use avalon_indexer::Indexer;
use avalon_protocol::events::{IdentityChainPosition, ProtocolEvent};
use avalon_protocol::identity_chain_wire::event_hash;
use avalon_protocol::identity_id::{
    device_grant_approval_signing_bytes, TestIdentity, TEST_NETWORK_ID,
};
use avalon_protocol::ids::{GlobalId, IdentityId};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

async fn test_pool() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres")
}

fn gid(identity_id: IdentityId, verb: &str) -> GlobalId {
    GlobalId::new("identity", &identity_id.to_string(), "self", verb)
}

fn chained(
    identity_id: IdentityId,
    kind: &str,
    verb: &str,
    payload: serde_json::Value,
    ts_secs: i64,
    seq: u64,
    prev: Option<&ProtocolEvent>,
) -> ProtocolEvent {
    let mut event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: kind.to_string(),
        issuer: gid(identity_id, verb),
        subject: gid(identity_id, verb),
        payload,
        timestamp: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(ts_secs),
        version: 1,
        identity_chain: None,
    };
    event.identity_chain = Some(IdentityChainPosition::current(
        seq,
        prev.map(|p| hex::encode(event_hash(p).unwrap())),
    ));
    event
}

fn indexer(pool: &PgPool) -> PostgresIndexer {
    PostgresIndexer::new(pool.clone()).with_local_origin(TEST_NETWORK_ID, "core")
}

/// A signed `identity.signing_key_added` payload for a fresh device key, approved by `approver`
/// at the first chain position (seq 1, no previous event).
fn device_grant_payload(who: &TestIdentity, approver_key_id: Uuid, seed: u8) -> serde_json::Value {
    use base64::Engine as _;
    use ed25519_dalek::Signer as _;
    let device = TestIdentity::from_seed([seed; 32]);
    let grant_id = Uuid::new_v4();
    let bytes = device_grant_approval_signing_bytes(
        TEST_NETWORK_ID,
        grant_id,
        &who.id,
        approver_key_id,
        &device.public_key(),
        1,
        None,
    );
    let b64 = base64::engine::general_purpose::STANDARD;
    serde_json::json!({
        "signing_key_id": grant_id,
        "public_key": b64.encode(device.public_key()),
        "device_label": null,
        "approved_by_signing_key_id": approver_key_id,
        "identity_id": who.id,
        "kind": "device_grant",
        "grant_id": grant_id,
        "approval_signature": b64.encode(who.signing_key.sign(&bytes).to_bytes()),
    })
}

/// Applies the identity's inception key (unchained) and returns its key id, which the creation
/// ticket fixed (seeded here, as no `identity.created` is applied).
async fn add_inception_key(pool: &PgPool, indexer: &PostgresIndexer, who: &TestIdentity) -> Uuid {
    use base64::Engine as _;
    let key_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO indexer_identity_signing_keys (signing_key_id, identity_id, public_key, added_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(key_id)
    .bind(who.id)
    .bind(who.public_key().to_vec())
    .execute(pool)
    .await
    .unwrap();
    let mut event = chained(
        who.id,
        "identity.signing_key_added",
        "signing_key_added",
        serde_json::json!({
            "signing_key_id": key_id,
            "public_key": base64::engine::general_purpose::STANDARD.encode(who.public_key()),
            "device_label": null,
            "approved_by_signing_key_id": key_id,
            "identity_id": who.id,
            "kind": "inception",
        }),
        50,
        1,
        None,
    );
    event.version = 2;
    event.identity_chain = None;
    indexer.apply(&event).await.unwrap();
    key_id
}

async fn seed_identity(pool: &PgPool) -> TestIdentity {
    let who = TestIdentity::new();
    let identity_id = who.id;
    sqlx::query("INSERT INTO identities (id, inception_public_key) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(who.public_key().to_vec())
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(format!("chain-test-{identity_id}"))
        .execute(pool)
        .await
        .unwrap();
    who
}

async fn reset(pool: &PgPool, identity_id: IdentityId, events: &[&ProtocolEvent]) {
    for e in events {
        sqlx::query("DELETE FROM indexer_applied_events WHERE event_id = $1")
            .bind(e.id)
            .execute(pool)
            .await
            .unwrap();
    }
    sqlx::query("DELETE FROM identity_chain_events WHERE identity_id = $1")
        .bind(identity_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM identity_chain_state WHERE identity_id = $1")
        .bind(identity_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE profiles SET bio = NULL, pronouns = NULL WHERE identity_id = $1")
        .bind(identity_id)
        .execute(pool)
        .await
        .unwrap();
}

async fn profile(pool: &PgPool, identity_id: IdentityId) -> (Option<String>, Option<String>) {
    let row = sqlx::query("SELECT bio, pronouns FROM profiles WHERE identity_id = $1")
        .bind(identity_id)
        .fetch_one(pool)
        .await
        .unwrap();
    (
        row.try_get("bio").unwrap(),
        row.try_get("pronouns").unwrap(),
    )
}

#[tokio::test]
#[ignore]
async fn conflicting_profile_edits_converge_in_any_arrival_order() {
    let pool = test_pool().await;
    let indexer = indexer(&pool);
    let id = seed_identity(&pool).await.id;

    let root = chained(
        id,
        "profile.updated",
        "profile_updated",
        serde_json::json!({"bio": "root"}),
        100,
        1,
        None,
    );
    // Concurrent at seq 2: the later timestamp (b) wins; a also set pronouns,
    // which must not survive its displacement.
    let a = chained(
        id,
        "profile.updated",
        "profile_updated",
        serde_json::json!({"bio": "A", "pronouns": "pA"}),
        200,
        2,
        Some(&root),
    );
    let b = chained(
        id,
        "profile.updated",
        "profile_updated",
        serde_json::json!({"bio": "B"}),
        300,
        2,
        Some(&root),
    );
    let all = [&root, &a, &b];

    let orders: [[&ProtocolEvent; 3]; 4] = [
        [&root, &a, &b],
        [&root, &b, &a],
        [&b, &a, &root],
        [&a, &b, &root],
    ];
    for order in orders {
        reset(&pool, id, &all).await;
        for e in order {
            indexer.apply(e).await.unwrap();
        }
        assert_eq!(
            profile(&pool, id).await,
            (Some("B".to_string()), None),
            "order {:?}",
            order
                .iter()
                .map(|e| e.payload.to_string())
                .collect::<Vec<_>>()
        );
        let state = identity_chain_store::state(&pool, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.seq, 2);
        assert_eq!(state.forked_at_seq, None);
    }
}

#[tokio::test]
#[ignore]
async fn conflicting_key_events_fork_and_recovery_resolves() {
    let pool = test_pool().await;
    let indexer = indexer(&pool);
    let who = seed_identity(&pool).await;
    let id = who.id;
    let inception = add_inception_key(&pool, &indexer, &who).await;

    let mut a = chained(
        id,
        "identity.signing_key_added",
        "signing_key_added",
        device_grant_payload(&who, inception, 7),
        100,
        1,
        None,
    );
    let mut b = chained(
        id,
        "identity.signing_key_added",
        "signing_key_added",
        device_grant_payload(&who, inception, 8),
        100,
        1,
        None,
    );
    a.version = 2;
    b.version = 2;
    let recovered = chained(
        id,
        "identity.recovered",
        "recovered",
        serde_json::json!({"request_id": Uuid::nil()}),
        400,
        1,
        None,
    );

    reset(&pool, id, &[&a, &b, &recovered]).await;
    assert!(!identity_chain_store::is_forked(&pool, id).await.unwrap());
    indexer.apply(&a).await.unwrap();
    assert!(!identity_chain_store::is_forked(&pool, id).await.unwrap());
    indexer.apply(&b).await.unwrap();
    assert!(identity_chain_store::is_forked(&pool, id).await.unwrap());

    indexer.apply(&recovered).await.unwrap();
    let state = identity_chain_store::state(&pool, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.forked_at_seq, None);
    assert_eq!(state.seq, 1);
    assert_eq!(
        state.head_hash.unwrap(),
        hex::encode(event_hash(&recovered).unwrap())
    );
}

#[tokio::test]
#[ignore]
async fn local_assignment_chains_events_and_freezes_on_fork() {
    let pool = test_pool().await;
    let id = seed_identity(&pool).await.id;

    let mut tx = pool.begin().await.unwrap();
    let mut first = chained(
        id,
        "profile.updated",
        "profile_updated",
        serde_json::json!({"bio": "1"}),
        0,
        1,
        None,
    );
    first.identity_chain = None;
    first.timestamp = OffsetDateTime::now_utc();
    identity_chain_store::assign_local(&mut tx, &mut first)
        .await
        .unwrap();
    let mut second = first.clone();
    second.id = Uuid::new_v4();
    second.payload = serde_json::json!({"bio": "2"});
    second.identity_chain = None;
    identity_chain_store::assign_local(&mut tx, &mut second)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let p1 = first.identity_chain.clone().unwrap();
    let p2 = second.identity_chain.clone().unwrap();
    assert_eq!((p1.seq, p1.prev_hash.clone()), (1, None));
    assert_eq!(p2.seq, 2);
    assert_eq!(p2.prev_hash, Some(hex::encode(event_hash(&first).unwrap())));

    // A forked identity gets no position for ordinary local events.
    sqlx::query("UPDATE identity_chain_state SET forked_at_seq = 3 WHERE identity_id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let mut third = second.clone();
    third.id = Uuid::new_v4();
    third.identity_chain = None;
    identity_chain_store::assign_local(&mut tx, &mut third)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(third.identity_chain.is_none());
}
