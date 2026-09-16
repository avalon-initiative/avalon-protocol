//! Exercises `PostgresIndexer::apply` against a real, migrated Postgres.
//! Gated `--ignored` since it needs live infra — see `make test-live` /
//! `make migrate`, same convention `crates/server/tests/friends.rs` and
//! friends already use in this repo. `cargo test --workspace` (this
//! sandbox's only reachable check) skips these by default.

use avalon_indexer::postgres::PostgresIndexer;
use avalon_indexer::Indexer;
use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::ids::GlobalId;
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

async fn seed_identity(pool: &PgPool) -> Uuid {
    let identity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(identity_id)
        .execute(pool)
        .await
        .expect("failed to seed identity");
    identity_id
}

fn identity_created_event(identity_id: Uuid) -> ProtocolEvent {
    ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "identity.created".to_string(),
        issuer: GlobalId::new("identity", &identity_id.to_string(), "self", "created"),
        subject: GlobalId::new("identity", &identity_id.to_string(), "self", "created"),
        payload: serde_json::json!({
            "identity_id": identity_id,
            "display_name": format!("indexer-test-{identity_id}"),
        }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    }
}

#[tokio::test]
#[ignore]
async fn apply_is_idempotent_per_projection() {
    let pool = test_pool().await;
    let indexer = PostgresIndexer::new(pool.clone());
    let identity_id = seed_identity(&pool).await;
    let event = identity_created_event(identity_id);

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
    let indexer = PostgresIndexer::new(pool.clone());

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "some.future.kind".to_string(),
        issuer: GlobalId::new("identity", &Uuid::new_v4().to_string(), "self", "x"),
        subject: GlobalId::new("identity", &Uuid::new_v4().to_string(), "self", "x"),
        payload: serde_json::json!({ "anything": "at all" }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    };

    indexer
        .apply(&event)
        .await
        .expect("an unrecognized event kind must be skipped, never returned as an error");
}
