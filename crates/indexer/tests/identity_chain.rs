//! Live tests for per-identity chain resolution in the indexer, run with
//! `--ignored` against a migrated Postgres. Two nodes are simulated by
//! applying the same event set to one database in different orders and
//! comparing the resulting state.

use avalon_indexer::identity_chain_store;
use avalon_indexer::identity_proof::EventOrigin;
use avalon_indexer::postgres::PostgresIndexer;
use avalon_indexer::Indexer;
use avalon_protocol::events::{IdentityChainPosition, ProtocolEvent};
use avalon_protocol::identity_chain_wire::{event_hash, needs_author_signature};
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

#[allow(clippy::too_many_arguments)]
fn chained(
    who: &TestIdentity,
    key_id: Uuid,
    kind: &str,
    verb: &str,
    payload: serde_json::Value,
    ts_secs: i64,
    seq: u64,
    prev: Option<&ProtocolEvent>,
) -> ProtocolEvent {
    let identity_id = who.id;
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
    if needs_author_signature(kind) {
        who.sign_event(&mut event, key_id);
    }
    event
}

fn indexer(pool: &PgPool) -> PostgresIndexer {
    PostgresIndexer::new(pool.clone()).with_local_origin(TEST_NETWORK_ID, "core")
}

/// A signed `identity.signing_key_added` payload for a fresh device key, approved by `approver`
/// at the first chain position (seq 1, no previous event).
fn device_grant_payload(
    who: &TestIdentity,
    approver_key_id: Uuid,
    seed: u8,
    seq: u64,
    prev: Option<&ProtocolEvent>,
) -> serde_json::Value {
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
        seq,
        prev.map(|p| event_hash(p).unwrap()).as_ref(),
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
        who,
        key_id,
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
    let who = seed_identity(&pool).await;
    let id = who.id;
    let key = add_inception_key(&pool, &indexer, &who).await;

    let root = chained(
        &who,
        key,
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
        &who,
        key,
        "profile.updated",
        "profile_updated",
        serde_json::json!({"bio": "A", "pronouns": "pA"}),
        200,
        2,
        Some(&root),
    );
    let b = chained(
        &who,
        key,
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
    use base64::Engine as _;
    use ed25519_dalek::Signer as _;
    let pool = test_pool().await;
    let indexer = indexer(&pool);
    let who = seed_identity(&pool).await;
    let id = who.id;
    let inception = add_inception_key(&pool, &indexer, &who).await;
    let mut guardians = Vec::new();
    for _ in 0..2 {
        let g = seed_identity(&pool).await;
        let key = add_inception_key(&pool, &indexer, &g).await;
        guardians.push((g, key));
    }

    let configured = chained(
        &who,
        inception,
        "identity.recovery_configured",
        "recovery_configured",
        serde_json::json!({
            "guardian_ids": guardians.iter().map(|(g, _)| g.id).collect::<Vec<_>>(),
            "threshold": 2,
        }),
        50,
        1,
        None,
    );
    let mut a = chained(
        &who,
        inception,
        "identity.signing_key_added",
        "signing_key_added",
        device_grant_payload(&who, inception, 7, 2, Some(&configured)),
        100,
        2,
        Some(&configured),
    );
    let mut b = chained(
        &who,
        inception,
        "identity.signing_key_added",
        "signing_key_added",
        device_grant_payload(&who, inception, 8, 2, Some(&configured)),
        100,
        2,
        Some(&configured),
    );
    a.version = 2;
    b.version = 2;

    let new_key = TestIdentity::new();
    let request_id = Uuid::new_v4();
    let b64 = base64::engine::general_purpose::STANDARD;
    let approvals: Vec<_> = guardians
        .iter()
        .map(|(g, key)| {
            let bytes = avalon_protocol::identity_id::recovery_approval_signing_bytes(
                TEST_NETWORK_ID,
                &id,
                request_id,
                &g.id,
                *key,
                &new_key.public_key(),
            );
            serde_json::json!({
                "guardian_id": g.id,
                "signing_key_id": key,
                "signature": b64.encode(g.signing_key.sign(&bytes).to_bytes()),
            })
        })
        .collect();
    let mut recovered = chained(
        &who,
        inception,
        "identity.recovered",
        "recovered",
        serde_json::json!({
            "request_id": request_id,
            "device_label": null,
            "new_signing_public_key": b64.encode(new_key.public_key()),
            "approvals": approvals,
        }),
        400,
        2,
        Some(&configured),
    );
    new_key.sign_event(&mut recovered, request_id);

    reset(&pool, id, &[&configured, &a, &b, &recovered]).await;
    indexer.apply(&configured).await.unwrap();
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
    assert_eq!(state.seq, 2);
    assert_eq!(
        state.head_hash.unwrap(),
        hex::encode(event_hash(&recovered).unwrap())
    );
}

#[tokio::test]
#[ignore]
async fn a_chained_event_without_a_valid_author_signature_cannot_take_a_chain_position() {
    let pool = test_pool().await;
    let indexer = indexer(&pool);
    let who = seed_identity(&pool).await;
    let id = who.id;
    let inception = add_inception_key(&pool, &indexer, &who).await;

    let mut forged = chained(
        &who,
        inception,
        "friend.requested",
        "friend_requested",
        serde_json::json!({"from": id, "to": IdentityId::random_for_tests()}),
        100,
        1,
        None,
    );
    let origin = EventOrigin::mirrored(TEST_NETWORK_ID, "game:evil/1");
    // Signed by a key that is not the identity's, then unsigned: neither takes the position.
    let attacker = TestIdentity::new();
    attacker.sign_event(&mut forged, inception);
    let unsigned = {
        let mut e = forged.clone();
        e.identity_chain = Some(IdentityChainPosition::current(1, None));
        e
    };
    for event in [&forged, &unsigned] {
        let mut tx = pool.begin().await.unwrap();
        let applied = indexer.apply_in_tx_from(&mut tx, event, &origin).await;
        tx.commit().await.unwrap();
        assert!(applied.is_err(), "the forged event was accepted");
    }

    let mut grant = chained(
        &who,
        inception,
        "identity.signing_key_added",
        "signing_key_added",
        device_grant_payload(&who, inception, 7, 1, None),
        200,
        1,
        None,
    );
    grant.version = 2;
    let origin = EventOrigin::mirrored(TEST_NETWORK_ID, "game:honest/1");
    let mut tx = pool.begin().await.unwrap();
    indexer
        .apply_in_tx_from(&mut tx, &grant, &origin)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(!identity_chain_store::is_forked(&pool, id).await.unwrap());

    // A properly signed event extends the chain from whichever shard delivers it.
    let next = chained(
        &who,
        inception,
        "friend.requested",
        "friend_requested",
        serde_json::json!({"from": id, "to": IdentityId::random_for_tests()}),
        300,
        2,
        Some(&grant),
    );
    let origin = EventOrigin::mirrored(TEST_NETWORK_ID, "game:other/1");
    let mut tx = pool.begin().await.unwrap();
    indexer
        .apply_in_tx_from(&mut tx, &next, &origin)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let state = identity_chain_store::state(&pool, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((state.seq, state.forked_at_seq), (2, None));
}
