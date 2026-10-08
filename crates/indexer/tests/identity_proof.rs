//! Live tests (`--ignored`, migrated Postgres) for projection-time verification of identity
//! events: forged or foreign events are refused, leave nothing behind, and creations never
//! overwrite or merge.

use avalon_indexer::identity_proof::EventOrigin;
use avalon_indexer::postgres::PostgresIndexer;
use avalon_indexer::IndexError;
use avalon_protocol::events::{IdentityChainPosition, ProtocolEvent};
use avalon_protocol::identity_chain_wire::event_hash;
use avalon_protocol::identity_id::{
    device_grant_approval_signing_bytes, signing_key_revoked_signing_bytes, TestIdentity,
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

/// Another non-core shard of the same network.
fn other() -> EventOrigin {
    EventOrigin::mirrored(TEST_NETWORK_ID, "game:other/1")
}

async fn apply(
    pool: &PgPool,
    event: &ProtocolEvent,
    origin: &EventOrigin,
) -> Result<(), IndexError> {
    let mut tx = pool.begin().await.unwrap();
    let result = PostgresIndexer::new(pool.clone())
        .with_local_origin(TEST_NETWORK_ID, TEST_SHARD_ID)
        .apply_in_tx_from(&mut tx, event, origin)
        .await;
    // Commits even after a refusal: the refused event must have left nothing in the transaction.
    tx.commit().await.unwrap();
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

fn created(who: &TestIdentity, name: &str) -> ProtocolEvent {
    let payload = who.created_payload_for(TEST_NETWORK_ID, Uuid::new_v4(), name);
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

/// The chain position directly after `after` (the first chained position when `None`).
fn next_position(after: Option<&ProtocolEvent>) -> (u64, Option<[u8; 32]>) {
    after.map_or((1, None), |e| {
        let position = e.identity_chain.as_ref().unwrap();
        (position.seq + 1, Some(event_hash(e).unwrap()))
    })
}

fn positioned(mut event: ProtocolEvent, seq: u64, prev: Option<[u8; 32]>) -> ProtocolEvent {
    event.identity_chain = Some(IdentityChainPosition::current(seq, prev.map(hex::encode)));
    event
}

/// A grant adding `device`'s key to `who` as `key_id` (the grant id), signed by `signer`, naming
/// `approver_key_id`, at the chain position after `after`.
fn grant(
    who: &TestIdentity,
    signer: &TestIdentity,
    approver_key_id: Uuid,
    device: &TestIdentity,
    key_id: Uuid,
    after: Option<&ProtocolEvent>,
) -> ProtocolEvent {
    let grant_id = key_id;
    let (seq, prev) = next_position(after);
    let bytes = device_grant_approval_signing_bytes(
        TEST_NETWORK_ID,
        grant_id,
        &who.id,
        approver_key_id,
        &device.public_key(),
        seq,
        prev.as_ref(),
    );
    let event = event(
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
    );
    positioned(event, seq, prev)
}

fn revoke(
    who: &TestIdentity,
    signer: &TestIdentity,
    signer_key_id: Uuid,
    key_id: Uuid,
    after: Option<&ProtocolEvent>,
) -> ProtocolEvent {
    let (seq, prev) = next_position(after);
    let bytes = signing_key_revoked_signing_bytes(
        TEST_NETWORK_ID,
        &who.id,
        key_id,
        signer_key_id,
        seq,
        prev.as_ref(),
    );
    let event = event(
        who,
        "identity.signing_key_revoked",
        2,
        serde_json::json!({
            "identity_id": who.id,
            "signing_key_id": key_id,
            "revoked_by_signing_key_id": signer_key_id,
            "signature": b64(&signer.signing_key.sign(&bytes).to_bytes()),
        }),
    );
    positioned(event, seq, prev)
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

/// The inception key id a creation fixes: its ticket id.
fn ticket_of(created: &ProtocolEvent) -> Uuid {
    serde_json::from_value(created.payload["ticket_id"].clone()).unwrap()
}

/// Creates `who` (valid) and its inception key; returns the inception key id.
async fn register(pool: &PgPool, who: &TestIdentity) -> Uuid {
    let creation = created(who, &unique_name("reg"));
    apply(pool, &creation, &game()).await.unwrap();
    let key_id = ticket_of(&creation);
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

    let mut forged = created(&victim, &unique_name("forged"));
    forged.payload["signature"] = created(&attacker, "x").payload["signature"].clone();
    assert!(matches!(
        apply(&pool, &forged, &core()).await,
        Err(IndexError::Rejected(_))
    ));
    nothing_left(&pool, &victim, &forged).await;

    // Signed for this network, delivered as if from another network.
    let replayed = created(&victim, &unique_name("replayed"));
    let foreign = EventOrigin::mirrored("another-network", TEST_SHARD_ID);
    assert!(matches!(
        apply(&pool, &replayed, &foreign).await,
        Err(IndexError::Rejected(_))
    ));
    nothing_left(&pool, &victim, &replayed).await;

    // The same creation projects from any shard of its network.
    let honest = created(&victim, &unique_name("honest"));
    apply(&pool, &honest, &game()).await.unwrap();
    assert!(profile_name(&pool, &victim).await.is_some());
}

#[tokio::test]
#[ignore]
async fn a_second_creation_never_changes_an_existing_profile() {
    let pool = pool().await;
    let who = TestIdentity::new();
    let name = unique_name("first");
    let first = created(&who, &name);
    apply(&pool, &first, &core()).await.unwrap();

    let renamed = created(&who, &unique_name("second"));
    assert!(matches!(
        apply(&pool, &renamed, &core()).await,
        Err(IndexError::Rejected(_))
    ));
    assert_eq!(profile_name(&pool, &who).await, Some(name.clone()));

    // A re-creation naming the same name changes nothing.
    apply(&pool, &created(&who, &name), &core()).await.unwrap();
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
    let bad_sig = grant(
        &victim,
        &attacker,
        inception_id,
        &device,
        Uuid::new_v4(),
        None,
    );
    assert!(apply(&pool, &bad_sig, &game()).await.is_err());
    // A grant naming an approver key id that does not belong to the victim.
    let foreign_approver = grant(
        &victim,
        &attacker,
        attacker_inception,
        &device,
        Uuid::new_v4(),
        None,
    );
    assert!(apply(&pool, &foreign_approver, &game()).await.is_err());
    // A grant with the signature stripped.
    let mut unsigned = grant(
        &victim,
        &victim,
        inception_id,
        &device,
        Uuid::new_v4(),
        None,
    );
    unsigned
        .payload
        .as_object_mut()
        .unwrap()
        .remove("approval_signature");
    assert!(apply(&pool, &unsigned, &game()).await.is_err());
    // The pre-v2 shape is refused outright.
    let mut v1 = grant(
        &victim,
        &victim,
        inception_id,
        &device,
        Uuid::new_v4(),
        None,
    );
    v1.version = 1;
    assert!(apply(&pool, &v1, &game()).await.is_err());
    // A recovery-kind key carries no proof.
    let mut recovery = grant(
        &victim,
        &victim,
        inception_id,
        &device,
        Uuid::new_v4(),
        None,
    );
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
        &grant(&victim, &victim, inception_id, &device, device_key, None),
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
    let added = grant(&victim, &victim, inception_id, &device, device_key, None);
    apply(&pool, &added, &game()).await.unwrap();

    // Signed by the attacker, or naming a key the victim does not have, or tampered.
    assert!(apply(
        &pool,
        &revoke(&victim, &attacker, inception_id, device_key, Some(&added)),
        &game()
    )
    .await
    .is_err());
    assert!(apply(
        &pool,
        &revoke(&victim, &attacker, attacker_key, device_key, Some(&added)),
        &game()
    )
    .await
    .is_err());
    let mut tampered = revoke(&victim, &victim, inception_id, device_key, Some(&added));
    tampered.payload["signing_key_id"] = serde_json::json!(inception_id);
    assert!(apply(&pool, &tampered, &game()).await.is_err());
    assert!(active(&pool, &victim, device_key).await);
    assert!(active(&pool, &victim, inception_id).await);

    // A revoked key can no longer revoke or approve.
    let revoked = revoke(&victim, &victim, inception_id, device_key, Some(&added));
    apply(&pool, &revoked, &game()).await.unwrap();
    assert!(!active(&pool, &victim, device_key).await);
    let from_revoked = revoke(&victim, &device, device_key, inception_id, Some(&revoked));
    assert!(apply(&pool, &from_revoked, &game()).await.is_err());
    let approved_by_revoked = grant(
        &victim,
        &device,
        device_key,
        &attacker,
        Uuid::new_v4(),
        Some(&revoked),
    );
    assert!(apply(&pool, &approved_by_revoked, &game()).await.is_err());
}

#[tokio::test]
#[ignore]
async fn a_revoked_key_cannot_return_under_a_new_key_id() {
    let pool = pool().await;
    let (victim, device) = (TestIdentity::new(), TestIdentity::new());
    let inception_id = register(&pool, &victim).await;
    let device_key = Uuid::new_v4();
    let added = grant(&victim, &victim, inception_id, &device, device_key, None);
    apply(&pool, &added, &game()).await.unwrap();
    let revoked = revoke(&victim, &victim, inception_id, device_key, Some(&added));
    apply(&pool, &revoked, &game()).await.unwrap();
    let replay = grant(
        &victim,
        &victim,
        inception_id,
        &device,
        Uuid::new_v4(),
        Some(&revoked),
    );
    assert!(matches!(
        apply(&pool, &replay, &game()).await,
        Err(IndexError::Rejected(_))
    ));
    // A replayed inception under a fresh id is refused the same way.
    apply(
        &pool,
        &revoke(&victim, &victim, inception_id, inception_id, Some(&revoked)),
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
async fn a_contested_name_stays_with_its_first_holder_and_the_newcomer_is_suffixed() {
    let pool = pool().await;
    let (x, y) = (TestIdentity::new(), TestIdentity::new());
    let name = unique_name("contested");
    apply(&pool, &created(&x, &name), &game()).await.unwrap();
    apply(&pool, &created(&y, &name), &core()).await.unwrap();
    assert_eq!(profile_name(&pool, &x).await, Some(name.clone()));
    let suffixed = profile_name(&pool, &y).await.unwrap();
    assert_eq!(suffixed, format!("{name}~{}", &y.id.to_string()[..12]));
}

#[tokio::test]
#[ignore]
async fn a_mirrored_creation_never_displaces_a_local_holder_whatever_its_id() {
    let pool = pool().await;
    let local_holder = TestIdentity::new();
    let name = unique_name("local-holder");
    let local = EventOrigin::local(TEST_NETWORK_ID, TEST_SHARD_ID);
    apply(&pool, &created(&local_holder, &name), &local)
        .await
        .unwrap();
    // An identity with a smaller id than the holder's (ids are cheap to grind).
    let mut grinder = TestIdentity::new();
    while grinder.id >= local_holder.id {
        grinder = TestIdentity::new();
    }
    apply(&pool, &created(&grinder, &name), &game())
        .await
        .unwrap();
    assert_eq!(profile_name(&pool, &local_holder).await, Some(name.clone()));
    assert_ne!(profile_name(&pool, &grinder).await, Some(name));
}

#[tokio::test]
#[ignore]
async fn a_taken_suffixed_name_moves_on_to_a_longer_prefix() {
    let pool = pool().await;
    let (holder, squatter, newcomer) = (
        TestIdentity::new(),
        TestIdentity::new(),
        TestIdentity::new(),
    );
    let name = unique_name("suffix");
    apply(&pool, &created(&holder, &name), &game())
        .await
        .unwrap();
    // Someone claims exactly the name the newcomer would be given.
    let taken = format!("{name}~{}", &newcomer.id.to_string()[..12]);
    apply(&pool, &created(&squatter, &taken), &game())
        .await
        .unwrap();
    apply(&pool, &created(&newcomer, &name), &game())
        .await
        .unwrap();
    assert_eq!(
        profile_name(&pool, &newcomer).await,
        Some(format!("{name}~{}", &newcomer.id.to_string()[..16]))
    );
}

#[tokio::test]
#[ignore]
async fn a_local_name_clash_is_refused_and_leaves_no_identity_row() {
    let pool = pool().await;
    let (holder, newcomer) = (TestIdentity::new(), TestIdentity::new());
    let name = unique_name("local");
    apply(&pool, &created(&holder, &name), &core())
        .await
        .unwrap();
    let clash = created(&newcomer, &name);
    let local = EventOrigin::local(TEST_NETWORK_ID, TEST_SHARD_ID);
    assert!(matches!(
        apply(&pool, &clash, &local).await,
        Err(IndexError::DisplayNameTaken)
    ));
    nothing_left(&pool, &newcomer, &clash).await;
}

async fn chain_rows(pool: &PgPool, who: &TestIdentity) -> i64 {
    count(
        pool,
        "SELECT count(*) FROM identity_chain_events WHERE identity_id = $1",
        who.id,
    )
    .await
}

fn at_position(
    mut e: ProtocolEvent,
    seq: u64,
    prev: Option<&ProtocolEvent>,
    secs: i64,
) -> ProtocolEvent {
    use avalon_protocol::events::IdentityChainPosition;
    e.timestamp = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(secs);
    e.identity_chain = Some(IdentityChainPosition::current(
        seq,
        prev.map(|p| hex::encode(avalon_protocol::identity_chain_wire::event_hash(p).unwrap())),
    ));
    e
}

#[tokio::test]
#[ignore]
async fn a_signed_key_event_replayed_with_other_unsigned_fields_is_the_same_chain_event() {
    let pool = pool().await;
    let (victim, device) = (TestIdentity::new(), TestIdentity::new());
    let inception_id = register(&pool, &victim).await;
    let key_id = Uuid::new_v4();
    let signed = grant(&victim, &victim, inception_id, &device, key_id, None);
    let genuine = at_position(signed.clone(), 1, None, 100);
    apply(&pool, &genuine, &game()).await.unwrap();
    assert_eq!(chain_rows(&pool, &victim).await, 1);

    // The same signed grant under another event id, time, label, issuer verb and extra field, from
    // any shard, never adds a second hash at the position.
    let mut replay = at_position(signed, 1, None, 200);
    replay.id = Uuid::new_v4();
    replay.payload["device_label"] = serde_json::json!("renamed");
    replay.payload["unexpected"] = serde_json::json!(1);
    replay.issuer = GlobalId::new("identity", &victim.id.to_string(), "self", "other");
    apply(&pool, &replay, &other()).await.unwrap();
    assert_eq!(chain_rows(&pool, &victim).await, 1);
    assert!(
        !avalon_indexer::identity_chain_store::is_forked(&pool, victim.id)
            .await
            .unwrap()
    );
    assert!(active(&pool, &victim, key_id).await);

    // The same holds for the revocation that follows.
    let signed = revoke(&victim, &victim, inception_id, key_id, Some(&genuine));
    let genuine_revocation = at_position(signed.clone(), 2, Some(&genuine), 300);
    apply(&pool, &genuine_revocation, &game()).await.unwrap();
    let mut replay = at_position(signed, 2, Some(&genuine), 400);
    replay.id = Uuid::new_v4();
    apply(&pool, &replay, &other()).await.unwrap();
    assert_eq!(chain_rows(&pool, &victim).await, 2);
    assert!(
        !avalon_indexer::identity_chain_store::is_forked(&pool, victim.id)
            .await
            .unwrap()
    );
}

#[tokio::test]
#[ignore]
async fn a_variant_delivered_first_does_not_displace_the_genuine_event() {
    let pool = pool().await;
    let (victim, device) = (TestIdentity::new(), TestIdentity::new());
    let inception_id = register(&pool, &victim).await;
    let signed = grant(
        &victim,
        &victim,
        inception_id,
        &device,
        Uuid::new_v4(),
        None,
    );
    let mut variant = at_position(signed.clone(), 1, None, 200);
    variant.id = Uuid::new_v4();
    apply(&pool, &variant, &other()).await.unwrap();
    let genuine = at_position(signed, 1, None, 100);
    apply(&pool, &genuine, &game()).await.unwrap();
    assert_eq!(chain_rows(&pool, &victim).await, 1);
    assert!(
        !avalon_indexer::identity_chain_store::is_forked(&pool, victim.id)
            .await
            .unwrap()
    );
}

#[tokio::test]
#[ignore]
async fn a_signed_key_event_replayed_under_another_key_id_or_position_is_refused() {
    let pool = pool().await;
    let (victim, device) = (TestIdentity::new(), TestIdentity::new());
    let inception_id = register(&pool, &victim).await;
    let key_id = Uuid::new_v4();
    let added = grant(&victim, &victim, inception_id, &device, key_id, None);

    // Another key id, another approver id, or a conflicting position: the signature no longer holds.
    let mut other_key = added.clone();
    other_key.payload["signing_key_id"] = serde_json::json!(Uuid::new_v4());
    let mut other_approver = added.clone();
    other_approver.payload["approved_by_signing_key_id"] = serde_json::json!(Uuid::new_v4());
    let moved = positioned(added.clone(), 2, Some([7; 32]));
    let mut unchained = added.clone();
    unchained.identity_chain = None;
    for replay in [other_key, other_approver, moved, unchained] {
        assert!(matches!(
            apply(&pool, &replay, &game()).await,
            Err(IndexError::Rejected(_)) | Err(IndexError::AwaitingKey(_))
        ));
    }
    assert_eq!(chain_rows(&pool, &victim).await, 0);
    apply(&pool, &added, &game()).await.unwrap();

    let revoked = revoke(&victim, &victim, inception_id, key_id, Some(&added));
    let moved = positioned(revoked.clone(), 3, Some([7; 32]));
    let mut unchained = revoked.clone();
    unchained.identity_chain = None;
    let mut other_target = revoked.clone();
    other_target.payload["signing_key_id"] = serde_json::json!(inception_id);
    for replay in [moved, unchained, other_target] {
        assert!(matches!(
            apply(&pool, &replay, &game()).await,
            Err(IndexError::Rejected(_))
        ));
    }
    apply(&pool, &revoked, &game()).await.unwrap();
    assert!(!active(&pool, &victim, key_id).await);
    assert!(active(&pool, &victim, inception_id).await);
}

#[tokio::test]
#[ignore]
async fn an_alias_published_first_by_another_shard_does_not_block_the_real_key_or_its_revocation() {
    let pool = pool().await;
    let (victim, device) = (TestIdentity::new(), TestIdentity::new());
    let inception_id = register(&pool, &victim).await;
    let real_key = Uuid::new_v4();
    let real = grant(&victim, &victim, inception_id, &device, real_key, None);

    let mut alias = real.clone();
    alias.id = Uuid::new_v4();
    alias.payload["signing_key_id"] = serde_json::json!(Uuid::new_v4());
    assert!(apply(&pool, &alias, &other()).await.is_err());

    apply(&pool, &real, &game()).await.unwrap();
    assert!(active(&pool, &victim, real_key).await);
    apply(
        &pool,
        &revoke(&victim, &victim, inception_id, real_key, Some(&real)),
        &game(),
    )
    .await
    .unwrap();
    assert!(!active(&pool, &victim, real_key).await);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM indexer_identity_signing_keys \
             WHERE identity_id = $1 AND revoked_at IS NULL",
            victim.id
        )
        .await,
        1,
        "only the inception key stays active"
    );
}

#[tokio::test]
#[ignore]
async fn the_inception_key_event_must_name_the_ticket_key_and_stay_unchained() {
    let pool = pool().await;
    let who = TestIdentity::new();
    let creation = created(&who, &unique_name("incep"));
    let ticket = ticket_of(&creation);

    // Ahead of its creation the inception event waits.
    assert!(matches!(
        apply(&pool, &inception(&who, ticket), &other()).await,
        Err(IndexError::AwaitingKey(_))
    ));
    apply(&pool, &creation, &game()).await.unwrap();

    // The identity's own key under another id, or carrying a chain position, is refused from any shard.
    assert!(matches!(
        apply(&pool, &inception(&who, Uuid::new_v4()), &other()).await,
        Err(IndexError::Rejected(_))
    ));
    let positioned_inception = positioned(inception(&who, ticket), 1, None);
    assert!(matches!(
        apply(&pool, &positioned_inception, &other()).await,
        Err(IndexError::Rejected(_))
    ));
    assert_eq!(chain_rows(&pool, &who).await, 0);

    apply(&pool, &inception(&who, ticket), &other())
        .await
        .unwrap();
    assert!(active(&pool, &who, ticket).await);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM indexer_identity_signing_keys WHERE identity_id = $1",
            who.id
        )
        .await,
        1
    );
}

#[tokio::test]
#[ignore]
async fn a_grant_or_revocation_ahead_of_its_signing_key_is_deferred_then_applies() {
    let pool = pool().await;
    let (victim, device, second) = (
        TestIdentity::new(),
        TestIdentity::new(),
        TestIdentity::new(),
    );
    let creation = created(&victim, &unique_name("late"));
    apply(&pool, &creation, &game()).await.unwrap();
    let ticket_key = ticket_of(&creation);
    // A grant approved by a device key arrives ahead of the grant that adds that key.
    let device_key = Uuid::new_v4();
    let later_key = Uuid::new_v4();
    let first = grant(&victim, &victim, ticket_key, &device, device_key, None);
    let second_grant = grant(
        &victim,
        &device,
        device_key,
        &second,
        later_key,
        Some(&first),
    );
    let err = apply(&pool, &second_grant, &game()).await.unwrap_err();
    assert!(err.is_deferred() && !err.is_transient(), "{err:?}");

    // A revocation naming a key that is not projected yet is deferred the same way.
    let early_revoke = revoke(&victim, &second, later_key, device_key, Some(&second_grant));
    assert!(apply(&pool, &early_revoke, &game())
        .await
        .unwrap_err()
        .is_deferred());

    apply(&pool, &first, &game()).await.unwrap();
    apply(&pool, &second_grant, &game()).await.unwrap();
    assert!(active(&pool, &victim, device_key).await);
    assert!(active(&pool, &victim, later_key).await);

    // Once it is known but revoked, it is refused for good.
    let second_revoke = revoke(&victim, &victim, ticket_key, later_key, Some(&second_grant));
    apply(&pool, &second_revoke, &game()).await.unwrap();
    let err = apply(&pool, &early_revoke, &game()).await.unwrap_err();
    assert!(matches!(err, IndexError::Rejected(_)), "{err:?}");
}

#[tokio::test]
#[ignore]
async fn a_child_event_for_an_unknown_identity_is_deferred_not_refused() {
    let pool = pool().await;
    let ghost = TestIdentity::new();
    let passkey = event(
        &ghost,
        "identity.passkey_registered",
        1,
        serde_json::json!({
            "passkey_id": Uuid::new_v4(), "identity_id": ghost.id,
            "credential_id": b64(Uuid::new_v4().as_bytes()),
            "passkey_data": {"k": 1}, "label": null,
        }),
    );
    let err = apply(&pool, &passkey, &core()).await.unwrap_err();
    assert!(err.is_deferred() && !err.is_transient(), "{err:?}");
    apply(&pool, &created(&ghost, &unique_name("g")), &core())
        .await
        .unwrap();
    apply(&pool, &passkey, &core()).await.unwrap();
}

#[tokio::test]
#[ignore]
async fn database_rule_violations_are_classified_not_parked_as_storage_faults() {
    let pool = pool().await;
    let (who, device) = (TestIdentity::new(), TestIdentity::new());
    let inception_id = register(&pool, &who).await;
    let key = Uuid::new_v4();
    apply(
        &pool,
        &grant(&who, &who, inception_id, &device, key, None),
        &game(),
    )
    .await
    .unwrap();
    // The same public key under another id, bypassing the pre-check.
    let duplicate = sqlx::query(
        "INSERT INTO indexer_identity_signing_keys (signing_key_id, identity_id, public_key, added_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(Uuid::new_v4())
    .bind(who.id)
    .bind(device.public_key().to_vec())
    .execute(&pool)
    .await
    .unwrap_err();
    let err = IndexError::from(duplicate);
    assert!(matches!(err, IndexError::Rejected(_)), "{err:?}");
    assert!(!err.is_transient() && !err.is_deferred());
    // A row for an identity that does not exist waits for it.
    let orphan = sqlx::query(
        "INSERT INTO indexer_identity_signing_keys (signing_key_id, identity_id, public_key, added_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(Uuid::new_v4())
    .bind(TestIdentity::new().id)
    .bind(device.public_key().to_vec())
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(IndexError::from(orphan).is_deferred());
}

#[tokio::test]
#[ignore]
async fn another_networks_core_shard_is_not_authoritative() {
    let pool = pool().await;
    let who = TestIdentity::new();
    register(&pool, &who).await;
    let passkey = event(
        &who,
        "identity.passkey_registered",
        1,
        serde_json::json!({
            "passkey_id": Uuid::new_v4(), "identity_id": who.id,
            "credential_id": b64(Uuid::new_v4().as_bytes()),
            "passkey_data": {"k": 1}, "label": null,
        }),
    );
    let foreign_core = EventOrigin::mirrored("another-network", "core");
    assert!(matches!(
        apply(&pool, &passkey, &foreign_core).await,
        Err(IndexError::Rejected(_))
    ));
    apply(&pool, &passkey, &core()).await.unwrap();
}

#[tokio::test]
#[ignore]
async fn one_shards_copy_of_an_event_id_does_not_suppress_another_shards() {
    let pool = pool().await;
    let who = TestIdentity::new();
    register(&pool, &who).await;
    let update = event(
        &who,
        "profile.updated",
        1,
        serde_json::json!({ "bio": "first" }),
    );
    let local_game = EventOrigin::local(TEST_NETWORK_ID, "game:slug/1");
    apply(&pool, &update, &local_game).await.unwrap();
    // The same event id arriving from core is still applied (and idempotently so).
    apply(&pool, &update, &core()).await.unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM indexer_applied_events WHERE event_id::text = $1",
            update.id
        )
        .await,
        2
    );
    // The same shard's redelivery is a no-op.
    apply(&pool, &update, &core()).await.unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM indexer_applied_events WHERE event_id::text = $1",
            update.id
        )
        .await,
        2
    );
}

#[tokio::test]
#[ignore]
async fn pre_claiming_every_candidate_name_cannot_keep_an_identity_out() {
    let pool = pool().await;
    let (holder, newcomer) = (TestIdentity::new(), TestIdentity::new());
    let name = unique_name("squat");
    apply(&pool, &created(&holder, &name), &game())
        .await
        .unwrap();
    // Every name the newcomer could be given, claimed ahead of its creation.
    let mut taken = Vec::new();
    for len in (12..=60).step_by(4) {
        taken.push(
            avalon_indexer::projections::profiles::disambiguated_display_name(
                &name,
                &newcomer.id,
                len,
            ),
        );
    }
    for candidate in &taken {
        apply(&pool, &created(&TestIdentity::new(), candidate), &game())
            .await
            .unwrap();
    }
    apply(&pool, &created(&newcomer, &name), &game())
        .await
        .unwrap();
    assert_eq!(
        profile_name(&pool, &newcomer).await,
        Some(newcomer.id.to_string())
    );
    // Ordinary users cannot claim an id-shaped name, so the fallback stays free.
    assert!(!avalon_protocol::identity_id::display_name_permitted(
        &newcomer.id.to_string()
    ));
}
