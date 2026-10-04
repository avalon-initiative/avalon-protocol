//! `rebuild_from_scratch` skips events refused by validity rules instead of aborting. Gated
//! `--ignored`; it truncates every projection table, so run it only against a throwaway database.

use avalon_indexer::postgres::PostgresIndexer;
use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::identity_id::TestIdentity;
use avalon_protocol::ids::GlobalId;
use sqlx::postgres::PgPoolOptions;
use time::OffsetDateTime;
use uuid::Uuid;

fn created(who: &TestIdentity, name: &str) -> ProtocolEvent {
    let gid = GlobalId::new("identity", &who.id.to_string(), "self", "created");
    ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "identity.created".to_string(),
        issuer: gid.clone(),
        subject: gid,
        payload: serde_json::to_value(who.created_payload(name)).unwrap(),
        timestamp: OffsetDateTime::now_utc(),
        version: 2,
        identity_chain: None,
    }
}

#[tokio::test]
#[ignore]
async fn a_taken_or_lookalike_name_is_skipped_and_leaves_no_orphan_identity() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let indexer =
        PostgresIndexer::new(pool.clone()).with_local_origin("avalon-test-network", "core");
    let (first, second, third) = (
        TestIdentity::new(),
        TestIdentity::new(),
        TestIdentity::new(),
    );
    let name = format!("rebuild-{}", Uuid::new_v4().simple());
    let passkey_for_second = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "identity.passkey_registered".to_string(),
        issuer: GlobalId::new(
            "identity",
            &second.id.to_string(),
            "self",
            "passkey_registered",
        ),
        subject: GlobalId::new(
            "identity",
            &second.id.to_string(),
            "self",
            "passkey_registered",
        ),
        payload: serde_json::json!({
            "passkey_id": Uuid::new_v4(),
            "identity_id": second.id,
            "credential_id": "AAAA",
            "passkey_data": {"k": 1},
            "label": null,
        }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
        identity_chain: None,
    };
    let events = vec![
        created(&first, &name),
        // Same name (case-insensitive): refused by the unique index.
        created(&second, &name.to_uppercase()),
        passkey_for_second,
        // Lookalike of the first identity's id.
        created(&third, &first.id.to_string().to_uppercase()),
    ];
    indexer
        .rebuild_from_scratch(&events)
        .await
        .expect("rebuild must not abort");

    let has = |id: String| {
        let pool = pool.clone();
        async move {
            let identities: i64 =
                sqlx::query_scalar("SELECT count(*) FROM identities WHERE id = $1")
                    .bind(&id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            let profiles: i64 =
                sqlx::query_scalar("SELECT count(*) FROM profiles WHERE identity_id = $1")
                    .bind(&id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            (identities, profiles)
        }
    };
    assert_eq!(has(first.id.to_string()).await, (1, 1));
    assert_eq!(
        has(second.id.to_string()).await,
        (0, 0),
        "no orphan identity row"
    );
    assert_eq!(has(third.id.to_string()).await, (0, 0));
}

#[tokio::test]
#[ignore]
async fn children_ahead_of_their_creation_are_applied_once_it_arrives() {
    use base64::Engine as _;
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let indexer =
        PostgresIndexer::new(pool.clone()).with_local_origin("avalon-test-network", "core");
    let who = TestIdentity::new();
    let key_id = Uuid::new_v4();
    let child = |kind: &str, verb: &str, version: u32, payload: serde_json::Value| ProtocolEvent {
        id: Uuid::new_v4(),
        kind: kind.to_string(),
        issuer: GlobalId::new("identity", &who.id.to_string(), "self", verb),
        subject: GlobalId::new("identity", &who.id.to_string(), "self", verb),
        payload,
        timestamp: OffsetDateTime::now_utc(),
        version,
        identity_chain: None,
    };
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    let events = vec![
        child(
            "identity.passkey_registered",
            "passkey_registered",
            1,
            serde_json::json!({
                "passkey_id": Uuid::new_v4(), "identity_id": who.id,
                "credential_id": b64(Uuid::new_v4().as_bytes()),
                "passkey_data": {"k": 1}, "label": null,
            }),
        ),
        child(
            "identity.signing_key_added",
            "signing_key_added",
            2,
            serde_json::json!({
                "signing_key_id": key_id, "public_key": b64(&who.public_key()),
                "device_label": null, "approved_by_signing_key_id": key_id,
                "identity_id": who.id, "kind": "inception",
            }),
        ),
        created(&who, &format!("legacy-{}", Uuid::new_v4().simple())),
    ];
    let outcome = indexer.rebuild_from_scratch(&events).await.unwrap();
    assert_eq!((outcome.applied, outcome.refused), (3, 0));
    let counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM indexer_identity_passkeys WHERE identity_id = $1), \
                (SELECT count(*) FROM indexer_identity_signing_keys WHERE identity_id = $1), \
                (SELECT count(*) FROM profiles WHERE identity_id = $1)",
    )
    .bind(who.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (1, 1, 1));
}
