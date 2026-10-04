//! Exercises `PostgresIndexer::apply` against a real, migrated Postgres.
//! Gated `--ignored` since it needs live infra — see `make test-live` /
//! `make migrate`, same convention `crates/server/tests/friends.rs` and
//! friends already use in this repo. `cargo test --workspace` (this
//! sandbox's only reachable check) skips these by default.

use avalon_indexer::postgres::PostgresIndexer;
use avalon_indexer::Indexer;
use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::identity_id::{
    device_grant_approval_signing_bytes_v2, signing_key_revoked_signing_bytes_v2, TestIdentity,
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
        .expect("failed to connect to Postgres — is it reachable?")
}

fn indexer(pool: &PgPool) -> PostgresIndexer {
    PostgresIndexer::new(pool.clone()).with_local_origin("avalon-test-network", "core")
}

fn global(who: &TestIdentity, verb: &str) -> GlobalId {
    GlobalId::new("identity", &who.id.to_string(), "self", verb)
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn key_event(who: &TestIdentity, kind: &str, payload: serde_json::Value) -> ProtocolEvent {
    ProtocolEvent {
        id: Uuid::new_v4(),
        kind: kind.to_string(),
        issuer: global(who, "signing_key_event"),
        subject: global(who, "signing_key_event"),
        payload,
        timestamp: OffsetDateTime::now_utc(),
        version: 2,
        identity_chain: None,
    }
}

fn inception_event(who: &TestIdentity, key_id: Uuid) -> ProtocolEvent {
    key_event(
        who,
        "identity.signing_key_added",
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

/// A device-grant addition of `device`'s key as `key_id`, approved (signed) by `approver`.
fn grant_event(
    who: &TestIdentity,
    approver: &TestIdentity,
    approver_key_id: Uuid,
    device: &TestIdentity,
    key_id: Uuid,
) -> ProtocolEvent {
    use ed25519_dalek::Signer as _;
    let grant_id = Uuid::new_v4();
    let bytes = device_grant_approval_signing_bytes_v2(grant_id, &who.id, &device.public_key());
    key_event(
        who,
        "identity.signing_key_added",
        serde_json::json!({
            "signing_key_id": key_id,
            "public_key": b64(&device.public_key()),
            "device_label": null,
            "approved_by_signing_key_id": approver_key_id,
            "identity_id": who.id,
            "kind": "device_grant",
            "grant_id": grant_id,
            "approval_signature": b64(&approver.signing_key.sign(&bytes).to_bytes()),
        }),
    )
}

/// A revocation of `key_id` signed by `signer`, naming `signer_key_id` as the revoker.
fn revoke_event(
    who: &TestIdentity,
    signer: &TestIdentity,
    signer_key_id: Uuid,
    key_id: Uuid,
) -> ProtocolEvent {
    use ed25519_dalek::Signer as _;
    let bytes = signing_key_revoked_signing_bytes_v2(&who.id, key_id, signer_key_id);
    key_event(
        who,
        "identity.signing_key_revoked",
        serde_json::json!({
            "identity_id": who.id,
            "signing_key_id": key_id,
            "revoked_by_signing_key_id": signer_key_id,
            "signature": b64(&signer.signing_key.sign(&bytes).to_bytes()),
        }),
    )
}

async fn key_active(pool: &PgPool, who: &TestIdentity, key_id: Uuid) -> bool {
    avalon_indexer::projections::identity_signing_keys::find_active_by_id(pool, who.id, key_id)
        .await
        .unwrap()
        .is_some()
}

async fn seed_identity(pool: &PgPool) -> TestIdentity {
    let who = avalon_protocol::identity_id::TestIdentity::new();
    let identity_id = who.id;
    sqlx::query("INSERT INTO identities (id, inception_public_key) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(who.public_key().to_vec())
        .execute(pool)
        .await
        .expect("failed to seed identity");
    who
}

fn identity_created_event(who: &TestIdentity) -> ProtocolEvent {
    let identity_id = who.id;
    ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "identity.created".to_string(),
        issuer: GlobalId::new("identity", &identity_id.to_string(), "self", "created"),
        subject: GlobalId::new("identity", &identity_id.to_string(), "self", "created"),
        payload: serde_json::to_value(who.created_payload(&format!("indexer-test-{identity_id}")))
            .unwrap(),
        timestamp: OffsetDateTime::now_utc(),
        version: 2,
        identity_chain: None,
    }
}

#[tokio::test]
#[ignore]
async fn apply_is_idempotent_per_projection() {
    let pool = test_pool().await;
    let indexer = indexer(&pool);
    let who = seed_identity(&pool).await;
    let identity_id = who.id;
    let event = identity_created_event(&who);

    indexer.apply(&event).await.expect("first apply failed");
    indexer.apply(&event).await.expect("second apply failed");

    let rows = sqlx::query("SELECT display_name FROM profiles WHERE identity_id = $1")
        .bind(identity_id)
        .fetch_all(&pool)
        .await
        .expect("failed to read back profiles row");
    assert_eq!(
        rows.len(),
        1,
        "applying the same event twice must not duplicate the row"
    );
    let display_name: String = rows[0].try_get("display_name").unwrap();
    assert_eq!(display_name, format!("indexer-test-{identity_id}"));
}

#[tokio::test]
#[ignore]
async fn unknown_kind_is_skipped_not_error() {
    let pool = test_pool().await;
    let indexer = indexer(&pool);

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "some.future.kind".to_string(),
        issuer: GlobalId::new(
            "identity",
            &IdentityId::random_for_tests().to_string(),
            "self",
            "x",
        ),
        subject: GlobalId::new(
            "identity",
            &IdentityId::random_for_tests().to_string(),
            "self",
            "x",
        ),
        payload: serde_json::json!({ "anything": "at all" }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
        identity_chain: None,
    };

    indexer
        .apply(&event)
        .await
        .expect("an unrecognized event kind must be skipped, never returned as an error");
}

/// #678: the 14 kinds that were uninvestigated as of #669 and got the same
/// server-owned no-op treatment. Not asserting anything about their
/// payload shape (that's each handler's own concern) — just that
/// `apply_in_tx`'s dispatch treats them as a deliberate no-op rather than
/// erroring. Deliberately does NOT call `rebuild_from_scratch` here: that
/// truncates every projection table in the whole database before
/// replaying whatever event list it's given, so calling it with only
/// these 14 synthetic events (rather than the real, full ledger) would
/// permanently discard every other real row those tables hold — the
/// dedicated `crates/server/tests/rebuild_from_events.rs` live tests are
/// the ones that exercise `rebuild_from_scratch`/`rebuild_index_from_ledger`,
/// and they do it by rebuilding from the *actual* full ledger, never a
/// hand-picked subset.
#[tokio::test]
#[ignore]
async fn server_owned_kinds_from_678_are_a_noop_not_an_error() {
    let pool = test_pool().await;
    let indexer = indexer(&pool);

    let kinds = [
        "identity.recovery_configured",
        "identity.recovery_requested",
        "identity.recovery_approved",
        "identity.recovery_cancelled",
        "identity.recovered",
        "issuer.registered",
        "guild.updated",
        "guild.role_defined",
        "guild.role_deleted",
        "guild.owner_transferred",
        "guild.game_associated",
        "guild.favorite_games_updated",
        "guild.channel_renamed",
        "guild.channel_archived",
    ];

    let events: Vec<ProtocolEvent> = kinds
        .iter()
        .map(|kind| {
            let id = Uuid::new_v4();
            ProtocolEvent {
                id: Uuid::new_v4(),
                kind: kind.to_string(),
                issuer: GlobalId::new("identity", &id.to_string(), "self", "x"),
                subject: GlobalId::new("identity", &id.to_string(), "self", "x"),
                payload: serde_json::json!({ "anything": "at all" }),
                timestamp: OffsetDateTime::now_utc(),
                version: 1,
                identity_chain: None,
            }
        })
        .collect();

    for event in &events {
        indexer
            .apply(event)
            .await
            .unwrap_or_else(|e| panic!("{:?} must be a no-op, not an error: {e}", event.kind));
    }
}

#[tokio::test]
#[ignore]
async fn a_projected_identity_created_with_an_id_lookalike_name_is_refused() {
    let pool = test_pool().await;
    let indexer = indexer(&pool);
    let who = seed_identity(&pool).await;
    let other = TestIdentity::new();
    let mut event = identity_created_event(&who);
    event.payload =
        serde_json::to_value(who.created_payload(&other.id.to_string().to_uppercase())).unwrap();
    let err = indexer
        .apply(&event)
        .await
        .expect_err("lookalike name must not project");
    assert!(
        matches!(err, avalon_indexer::IndexError::DisplayNameNotPermitted),
        "{err:?}"
    );
    assert!(!err.is_transient());
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM profiles WHERE identity_id = $1")
        .bind(who.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
#[ignore]
async fn a_signing_key_row_cannot_change_its_public_key() {
    let pool = test_pool().await;
    let indexer = indexer(&pool);
    let a = seed_identity(&pool).await;
    let inception = Uuid::new_v4();
    indexer
        .apply(&inception_event(&a, inception))
        .await
        .unwrap();
    let (device, other) = (TestIdentity::new(), TestIdentity::new());
    let key_id = Uuid::new_v4();
    indexer
        .apply(&grant_event(&a, &a, inception, &device, key_id))
        .await
        .unwrap();
    // The same registration delivered again is idempotent.
    indexer
        .apply(&grant_event(&a, &a, inception, &device, key_id))
        .await
        .unwrap();
    // A validly approved grant that reuses the key id for another public key is refused.
    let swapped = indexer
        .apply(&grant_event(&a, &a, inception, &other, key_id))
        .await;
    assert!(
        matches!(swapped, Err(avalon_indexer::IndexError::Rejected(_))),
        "{swapped:?}"
    );
    let stored: Vec<u8> = sqlx::query_scalar(
        "SELECT public_key FROM indexer_identity_signing_keys \
         WHERE identity_id = $1 AND signing_key_id = $2",
    )
    .bind(a.id)
    .bind(key_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored, device.public_key().to_vec());
}

#[tokio::test]
#[ignore]
async fn a_passkey_row_cannot_be_repointed_or_revoked_by_another_identity() {
    use base64::Engine as _;
    let pool = test_pool().await;
    let indexer = indexer(&pool);
    let (a, b) = (seed_identity(&pool).await, seed_identity(&pool).await);
    let passkey_id = Uuid::new_v4();
    let credential = base64::engine::general_purpose::STANDARD.encode(Uuid::new_v4().as_bytes());
    let registered = |who: &TestIdentity, credential: &str| ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "identity.passkey_registered".to_string(),
        issuer: GlobalId::new(
            "identity",
            &who.id.to_string(),
            "self",
            "passkey_registered",
        ),
        subject: GlobalId::new(
            "identity",
            &who.id.to_string(),
            "self",
            "passkey_registered",
        ),
        payload: serde_json::json!({
            "passkey_id": passkey_id,
            "identity_id": who.id,
            "credential_id": credential,
            "passkey_data": {"k": 1},
            "label": null,
        }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
        identity_chain: None,
    };
    indexer.apply(&registered(&a, &credential)).await.unwrap();
    // The same registration delivered again (new event id) stays idempotent.
    indexer.apply(&registered(&a, &credential)).await.unwrap();
    let to_other_identity = indexer.apply(&registered(&b, &credential)).await;
    assert!(
        matches!(
            to_other_identity,
            Err(avalon_indexer::IndexError::Rejected(_))
        ),
        "{to_other_identity:?}"
    );
    let other_credential =
        base64::engine::general_purpose::STANDARD.encode(Uuid::new_v4().as_bytes());
    let to_other_credential = indexer.apply(&registered(&a, &other_credential)).await;
    assert!(
        matches!(
            to_other_credential,
            Err(avalon_indexer::IndexError::Rejected(_))
        ),
        "{to_other_credential:?}"
    );

    let revoke = |who: &TestIdentity| ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "identity.passkey_revoked".to_string(),
        issuer: GlobalId::new("identity", &who.id.to_string(), "self", "passkey_revoked"),
        subject: GlobalId::new("identity", &who.id.to_string(), "self", "passkey_revoked"),
        payload: serde_json::json!({ "passkey_id": passkey_id, "identity_id": who.id }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
        identity_chain: None,
    };
    indexer.apply(&revoke(&b)).await.unwrap();
    let revoked: Option<OffsetDateTime> = sqlx::query_scalar(
        "SELECT revoked_at FROM indexer_identity_passkeys WHERE passkey_id = $1",
    )
    .bind(passkey_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        revoked.is_none(),
        "another identity must not revoke the passkey"
    );
    indexer.apply(&revoke(&a)).await.unwrap();
    let revoked: Option<OffsetDateTime> = sqlx::query_scalar(
        "SELECT revoked_at FROM indexer_identity_passkeys WHERE passkey_id = $1",
    )
    .bind(passkey_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(revoked.is_some());
}

#[tokio::test]
#[ignore]
async fn key_ids_are_scoped_to_their_identity_and_early_revocations_stick() {
    let pool = test_pool().await;
    let indexer = indexer(&pool);
    let (a, b) = (seed_identity(&pool).await, seed_identity(&pool).await);
    let key_id = Uuid::new_v4();
    // Both identities use the same key id for their inception key.
    indexer.apply(&inception_event(&b, key_id)).await.unwrap();
    indexer.apply(&inception_event(&a, key_id)).await.unwrap();
    assert!(key_active(&pool, &a, key_id).await);
    // b revoking "its" key id does not touch a's key.
    indexer
        .apply(&revoke_event(&b, &b, key_id, key_id))
        .await
        .unwrap();
    assert!(key_active(&pool, &a, key_id).await);
    assert!(!key_active(&pool, &b, key_id).await);

    // Revoked-before-added: the later addition is born revoked.
    let c = seed_identity(&pool).await;
    let c_inception = Uuid::new_v4();
    indexer
        .apply(&inception_event(&c, c_inception))
        .await
        .unwrap();
    let (device, late_key) = (TestIdentity::new(), Uuid::new_v4());
    indexer
        .apply(&revoke_event(&c, &c, c_inception, late_key))
        .await
        .unwrap();
    indexer
        .apply(&grant_event(&c, &c, c_inception, &device, late_key))
        .await
        .unwrap();
    assert!(!key_active(&pool, &c, late_key).await);
}

#[tokio::test]
#[ignore]
async fn an_early_passkey_revocation_sticks_and_a_repeat_keeps_the_first_time() {
    let pool = test_pool().await;
    let indexer = indexer(&pool);
    let a = seed_identity(&pool).await;
    let passkey_id = Uuid::new_v4();
    let at = |secs: i64| OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(secs);
    let event = |kind: &str, payload: serde_json::Value, ts: OffsetDateTime| ProtocolEvent {
        id: Uuid::new_v4(),
        kind: kind.to_string(),
        issuer: GlobalId::new("identity", &a.id.to_string(), "self", "x"),
        subject: GlobalId::new("identity", &a.id.to_string(), "self", "x"),
        payload,
        timestamp: ts,
        version: 1,
        identity_chain: None,
    };
    let revoke = serde_json::json!({ "passkey_id": passkey_id, "identity_id": a.id });
    indexer
        .apply(&event("identity.passkey_revoked", revoke.clone(), at(100)))
        .await
        .unwrap();
    indexer
        .apply(&event(
            "identity.passkey_registered",
            serde_json::json!({
                "passkey_id": passkey_id, "identity_id": a.id, "credential_id": b64(Uuid::new_v4().as_bytes()),
                "passkey_data": {"k": 1}, "label": null,
            }),
            at(50),
        ))
        .await
        .unwrap();
    indexer
        .apply(&event("identity.passkey_revoked", revoke, at(200)))
        .await
        .unwrap();
    let revoked: Option<OffsetDateTime> = sqlx::query_scalar(
        "SELECT revoked_at FROM indexer_identity_passkeys WHERE passkey_id = $1",
    )
    .bind(passkey_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(revoked, Some(at(100)));
}
