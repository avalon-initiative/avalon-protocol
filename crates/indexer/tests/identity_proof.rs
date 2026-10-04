//! Live tests (`--ignored`, migrated Postgres) for projection-time verification of identity
//! events: forged or foreign events are refused, leave nothing behind, and creations never
//! overwrite or merge.

use avalon_indexer::identity_proof::EventOrigin;
use avalon_indexer::postgres::PostgresIndexer;
use avalon_indexer::IndexError;
use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::identity_id::{
    device_grant_approval_signing_bytes_v2, signing_key_revoked_signing_bytes_v2, TestIdentity,
    TEST_NETWORK_ID, TEST_SHARD_ID,
};
use avalon_protocol::ids::GlobalId;
use base64::Engine as _;
use ed25519_dalek::Signer as _;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new().connect(&url).await.unwrap()
}

fn core() -> EventOrigin {
    EventOrigin::mirrored(TEST_NETWORK_ID, TEST_SHARD_ID)
}

fn game() -> EventOrigin {
    EventOrigin::mirrored(TEST_NETWORK_ID, "game:slug/1")
}

async fn apply(
    pool: &PgPool,
    event: &ProtocolEvent,
    origin: &EventOrigin,
) -> Result<(), IndexError> {
    let mut tx = pool.begin().await.unwrap();
    let result = PostgresIndexer::new(pool.clone())
        .apply_in_tx_from(&mut tx, event, origin)
        .await;
    match &result {
        Ok(()) => tx.commit().await.unwrap(),
        Err(_) => tx.rollback().await.unwrap(),
    }
    result
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn event(
    who: &TestIdentity,
    kind: &str,
    version: u32,
    payload: serde_json::Value,
) -> ProtocolEvent {
    let gid = GlobalId::new("identity", &who.id.to_string(), "self", "x");
    ProtocolEvent {
        id: Uuid::new_v4(),
        kind: kind.to_string(),
        issuer: gid.clone(),
        subject: gid,
        payload,
        timestamp: OffsetDateTime::now_utc(),
        version,
        identity_chain: None,
    }
}

fn created(who: &TestIdentity, shard: &str, name: &str) -> ProtocolEvent {
    let payload = who.created_payload_for(TEST_NETWORK_ID, shard, Uuid::new_v4(), name);
    event(
        who,
        "identity.created",
        2,
        serde_json::to_value(payload).unwrap(),
    )
}

fn unique_name(tag: &str) -> String {
    format!("{tag}-{}", Uuid::new_v4().simple())
}

fn inception(who: &TestIdentity, key_id: Uuid) -> ProtocolEvent {
    event(
        who,
        "identity.signing_key_added",
        2,
        serde_json::json!({
            "signing_key_id": key_id,
            "public_key": b64(&who.public_key()),
            "device_label": null,
            "approved_by_signing_key_id": key_id,
            "identity_id": who.id,
            "kind": "inception",
        }),
    )
}

/// A grant adding `device`'s key to `who`, signed by `signer` and naming `approver_key_id`.
fn grant(
    who: &TestIdentity,
    signer: &TestIdentity,
    approver_key_id: Uuid,
    device: &TestIdentity,
    key_id: Uuid,
) -> ProtocolEvent {
    let grant_id = Uuid::new_v4();
    let bytes = device_grant_approval_signing_bytes_v2(grant_id, &who.id, &device.public_key());
    event(
        who,
        "identity.signing_key_added",
        2,
        serde_json::json!({
            "signing_key_id": key_id,
            "public_key": b64(&device.public_key()),
            "device_label": null,
            "approved_by_signing_key_id": approver_key_id,
            "identity_id": who.id,
            "kind": "device_grant",
            "grant_id": grant_id,
            "approval_signature": b64(&signer.signing_key.sign(&bytes).to_bytes()),
        }),
    )
}

fn revoke(
    who: &TestIdentity,
    signer: &TestIdentity,
    signer_key_id: Uuid,
    key_id: Uuid,
) -> ProtocolEvent {
    let bytes = signing_key_revoked_signing_bytes_v2(&who.id, key_id, signer_key_id);
    event(
        who,
        "identity.signing_key_revoked",
        2,
        serde_json::json!({
            "identity_id": who.id,
            "signing_key_id": key_id,
            "revoked_by_signing_key_id": signer_key_id,
            "signature": b64(&signer.signing_key.sign(&bytes).to_bytes()),
        }),
    )
}

async fn count(pool: &PgPool, sql: &'static str, bind: impl ToString) -> i64 {
    sqlx::query_scalar(sql)
        .bind(bind.to_string())
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn nothing_left(pool: &PgPool, who: &TestIdentity, e: &ProtocolEvent) {
    let id = who.id;
    assert_eq!(
        count(pool, "SELECT count(*) FROM identities WHERE id = $1", id).await,
        0
    );
    assert_eq!(
        count(
            pool,
            "SELECT count(*) FROM profiles WHERE identity_id = $1",
            id
        )
        .await,
        0
    );
    assert_eq!(
        count(
            pool,
            "SELECT count(*) FROM indexer_applied_events WHERE event_id::text = $1",
            e.id
        )
        .await,
        0
    );
}

async fn profile_name(pool: &PgPool, who: &TestIdentity) -> Option<String> {
    sqlx::query_scalar("SELECT display_name FROM profiles WHERE identity_id = $1")
        .bind(who.id)
        .fetch_optional(pool)
        .await
        .unwrap()
}

/// Creates `who` (valid, from core) and its inception key; returns the inception key id.
async fn register(pool: &PgPool, who: &TestIdentity) -> Uuid {
    apply(
        pool,
        &created(who, TEST_SHARD_ID, &unique_name("reg")),
        &core(),
    )
    .await
    .unwrap();
    let key_id = Uuid::new_v4();
    apply(pool, &inception(who, key_id), &game()).await.unwrap();
    key_id
}

async fn active(pool: &PgPool, who: &TestIdentity, key_id: Uuid) -> bool {
    avalon_indexer::projections::identity_signing_keys::find_active_by_id(pool, who.id, key_id)
        .await
        .unwrap()
        .is_some()
}

#[tokio::test]
#[ignore]
async fn a_forged_or_misbound_creation_leaves_nothing_behind() {
    let pool = pool().await;
    let (victim, attacker) = (TestIdentity::new(), TestIdentity::new());

    let mut forged = created(&victim, TEST_SHARD_ID, &unique_name("forged"));
    forged.payload["signature"] =
        created(&attacker, TEST_SHARD_ID, "x").payload["signature"].clone();
    assert!(matches!(
        apply(&pool, &forged, &core()).await,
        Err(IndexError::Rejected(_))
    ));
    nothing_left(&pool, &victim, &forged).await;

    // Signed for shard `core`, delivered as if from a game shard.
    let replayed = created(&victim, TEST_SHARD_ID, &unique_name("replayed"));
    assert!(matches!(
        apply(&pool, &replayed, &game()).await,
        Err(IndexError::Rejected(_))
    ));
    nothing_left(&pool, &victim, &replayed).await;

    // The same creation, signed for the shard it comes from, projects from any shard.
    let honest = created(&victim, "game:slug/1", &unique_name("honest"));
    apply(&pool, &honest, &game()).await.unwrap();
    assert!(profile_name(&pool, &victim).await.is_some());
}

#[tokio::test]
#[ignore]
async fn a_second_creation_never_changes_an_existing_profile() {
    let pool = pool().await;
    let who = TestIdentity::new();
    let name = unique_name("first");
    let first = created(&who, TEST_SHARD_ID, &name);
    apply(&pool, &first, &core()).await.unwrap();

    let renamed = created(&who, TEST_SHARD_ID, &unique_name("second"));
    assert!(matches!(
        apply(&pool, &renamed, &core()).await,
        Err(IndexError::Rejected(_))
    ));
    assert_eq!(profile_name(&pool, &who).await, Some(name.clone()));

    // A re-creation naming the same name changes nothing.
    apply(&pool, &created(&who, TEST_SHARD_ID, &name), &core())
        .await
        .unwrap();
    assert_eq!(profile_name(&pool, &who).await, Some(name));
}

#[tokio::test]
#[ignore]
async fn keys_are_added_only_with_proof_from_an_active_key_of_the_identity() {
    let pool = pool().await;
    let (victim, attacker, device) = (
        TestIdentity::new(),
        TestIdentity::new(),
        TestIdentity::new(),
    );
    let inception_id = register(&pool, &victim).await;
    let attacker_inception = register(&pool, &attacker).await;

    // The attacker's own key as the victim's inception key.
    let injected = inception(&attacker, Uuid::new_v4());
    let mut as_victim = injected.clone();
    as_victim.id = Uuid::new_v4();
    as_victim.payload["identity_id"] = serde_json::json!(victim.id);
    as_victim.issuer = GlobalId::new("identity", &victim.id.to_string(), "self", "x");
    as_victim.subject = as_victim.issuer.clone();
    assert!(apply(&pool, &as_victim, &game()).await.is_err());

    // A grant signed by the attacker's key but naming the victim's approver key.
    let bad_sig = grant(&victim, &attacker, inception_id, &device, Uuid::new_v4());
    assert!(apply(&pool, &bad_sig, &game()).await.is_err());
    // A grant naming an approver key id that does not belong to the victim.
    let foreign_approver = grant(
        &victim,
        &attacker,
        attacker_inception,
        &device,
        Uuid::new_v4(),
    );
    assert!(apply(&pool, &foreign_approver, &game()).await.is_err());
    // A grant with the signature stripped.
    let mut unsigned = grant(&victim, &victim, inception_id, &device, Uuid::new_v4());
    unsigned
        .payload
        .as_object_mut()
        .unwrap()
        .remove("approval_signature");
    assert!(apply(&pool, &unsigned, &game()).await.is_err());
    // A recovery-kind key carries no proof.
    let mut recovery = grant(&victim, &victim, inception_id, &device, Uuid::new_v4());
    recovery.payload["kind"] = serde_json::json!("recovery");
    assert!(apply(&pool, &recovery, &game()).await.is_err());
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM indexer_identity_signing_keys WHERE identity_id = $1",
            victim.id
        )
        .await,
        1,
        "only the inception key exists"
    );

    // The victim's own approval projects, from any shard.
    let device_key = Uuid::new_v4();
    apply(
        &pool,
        &grant(&victim, &victim, inception_id, &device, device_key),
        &game(),
    )
    .await
    .unwrap();
    assert!(active(&pool, &victim, device_key).await);
}

#[tokio::test]
#[ignore]
async fn revocations_need_a_signature_from_an_active_key_of_the_identity() {
    let pool = pool().await;
    let (victim, attacker, device) = (
        TestIdentity::new(),
        TestIdentity::new(),
        TestIdentity::new(),
    );
    let inception_id = register(&pool, &victim).await;
    let attacker_key = register(&pool, &attacker).await;
    let device_key = Uuid::new_v4();
    apply(
        &pool,
        &grant(&victim, &victim, inception_id, &device, device_key),
        &game(),
    )
    .await
    .unwrap();

    // Signed by the attacker, or naming a key the victim does not have, or tampered.
    assert!(apply(
        &pool,
        &revoke(&victim, &attacker, inception_id, device_key),
        &game()
    )
    .await
    .is_err());
    assert!(apply(
        &pool,
        &revoke(&victim, &attacker, attacker_key, device_key),
        &game()
    )
    .await
    .is_err());
    let mut tampered = revoke(&victim, &victim, inception_id, device_key);
    tampered.payload["signing_key_id"] = serde_json::json!(inception_id);
    assert!(apply(&pool, &tampered, &game()).await.is_err());
    assert!(active(&pool, &victim, device_key).await);
    assert!(active(&pool, &victim, inception_id).await);

    // A revoked key can no longer revoke or approve.
    apply(
        &pool,
        &revoke(&victim, &victim, inception_id, device_key),
        &game(),
    )
    .await
    .unwrap();
    assert!(!active(&pool, &victim, device_key).await);
    let from_revoked = revoke(&victim, &device, device_key, inception_id);
    assert!(apply(&pool, &from_revoked, &game()).await.is_err());
    let approved_by_revoked = grant(&victim, &device, device_key, &attacker, Uuid::new_v4());
    assert!(apply(&pool, &approved_by_revoked, &game()).await.is_err());
}

#[tokio::test]
#[ignore]
async fn a_revoked_key_cannot_return_under_a_new_key_id() {
    let pool = pool().await;
    let (victim, device) = (TestIdentity::new(), TestIdentity::new());
    let inception_id = register(&pool, &victim).await;
    let device_key = Uuid::new_v4();
    apply(
        &pool,
        &grant(&victim, &victim, inception_id, &device, device_key),
        &game(),
    )
    .await
    .unwrap();
    apply(
        &pool,
        &revoke(&victim, &victim, inception_id, device_key),
        &game(),
    )
    .await
    .unwrap();
    let replay = grant(&victim, &victim, inception_id, &device, Uuid::new_v4());
    assert!(matches!(
        apply(&pool, &replay, &game()).await,
        Err(IndexError::Rejected(_))
    ));
    // A replayed inception under a fresh id is refused the same way.
    apply(
        &pool,
        &revoke(&victim, &victim, inception_id, inception_id),
        &core(),
    )
    .await
    .unwrap();
    assert!(apply(&pool, &inception(&victim, Uuid::new_v4()), &game())
        .await
        .is_err());
}

#[tokio::test]
#[ignore]
async fn unproven_identity_state_is_accepted_only_from_core_or_the_local_shard() {
    let pool = pool().await;
    let who = TestIdentity::new();
    register(&pool, &who).await;
    let passkey = |label: &str| {
        event(
            &who,
            "identity.passkey_registered",
            1,
            serde_json::json!({
                "passkey_id": Uuid::new_v4(), "identity_id": who.id,
                "credential_id": b64(Uuid::new_v4().as_bytes()),
                "passkey_data": {"k": 1}, "label": label,
            }),
        )
    };
    let from_game = passkey("game");
    assert!(matches!(
        apply(&pool, &from_game, &game()).await,
        Err(IndexError::Rejected(_))
    ));
    let profile_from_game = event(
        &who,
        "profile.updated",
        1,
        serde_json::json!({ "bio": "owned" }),
    );
    assert!(apply(&pool, &profile_from_game, &game()).await.is_err());
    let recovered = event(
        &who,
        "identity.recovered",
        1,
        serde_json::json!({ "request_id": Uuid::nil() }),
    );
    assert!(apply(&pool, &recovered, &game()).await.is_err());
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM indexer_identity_passkeys WHERE identity_id = $1",
            who.id
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM identity_chain_events WHERE identity_id = $1",
            who.id
        )
        .await,
        0,
        "a refused event never reaches the identity chain"
    );

    apply(&pool, &passkey("core"), &core()).await.unwrap();
    apply(
        &pool,
        &passkey("local"),
        &EventOrigin::local(TEST_NETWORK_ID, "game:slug/1"),
    )
    .await
    .unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM indexer_identity_passkeys WHERE identity_id = $1",
            who.id
        )
        .await,
        2
    );
}

#[tokio::test]
#[ignore]
async fn a_contested_name_resolves_the_same_in_either_arrival_order() {
    let pool = pool().await;
    let (x, y) = (TestIdentity::new(), TestIdentity::new());
    let name = unique_name("contested");
    let (cx, cy) = (
        created(&x, "game:slug/1", &name),
        created(&y, "core", &name),
    );
    let (low, high) = if x.id < y.id { (&x, &y) } else { (&y, &x) };

    let mut outcomes = Vec::new();
    for reversed in [false, true] {
        let mut order = [(&cx, game()), (&cy, core())];
        if reversed {
            order.reverse();
        }
        for (event, origin) in &order {
            apply(&pool, event, origin).await.unwrap();
        }
        outcomes.push((
            profile_name(&pool, low).await.unwrap(),
            profile_name(&pool, high).await.unwrap(),
        ));
        sqlx::query("DELETE FROM identities WHERE id = ANY($1)")
            .bind(vec![x.id, y.id])
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM indexer_applied_events WHERE event_id = ANY($1)")
            .bind(vec![cx.id, cy.id])
            .execute(&pool)
            .await
            .unwrap();
    }
    assert_eq!(outcomes[0], outcomes[1]);
    assert_eq!(outcomes[0].0, name, "the smaller id keeps the name");
    assert!(outcomes[0].1.starts_with(&name) && outcomes[0].1.contains('~'));
}

#[tokio::test]
#[ignore]
async fn a_local_name_clash_is_refused_and_leaves_no_identity_row() {
    let pool = pool().await;
    let (holder, newcomer) = (TestIdentity::new(), TestIdentity::new());
    let name = unique_name("local");
    apply(&pool, &created(&holder, TEST_SHARD_ID, &name), &core())
        .await
        .unwrap();
    let clash = created(&newcomer, TEST_SHARD_ID, &name);
    let local = EventOrigin::local(TEST_NETWORK_ID, TEST_SHARD_ID);
    assert!(matches!(
        apply(&pool, &clash, &local).await,
        Err(IndexError::DisplayNameTaken)
    ));
    nothing_left(&pool, &newcomer, &clash).await;
}
