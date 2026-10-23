//! Live tests (`--ignored`, migrated Postgres) for projection-time verification of identity
//! events: forged or foreign events are refused, leave nothing behind, and creations never
//! overwrite or merge.

mod support;

use avalon_indexer::identity_proof::EventOrigin;
use avalon_indexer::postgres::PostgresIndexer;
use avalon_indexer::IndexError;
use avalon_protocol::event_payloads::IdentityRecoveryRequestedPayload;
use avalon_protocol::events::{IdentityChainPosition, ProtocolEvent};
use avalon_protocol::identity_chain_wire::{embed_position, event_hash};
use avalon_protocol::identity_id::{
    device_grant_approval_signing_bytes, recovery_approval_signing_bytes,
    recovery_request_signing_bytes, recovery_withdrawal_signing_bytes,
    signing_key_revoked_signing_bytes, RecoveryApprovalBinding, TestIdentity, TEST_NETWORK_ID,
    TEST_SHARD_ID,
};
use avalon_protocol::ids::{GlobalId, IdentityId};
use avalon_protocol::ledger_entry::payload_hash;
use avalon_protocol::signing_bytes::HashAlgo;
use base64::Engine as _;
use ed25519_dalek::Signer as _;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use support::{attestation, heads};
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
        .with_head_attestor(heads())
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

/// A chained event authored by `who` with key `key_id` at the first chain position.
fn authored(
    who: &TestIdentity,
    key_id: Uuid,
    kind: &str,
    payload: serde_json::Value,
) -> ProtocolEvent {
    let mut e = positioned(event(who, kind, 1, payload), 1, None);
    who.sign_event(&mut e, key_id);
    e
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

    // A re-creation naming the same name under the same ticket changes nothing; another ticket
    // would give nodes different inception key ids and is refused.
    let mut again = first.clone();
    again.id = Uuid::new_v4();
    apply(&pool, &again, &core()).await.unwrap();
    assert_eq!(profile_name(&pool, &who).await, Some(name.clone()));
    assert!(matches!(
        apply(&pool, &created(&who, &name), &core()).await,
        Err(IndexError::Rejected(_))
    ));
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
async fn a_chained_event_is_accepted_from_any_shard_only_with_the_authors_signature() {
    let pool = pool().await;
    let (who, other_identity) = (TestIdentity::new(), TestIdentity::new());
    let key = register(&pool, &who).await;
    let passkey = |label: &str| {
        let payload = serde_json::json!({
            "passkey_id": Uuid::new_v4(), "identity_id": who.id,
            "credential_id": b64(Uuid::new_v4().as_bytes()),
            "passkey_data": {"k": 1}, "label": label,
        });
        authored(&who, key, "identity.passkey_registered", payload)
    };
    let count_passkeys = || {
        count(
            &pool,
            "SELECT count(*) FROM indexer_identity_passkeys WHERE identity_id = $1",
            who.id,
        )
    };

    // Unsigned, signed by another identity's key, or signed for another event: refused.
    let mut unsigned = passkey("unsigned");
    unsigned.identity_chain = Some(IdentityChainPosition::current(1, None));
    let mut foreign_signer = passkey("foreign");
    other_identity.sign_event(&mut foreign_signer, key);
    let mut tampered = passkey("tampered");
    tampered.payload["label"] = serde_json::json!("changed");
    for forged in [&unsigned, &foreign_signer, &tampered] {
        assert!(matches!(
            apply(&pool, forged, &game()).await,
            Err(IndexError::Rejected(_))
        ));
    }
    let recovered = authored(
        &who,
        Uuid::new_v4(),
        "identity.recovered",
        serde_json::json!({ "request_id": Uuid::nil() }),
    );
    assert!(apply(&pool, &recovered, &game()).await.is_err());
    assert_eq!(count_passkeys().await, 0);
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

    // The authored event is accepted from a game shard, core and the local shard alike.
    apply(&pool, &passkey("game"), &game()).await.unwrap();
    apply(&pool, &passkey("core"), &core()).await.unwrap();
    apply(
        &pool,
        &passkey("local"),
        &EventOrigin::local(TEST_NETWORK_ID, "game:slug/1"),
    )
    .await
    .unwrap();
    assert_eq!(count_passkeys().await, 3);
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
            Err(IndexError::Rejected(_)) | Err(IndexError::AwaitingKey(_))
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
    let late_revoke = revoke(
        &victim,
        &second,
        later_key,
        device_key,
        Some(&second_revoke),
    );
    let err = apply(&pool, &late_revoke, &game()).await.unwrap_err();
    assert!(matches!(err, IndexError::Rejected(_)), "{err:?}");
}

#[tokio::test]
#[ignore]
async fn a_child_event_for_an_unknown_identity_is_deferred_not_refused() {
    let pool = pool().await;
    let ghost = TestIdentity::new();
    let creation = created(&ghost, &unique_name("g"));
    let key_id = ticket_of(&creation);
    let passkey = authored(
        &ghost,
        key_id,
        "identity.passkey_registered",
        serde_json::json!({
            "passkey_id": Uuid::new_v4(), "identity_id": ghost.id,
            "credential_id": b64(Uuid::new_v4().as_bytes()),
            "passkey_data": {"k": 1}, "label": null,
        }),
    );
    let err = apply(&pool, &passkey, &core()).await.unwrap_err();
    assert!(matches!(err, IndexError::AwaitingKey(_)), "{err:?}");
    assert!(err.is_deferred() && !err.is_transient(), "{err:?}");
    apply(&pool, &creation, &core()).await.unwrap();
    apply(&pool, &inception(&ghost, key_id), &core())
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
        "INSERT INTO indexer_identity_signing_keys \
         (signing_key_id, identity_id, public_key, added_at, inception) \
         VALUES ($1, $2, $3, now(), true)",
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
        "INSERT INTO indexer_identity_signing_keys \
         (signing_key_id, identity_id, public_key, added_at, inception) \
         VALUES ($1, $2, $3, now(), true)",
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
async fn a_signature_made_for_another_network_is_refused() {
    let pool = pool().await;
    let who = TestIdentity::new();
    let key = register(&pool, &who).await;
    let passkey = authored(
        &who,
        key,
        "identity.passkey_registered",
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
    let key = register(&pool, &who).await;
    let update = authored(
        &who,
        key,
        "profile.updated",
        serde_json::json!({ "bio": "first" }),
    );
    let local_game = EventOrigin::local(TEST_NETWORK_ID, "game:slug/1");
    apply(&pool, &update, &local_game).await.unwrap();
    // The same chained event arriving from core is the same claim, however many shards deliver it.
    apply(&pool, &update, &core()).await.unwrap();
    apply(&pool, &update, &core()).await.unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM indexer_applied_events WHERE event_id::text = $1",
            update.id
        )
        .await,
        1
    );

    // An unchained event is claimed per shard: one shard's copy never suppresses another's.
    let who = TestIdentity::new();
    let creation = created(&who, &unique_name("claims"));
    apply(&pool, &creation, &game()).await.unwrap();
    apply(&pool, &creation, &core()).await.unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM indexer_applied_events WHERE event_id::text = $1",
            creation.id
        )
        .await,
        2
    );
}

#[tokio::test]
#[ignore]
async fn a_chained_event_id_cannot_be_pre_claimed_by_another_identity() {
    let pool = pool().await;
    let (victim, attacker) = (TestIdentity::new(), TestIdentity::new());
    let victim_key = register(&pool, &victim).await;
    let attacker_key = register(&pool, &attacker).await;
    // The attacker authors a genuinely signed event of their own under the id the victim will use.
    let squatted = authored(
        &attacker,
        attacker_key,
        "profile.updated",
        serde_json::json!({ "bio": "squatting" }),
    );
    apply(&pool, &squatted, &game()).await.unwrap();
    let mut genuine = authored(
        &victim,
        victim_key,
        "profile.updated",
        serde_json::json!({ "bio": "mine" }),
    );
    genuine.id = squatted.id;
    victim.sign_event(&mut genuine, victim_key);
    apply(&pool, &genuine, &game()).await.unwrap();
    assert_eq!(chain_rows(&pool, &victim).await, 1);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM indexer_applied_events WHERE event_id::text = $1",
            squatted.id
        )
        .await,
        2,
        "each distinct chained event holds its own claim"
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

#[tokio::test]
#[ignore]
async fn a_relabelled_key_event_cannot_squat_another_events_id() {
    let pool = pool().await;
    let (victim, device, second) = (
        TestIdentity::new(),
        TestIdentity::new(),
        TestIdentity::new(),
    );
    let inception_id = register(&pool, &victim).await;
    let first = grant(
        &victim,
        &victim,
        inception_id,
        &device,
        Uuid::new_v4(),
        None,
    );
    let second_key = Uuid::new_v4();
    let next = grant(
        &victim,
        &victim,
        inception_id,
        &second,
        second_key,
        Some(&first),
    );

    // The second grant's content arrives first, under the first grant's event id, from another shard.
    let mut relabelled = next.clone();
    relabelled.id = first.id;
    let _ = apply(&pool, &relabelled, &other()).await;
    // The genuine first grant still lands, then the genuine second one (a copy of held content).
    apply(&pool, &first, &game()).await.unwrap();
    apply(&pool, &next, &game()).await.unwrap();
    assert_eq!(chain_rows(&pool, &victim).await, 2);
    assert!(active(&pool, &victim, first_key_id(&first)).await);
    assert!(active(&pool, &victim, second_key).await);
}

fn first_key_id(grant: &ProtocolEvent) -> Uuid {
    serde_json::from_value(grant.payload["signing_key_id"].clone()).unwrap()
}

#[tokio::test]
#[ignore]
async fn a_redelivered_inception_event_keeps_the_first_label() {
    let pool = pool().await;
    let who = TestIdentity::new();
    let creation = created(&who, &unique_name("label"));
    apply(&pool, &creation, &game()).await.unwrap();
    let key_id = ticket_of(&creation);
    let labelled = |label: &str| {
        let mut e = inception(&who, key_id);
        e.payload["device_label"] = serde_json::json!(label);
        e
    };
    apply(&pool, &labelled("first"), &game()).await.unwrap();
    apply(&pool, &labelled("second"), &other()).await.unwrap();
    let label: Option<String> = sqlx::query_scalar(
        "SELECT label FROM indexer_identity_signing_keys WHERE identity_id = $1 AND signing_key_id = $2",
    )
    .bind(who.id)
    .bind(key_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(label.as_deref(), Some("first"));
}

/// An owner with three registered guardians and a signed 2-of-3 `identity.recovery_configured`
/// that cosigned heads covered long enough ago.
struct RecoveryFixture {
    owner: TestIdentity,
    owner_key: Uuid,
    guardians: Vec<(TestIdentity, Uuid)>,
    configured: ProtocolEvent,
}

/// Records `configured`'s ledger entry with the latest head `hours` after the first covering one.
fn attest_config(configured: &ProtocolEvent, hours: i64) {
    let position = configured.identity_chain.as_ref().unwrap();
    let stored = payload_hash(&embed_position(&configured.payload, position)).unwrap();
    heads().put(
        hex::encode(event_hash(configured).unwrap()),
        attestation(
            &configured.kind,
            configured.id,
            configured.subject.to_string(),
            stored,
            hours,
        ),
    );
}

async fn recovery_fixture(pool: &PgPool) -> RecoveryFixture {
    let owner = TestIdentity::new();
    let owner_key = register(pool, &owner).await;
    let mut guardians = Vec::new();
    for _ in 0..3 {
        let g = TestIdentity::new();
        let key = register(pool, &g).await;
        guardians.push((g, key));
    }
    let configured = authored(
        &owner,
        owner_key,
        "identity.recovery_configured",
        serde_json::json!({
            "guardian_ids": guardians.iter().map(|(g, _)| g.id).collect::<Vec<_>>(),
            "threshold": 2,
        }),
    );
    apply(pool, &configured, &game()).await.unwrap();
    attest_config(&configured, 100);
    RecoveryFixture {
        owner,
        owner_key,
        guardians,
        configured,
    }
}

/// A recovery request naming `config` (the owner's configuration event) and its marker entry.
struct Request {
    id: Uuid,
    new_key: TestIdentity,
    marker: IdentityRecoveryRequestedPayload,
    entry_hash: [u8; 32],
}

/// Records the request marker's ledger entry with the latest head `hours` after the first covering one.
fn attest_request(f: &RecoveryFixture, request: &Request, hours: i64) {
    heads().put(
        hex::encode(request.entry_hash),
        attestation(
            "identity.recovery_requested",
            Uuid::new_v4(),
            GlobalId::new(
                "identity",
                &f.owner.id.to_string(),
                "self",
                "recovery_requested",
            )
            .to_string(),
            payload_hash(&serde_json::to_value(&request.marker).unwrap()).unwrap(),
            hours,
        ),
    );
}

fn request_for(f: &RecoveryFixture, config: &ProtocolEvent) -> Request {
    let new_key = TestIdentity::new();
    let id = Uuid::new_v4();
    let config_hash = event_hash(config).unwrap();
    let bytes = recovery_request_signing_bytes(
        TEST_NETWORK_ID,
        &f.owner.id,
        id,
        &new_key.public_key(),
        &config_hash,
    );
    let request = Request {
        id,
        marker: IdentityRecoveryRequestedPayload {
            request_id: id,
            threshold: 2,
            new_signing_public_key: b64(&new_key.public_key()),
            config_event_hash: hex::encode(config_hash),
            signature: b64(&new_key.signing_key.sign(&bytes).to_bytes()),
        },
        entry_hash: HashAlgo::Sha256.digest(id.as_bytes()),
        new_key,
    };
    attest_request(f, &request, 100);
    request
}

fn request(f: &RecoveryFixture) -> Request {
    request_for(f, &f.configured)
}

type Position = (u64, Option<[u8; 32]>);

fn approval_for(
    owner: &IdentityId,
    request: &Request,
    guardian: &(TestIdentity, Uuid),
    (seq, prev): Position,
) -> serde_json::Value {
    approval_as(owner, request, &guardian.0.id, guardian, (seq, prev))
}

/// An approval naming `guardian_id`, signed by `signer` (a key of that guardian).
fn approval_as(
    owner: &IdentityId,
    request: &Request,
    guardian_id: &IdentityId,
    signer: &(TestIdentity, Uuid),
    (seq, prev): Position,
) -> serde_json::Value {
    let guardian = signer;
    let config_hash = hex::decode(&request.marker.config_event_hash)
        .unwrap()
        .try_into()
        .unwrap();
    let bytes = recovery_approval_signing_bytes(
        TEST_NETWORK_ID,
        owner,
        request.id,
        guardian_id,
        guardian.1,
        &request.new_key.public_key(),
        &RecoveryApprovalBinding {
            request_entry_hash: &request.entry_hash,
            config_event_hash: &config_hash,
            guardian_seq: seq,
            guardian_prev_hash: prev.as_ref(),
        },
    );
    let mut proof = serde_json::json!({
        "guardian_id": guardian_id,
        "signing_key_id": guardian.1,
        "seq": seq,
        "signature": b64(&guardian.0.signing_key.sign(&bytes).to_bytes()),
    });
    if let Some(prev) = prev {
        proof["prev_hash"] = serde_json::json!(hex::encode(prev));
    }
    proof
}

/// A guardian's approval at the start of their chain (position 1).
fn approval(
    f: &RecoveryFixture,
    request: &Request,
    guardian: &(TestIdentity, Uuid),
) -> serde_json::Value {
    approval_for(&f.owner.id, request, guardian, (1, None))
}

/// `proof` with a withdrawal signed by `signer` under `key_id` at the guardian's `position`.
fn withdrawn(
    f: &RecoveryFixture,
    request: &Request,
    mut proof: serde_json::Value,
    guardian: &TestIdentity,
    signer: &TestIdentity,
    key_id: Uuid,
    (seq, prev): Position,
) -> serde_json::Value {
    let bytes = recovery_withdrawal_signing_bytes(
        TEST_NETWORK_ID,
        &f.owner.id,
        request.id,
        &guardian.id,
        key_id,
        seq,
        prev.as_ref(),
    );
    let mut withdrawal = serde_json::json!({
        "signing_key_id": key_id,
        "seq": seq,
        "signature": b64(&signer.signing_key.sign(&bytes).to_bytes()),
    });
    if let Some(prev) = prev {
        withdrawal["prev_hash"] = serde_json::json!(hex::encode(prev));
    }
    proof["withdrawal"] = withdrawal;
    proof
}

/// `identity.recovered` at the chain position after `after`, signed by the new key.
fn recovered_after(
    f: &RecoveryFixture,
    request: &Request,
    approvals: Vec<serde_json::Value>,
    after: &ProtocolEvent,
) -> ProtocolEvent {
    let (seq, prev) = next_position(Some(after));
    let mut e = positioned(
        event(
            &f.owner,
            "identity.recovered",
            1,
            serde_json::json!({
                "request_id": request.id,
                "device_label": null,
                "new_signing_public_key": b64(&request.new_key.public_key()),
                "request": request.marker,
                "request_entry_hash": hex::encode(request.entry_hash),
                "approvals": approvals,
            }),
        ),
        seq,
        prev,
    );
    request.new_key.sign_event(&mut e, request.id);
    e
}

fn recovered(
    f: &RecoveryFixture,
    request: &Request,
    approvals: Vec<serde_json::Value>,
) -> ProtocolEvent {
    recovered_after(f, request, approvals, &f.configured)
}

fn is_refusal(result: Result<(), IndexError>) -> bool {
    matches!(result, Err(IndexError::Rejected(_)))
}

fn is_wait(result: Result<(), IndexError>) -> bool {
    matches!(result, Err(ref e) if e.is_deferred())
}

#[tokio::test]
#[ignore]
async fn recovered_needs_the_threshold_of_valid_guardian_approvals_for_this_request() {
    let pool = pool().await;
    let f = recovery_fixture(&pool).await;
    let req = request(&f);
    let owner = f.owner.id;
    let good = |i: usize| approval(&f, &req, &f.guardians[i]);

    // Below the threshold.
    let one = recovered(&f, &req, vec![good(0)]);
    assert!(is_refusal(apply(&pool, &one, &game()).await));
    // The same guardian twice is one approval.
    let twice = recovered(&f, &req, vec![good(0), good(0)]);
    assert!(is_refusal(apply(&pool, &twice, &game()).await));
    // Approvals of a different request.
    let other_request = request(&f);
    let wrong_request = recovered(
        &f,
        &req,
        (0..2)
            .map(|i| approval(&f, &other_request, &f.guardians[i]))
            .collect(),
    );
    assert!(is_refusal(apply(&pool, &wrong_request, &game()).await));
    // Approvals of a recovery to another key.
    let mut other_key = request(&f);
    other_key.id = req.id;
    let wrong_key = recovered(
        &f,
        &req,
        (0..2)
            .map(|i| approval(&f, &other_key, &f.guardians[i]))
            .collect(),
    );
    assert!(is_refusal(apply(&pool, &wrong_key, &game()).await));
    // Approvals replayed from the recovery of another identity.
    let victim_of_replay = TestIdentity::new();
    let replayed = recovered(
        &f,
        &req,
        (0..2)
            .map(|i| approval_for(&victim_of_replay.id, &req, &f.guardians[i], (1, None)))
            .collect(),
    );
    assert!(is_refusal(apply(&pool, &replayed, &game()).await));
    // Approvals of another request marker entry or another configuration.
    let mut other_entry = request(&f);
    other_entry.id = req.id;
    other_entry.new_key = TestIdentity::from_seed(req.new_key.signing_key.to_bytes());
    other_entry.marker = req.marker.clone();
    let wrong_entry = recovered(
        &f,
        &req,
        (0..2)
            .map(|i| approval(&f, &other_entry, &f.guardians[i]))
            .collect(),
    );
    assert!(is_refusal(apply(&pool, &wrong_entry, &game()).await));
    // A signer that is not one of the configured guardians.
    let stranger_identity = TestIdentity::new();
    let stranger_key = register(&pool, &stranger_identity).await;
    let stranger = (stranger_identity, stranger_key);
    let not_a_guardian = recovered(&f, &req, vec![good(0), approval(&f, &req, &stranger)]);
    assert!(is_refusal(apply(&pool, &not_a_guardian, &game()).await));
    // Not signed by the new key it introduces.
    let mut not_pop = recovered(&f, &req, vec![good(0), good(1)]);
    f.owner.sign_event(&mut not_pop, f.owner_key);
    assert!(apply(&pool, &not_pop, &game()).await.is_err());
    // A request the new key did not sign.
    let mut forged_request = req.marker.clone();
    forged_request.signature = b64(&[7u8; 64]);
    let mut forged = recovered(&f, &req, vec![good(0), good(1)]);
    forged.payload["request"] = serde_json::to_value(&forged_request).unwrap();
    req.new_key.sign_event(&mut forged, req.id);
    assert!(is_refusal(apply(&pool, &forged, &game()).await));
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM identity_chain_events WHERE identity_id = $1 AND seq = 2",
            owner
        )
        .await,
        0,
        "no refused recovery reaches the chain"
    );

    // Two valid approvals recover the identity from any shard: the new key replaces the old.
    let ok = recovered(&f, &req, vec![good(0), good(2)]);
    apply(&pool, &ok, &game()).await.unwrap();
    assert!(active(&pool, &f.owner, req.id).await);
    assert!(!active(&pool, &f.owner, f.owner_key).await);
}

#[tokio::test]
#[ignore]
async fn the_delay_runs_between_cosigned_heads_never_on_the_nodes_clock() {
    let pool = pool().await;
    let f = recovery_fixture(&pool).await;
    let req = request(&f);
    let approvals = || {
        vec![
            approval(&f, &req, &f.guardians[0]),
            approval(&f, &req, &f.guardians[1]),
        ]
    };
    let e = recovered(&f, &req, approvals());

    // The latest cosigned head is not yet a full delay after the one that first covered the marker.
    attest_request(&f, &req, 47);
    assert!(is_wait(apply(&pool, &e, &game()).await));
    // No cosigned head covers the marker at all.
    let mut uncovered = attestation(
        "identity.recovery_requested",
        Uuid::new_v4(),
        GlobalId::new(
            "identity",
            &f.owner.id.to_string(),
            "self",
            "recovery_requested",
        )
        .to_string(),
        payload_hash(&serde_json::to_value(&req.marker).unwrap()).unwrap(),
        100,
    );
    uncovered.anchor_at = None;
    heads().put(hex::encode(req.entry_hash), uncovered);
    assert!(is_wait(apply(&pool, &e, &game()).await));
    // The ledger entry is not held at all.
    heads().put(
        hex::encode(req.entry_hash),
        attestation(
            "identity.recovery_requested",
            Uuid::new_v4(),
            String::new(),
            [0; 32],
            100,
        ),
    );
    assert!(is_refusal(apply(&pool, &e, &game()).await));
    // A recovery configuration younger than the delay holds the recovery back too.
    attest_request(&f, &req, 100);
    attest_config(&f.configured, 10);
    assert!(is_wait(apply(&pool, &e, &game()).await));
    assert_eq!(
        chain_rows(&pool, &f.owner).await,
        1,
        "a waiting recovery leaves nothing"
    );

    // Once cosigned heads show the delay has run, the same event applies.
    attest_config(&f.configured, 100);
    apply(&pool, &e, &game()).await.unwrap();
    assert!(active(&pool, &f.owner, req.id).await);
}

#[tokio::test]
#[ignore]
async fn an_event_held_on_several_shards_is_timed_by_its_earliest_cosigned_copy() {
    let pool = pool().await;
    let f = recovery_fixture(&pool).await;
    let req = request(&f);
    let e = recovered(
        &f,
        &req,
        vec![
            approval(&f, &req, &f.guardians[0]),
            approval(&f, &req, &f.guardians[1]),
        ],
    );
    // Another shard republished the configuration and its head was cosigned only recently; this copy
    // is listed before the genuine one.
    let position = f.configured.identity_chain.as_ref().unwrap();
    let stored = payload_hash(&embed_position(&f.configured.payload, position)).unwrap();
    let late = OffsetDateTime::now_utc() - time::Duration::hours(10);
    heads().put(
        format!("late-copy-{}", f.configured.id),
        avalon_indexer::head_attestor::EntryAttestation {
            kind: f.configured.kind.clone(),
            event_id: f.configured.id,
            subject: f.configured.subject.to_string(),
            payload_hash: hex::encode(stored),
            anchor_at: Some(late),
            reference_at: Some(late + time::Duration::hours(5)),
        },
    );
    attest_config(&f.configured, 100);
    apply(&pool, &e, &game()).await.unwrap();
    assert!(active(&pool, &f.owner, req.id).await);
}

#[tokio::test]
#[ignore]
async fn a_recovery_must_rely_on_the_owners_current_configuration() {
    let pool = pool().await;
    let f = recovery_fixture(&pool).await;
    let stale = request(&f);

    // The owner reconfigures after the request was made.
    let reconfigured = positioned(
        event(
            &f.owner,
            "identity.recovery_configured",
            1,
            serde_json::json!({
                "guardian_ids": f.guardians.iter().map(|(g, _)| g.id).collect::<Vec<_>>(),
                "threshold": 3,
            }),
        ),
        2,
        Some(event_hash(&f.configured).unwrap()),
    );
    let mut reconfigured = reconfigured;
    f.owner.sign_event(&mut reconfigured, f.owner_key);
    apply(&pool, &reconfigured, &game()).await.unwrap();

    let approvals =
        |r: &Request| -> Vec<_> { (0..3).map(|i| approval(&f, r, &f.guardians[i])).collect() };
    let relies_on_old = recovered_after(&f, &stale, approvals(&stale), &reconfigured);
    assert!(is_refusal(apply(&pool, &relies_on_old, &game()).await));

    // A request on the new configuration waits until that configuration is old enough.
    let fresh = request_for(&f, &reconfigured);
    let e = recovered_after(&f, &fresh, approvals(&fresh), &reconfigured);
    attest_config(&reconfigured, 10);
    assert!(is_wait(apply(&pool, &e, &game()).await));
    attest_config(&reconfigured, 100);
    // The threshold the pinned configuration states (3) is what counts.
    let short = recovered_after(
        &f,
        &fresh,
        approvals(&fresh).into_iter().take(2).collect(),
        &reconfigured,
    );
    assert!(is_refusal(apply(&pool, &short, &game()).await));
    apply(&pool, &e, &game()).await.unwrap();
    assert!(active(&pool, &f.owner, fresh.id).await);
}

#[tokio::test]
#[ignore]
async fn the_owners_cancel_vetoes_a_recovery() {
    let pool = pool().await;
    let f = recovery_fixture(&pool).await;
    let req = request(&f);
    let approvals = vec![
        approval(&f, &req, &f.guardians[0]),
        approval(&f, &req, &f.guardians[1]),
    ];
    let cancel = |after: &ProtocolEvent| {
        let (seq, prev) = next_position(Some(after));
        let mut e = positioned(
            event(
                &f.owner,
                "identity.recovery_cancelled",
                1,
                serde_json::json!({"request_id": req.id, "cancelled_by": f.owner.id}),
            ),
            seq,
            prev,
        );
        f.owner.sign_event(&mut e, f.owner_key);
        e
    };

    // A cancel below the recovery makes it invalid.
    let below = cancel(&f.configured);
    apply(&pool, &below, &game()).await.unwrap();
    let e = recovered_after(&f, &req, approvals.clone(), &below);
    assert!(is_refusal(apply(&pool, &e, &game()).await));
    assert!(active(&pool, &f.owner, f.owner_key).await);

    // A cancel only the guardians or a stranger signed does not count: the owner's key signs it.
    let stranger = TestIdentity::new();
    let mut forged = cancel(&f.configured);
    forged.id = Uuid::new_v4();
    stranger.sign_event(&mut forged, f.owner_key);
    assert!(apply(&pool, &forged, &game()).await.is_err());
}

#[tokio::test]
#[ignore]
async fn a_cancel_at_the_recoverys_position_displaces_it_and_restores_the_old_keys() {
    let pool = pool().await;
    let f = recovery_fixture(&pool).await;
    let req = request(&f);
    let e = recovered(
        &f,
        &req,
        vec![
            approval(&f, &req, &f.guardians[0]),
            approval(&f, &req, &f.guardians[1]),
        ],
    );
    apply(&pool, &e, &game()).await.unwrap();
    assert!(active(&pool, &f.owner, req.id).await);
    assert!(!active(&pool, &f.owner, f.owner_key).await);

    // The owner's cancel at the same position arrives from another shard afterwards.
    let (seq, prev) = next_position(Some(&f.configured));
    let mut cancel = positioned(
        event(
            &f.owner,
            "identity.recovery_cancelled",
            1,
            serde_json::json!({"request_id": req.id, "cancelled_by": f.owner.id}),
        ),
        seq,
        prev,
    );
    f.owner.sign_event(&mut cancel, f.owner_key);
    apply(&pool, &cancel, &other()).await.unwrap();

    assert!(!active(&pool, &f.owner, req.id).await);
    assert!(active(&pool, &f.owner, f.owner_key).await);
    let state = avalon_indexer::identity_chain_store::state(&pool, f.owner.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.forked_at_seq, None);
    assert_eq!(
        state.head_hash,
        Some(hex::encode(event_hash(&cancel).unwrap()))
    );
}

#[tokio::test]
#[ignore]
async fn a_guardians_signed_withdrawal_inside_the_proof_withdraws_the_approval() {
    let pool = pool().await;
    let f = recovery_fixture(&pool).await;
    let req = request(&f);
    let g0 = &f.guardians[0];
    let plain = approval(&f, &req, &f.guardians[1]);

    // A withdrawal signed by the guardian's key at their position: the approval no longer counts.
    let gone = withdrawn(
        &f,
        &req,
        approval(&f, &req, g0),
        &g0.0,
        &g0.0,
        g0.1,
        (1, None),
    );
    let e = recovered(&f, &req, vec![gone, plain.clone()]);
    assert!(is_refusal(apply(&pool, &e, &game()).await));

    // A withdrawal someone else signed is no withdrawal.
    let impostor = TestIdentity::new();
    let forged = withdrawn(
        &f,
        &req,
        approval(&f, &req, g0),
        &g0.0,
        &impostor,
        g0.1,
        (1, None),
    );
    let e = recovered(&f, &req, vec![forged, plain]);
    apply(&pool, &e, &game()).await.unwrap();
}

#[tokio::test]
#[ignore]
async fn a_guardians_key_is_judged_at_the_chain_position_of_their_approval() {
    let pool = pool().await;
    let f = recovery_fixture(&pool).await;
    let req = request(&f);
    let (g, g_key) = (&f.guardians[0].0, f.guardians[0].1);
    let device = TestIdentity::new();
    let device_key = Uuid::new_v4();
    let add = grant(g, g, g_key, &device, device_key, None);
    apply(&pool, &add, &game()).await.unwrap();
    let revocation = revoke(g, g, g_key, device_key, Some(&add));
    let device_guardian = (device, device_key);
    let other_good = approval(&f, &req, &f.guardians[1]);
    let at = |position: Position| approval_as(&f.owner.id, &req, &g.id, &device_guardian, position);
    let try_with = |proof: serde_json::Value| recovered(&f, &req, vec![proof, other_good.clone()]);

    // The device key was not added below position 1; at position 2 it was.
    let early = try_with(at((1, None)));
    assert!(is_refusal(apply(&pool, &early, &game()).await));
    let good_position = try_with(at(next_position(Some(&add))));
    // The guardian then revokes it at position 2, and a later position no longer counts it.
    apply(&pool, &revocation, &game()).await.unwrap();
    let after_revocation = try_with(at(next_position(Some(&revocation))));
    assert!(is_refusal(apply(&pool, &after_revocation, &game()).await));
    // A position that does not extend the guardian's accepted chain never counts.
    let off_chain = try_with(at((2, Some([9; 32]))));
    assert!(is_refusal(apply(&pool, &off_chain, &game()).await));
    // An approval judged below the revocation still counts, however late it is delivered.
    apply(&pool, &good_position, &game()).await.unwrap();
    assert!(active(&pool, &f.owner, req.id).await);
}

#[tokio::test]
#[ignore]
async fn a_guardian_chain_not_accepted_far_enough_makes_the_recovery_wait() {
    let pool = pool().await;
    let f = recovery_fixture(&pool).await;
    let req = request(&f);
    let far = approval_for(&f.owner.id, &req, &f.guardians[1], (5, Some([3; 32])));
    let e = recovered(&f, &req, vec![approval(&f, &req, &f.guardians[0]), far]);
    assert!(is_wait(apply(&pool, &e, &game()).await));
    // Enough approvals that stand on their own do not wait for the rest.
    let ok = recovered(
        &f,
        &req,
        vec![
            approval(&f, &req, &f.guardians[0]),
            approval(&f, &req, &f.guardians[2]),
            approval_for(&f.owner.id, &req, &f.guardians[1], (5, Some([3; 32]))),
        ],
    );
    apply(&pool, &ok, &game()).await.unwrap();
}

#[tokio::test]
#[ignore]
async fn a_guardian_key_cannot_sign_anything_on_the_owners_chain() {
    let pool = pool().await;
    let f = recovery_fixture(&pool).await;
    let guardian = &f.guardians[0];
    for (kind, payload) in [
        (
            "friend.requested",
            serde_json::json!({"from": f.owner.id, "to": guardian.0.id, "actor": f.owner.id}),
        ),
        (
            "guild.member_added",
            serde_json::json!({"guild_id": Uuid::new_v4(), "identity_id": f.owner.id}),
        ),
        ("profile.updated", serde_json::json!({"bio": "hijacked"})),
        (
            "identity.recovery_configured",
            serde_json::json!({"guardian_ids": [guardian.0.id], "threshold": 1}),
        ),
        (
            "identity.recovery_cancelled",
            serde_json::json!({"request_id": Uuid::new_v4(), "cancelled_by": guardian.0.id}),
        ),
    ] {
        let mut e = positioned(
            event(&f.owner, kind, 1, payload),
            2,
            Some(event_hash(&f.configured).unwrap()),
        );
        // Under the guardian's own key id, and under the owner's key id.
        for key_id in [guardian.1, f.owner_key] {
            guardian.0.sign_event(&mut e, key_id);
            assert!(apply(&pool, &e, &game()).await.is_err(), "{kind}");
        }
    }
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM identity_chain_events WHERE identity_id = $1 AND seq = 2",
            f.owner.id
        )
        .await,
        0
    );
}

#[tokio::test]
#[ignore]
async fn a_recovered_cannot_be_replayed_after_the_recovery_key_is_revoked_or_at_a_later_head() {
    let pool = pool().await;
    let f = recovery_fixture(&pool).await;
    let req = request(&f);
    let owner = f.owner.id;
    let approvals: Vec<_> = (0..2)
        .map(|i| approval(&f, &req, &f.guardians[i]))
        .collect();
    let first = recovered(&f, &req, approvals);
    apply(&pool, &first, &game()).await.unwrap();
    // The same recovery delivered again from another shard is idempotent.
    apply(&pool, &first, &other()).await.unwrap();
    assert!(active(&pool, &f.owner, req.id).await);

    // The owner later revokes the recovery key (a device grant, then a revocation of the key).
    let device = TestIdentity::new();
    let device_key = Uuid::new_v4();
    let add = grant(
        &f.owner,
        &req.new_key,
        req.id,
        &device,
        device_key,
        Some(&first),
    );
    apply(&pool, &add, &game()).await.unwrap();
    let revocation = revoke(&f.owner, &device, device_key, req.id, Some(&add));
    apply(&pool, &revocation, &game()).await.unwrap();
    assert!(!active(&pool, &f.owner, req.id).await);

    // A thief with the revoked recovery key replays the recovered at the new head, with a fresh
    // event id, and again with a fresh request id but the old approvals.
    let (seq, prev) = next_position(Some(&revocation));
    let mut replay = first.clone();
    replay.id = Uuid::new_v4();
    replay.identity_chain = Some(IdentityChainPosition::current(seq, prev.map(hex::encode)));
    req.new_key.sign_event(&mut replay, req.id);
    assert!(is_refusal(apply(&pool, &replay, &game()).await));
    let other_request = Uuid::new_v4();
    let mut fresh_request = replay.clone();
    fresh_request.id = Uuid::new_v4();
    fresh_request.payload["request_id"] = serde_json::json!(other_request);
    req.new_key.sign_event(&mut fresh_request, other_request);
    assert!(apply(&pool, &fresh_request, &game()).await.is_err());
    assert!(!active(&pool, &f.owner, req.id).await);
    let _ = owner;
}

#[tokio::test]
#[ignore]
async fn a_signature_by_one_identity_cannot_act_on_another_identitys_state() {
    let pool = pool().await;
    let (a, b) = (TestIdentity::new(), TestIdentity::new());
    let key_b = register(&pool, &b).await;
    register(&pool, &a).await;
    let name_before = profile_name(&pool, &a).await;
    let victim = |kind: &str, payload: serde_json::Value, subject: IdentityId| {
        let mut e = event(&b, kind, 1, payload);
        e.subject = GlobalId::new("identity", &subject.to_string(), "self", "x");
        let mut e = positioned(e, 1, None);
        b.sign_event(&mut e, key_b);
        e
    };
    let guild = Uuid::new_v4();
    let forged = [
        victim("profile.updated", serde_json::json!({"bio": "owned"}), a.id),
        victim(
            "identity.passkey_registered",
            serde_json::json!({
                "passkey_id": Uuid::new_v4(), "identity_id": a.id,
                "credential_id": b64(Uuid::new_v4().as_bytes()),
                "passkey_data": {"k": 1}, "label": null,
            }),
            b.id,
        ),
        victim(
            "identity.passkey_revoked",
            serde_json::json!({"passkey_id": Uuid::new_v4(), "identity_id": a.id}),
            b.id,
        ),
        victim(
            "friend.accepted",
            serde_json::json!({"from": a.id, "to": TestIdentity::new().id, "actor": a.id}),
            a.id,
        ),
        victim(
            "friend.requested",
            serde_json::json!({"from": a.id, "to": b.id, "actor": a.id}),
            b.id,
        ),
        victim(
            "friend.removed",
            serde_json::json!({"a": a.id, "b": TestIdentity::new().id, "actor": b.id}),
            a.id,
        ),
        victim(
            "friend.relationship_reversed",
            serde_json::json!({
                "reverses_event_id": Uuid::nil(), "recovery_request_id": Uuid::nil(),
                "identity_id": a.id, "counterparty_id": b.id, "effect": "friendship_removed",
            }),
            b.id,
        ),
    ];
    for e in &forged {
        assert!(
            matches!(apply(&pool, e, &game()).await, Err(IndexError::Rejected(_))),
            "{}",
            e.kind
        );
    }
    // A guild act on another identity is refused unless it is an explicit admin act.
    let mut join = event(
        &b,
        "guild.member_added",
        1,
        serde_json::json!({
            "guild_id": guild, "identity_id": a.id, "role_index": 2, "via": "join", "actor": b.id,
        }),
    );
    join.subject = GlobalId::new("guild", &guild.to_string(), "self", "x");
    let mut join = positioned(join, 1, None);
    b.sign_event(&mut join, key_b);
    assert!(matches!(
        apply(&pool, &join, &game()).await,
        Err(IndexError::Rejected(_))
    ));
    assert_eq!(profile_name(&pool, &a).await, name_before);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM identity_chain_events WHERE identity_id = $1",
            b.id
        )
        .await,
        0,
        "no forged event reaches the signer's chain"
    );
}
