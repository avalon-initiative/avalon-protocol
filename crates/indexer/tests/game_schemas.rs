//! Exercises the Game Registry schema-discovery projection (issue #255)
//! against a real, migrated Postgres. Gated `--ignored` since it needs live
//! infra — see `make test-live` / `make migrate`, same convention
//! `postgres_indexer.rs` already uses. `cargo test --workspace` (this
//! sandbox's only reachable check) skips these by default.

use avalon_indexer::postgres::PostgresIndexer;
use avalon_indexer::projections::game_schemas::list_for_game;
use avalon_indexer::Indexer;
use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::ids::GlobalId;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

async fn test_pool() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

async fn seed_game(pool: &PgPool) -> Uuid {
    let game_id = Uuid::new_v4();
    let slug = format!("indexer-schema-test-{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO games (id, slug, name, developer, registered_at, status) \
         VALUES ($1, $2, $3, $4, $5, 'active')",
    )
    .bind(game_id)
    .bind(&slug)
    .bind("Indexer Schema Test Game")
    .bind("Test Studio")
    .bind(OffsetDateTime::now_utc())
    .execute(pool)
    .await
    .expect("failed to seed game");
    game_id
}

fn published_event(
    game_id: Uuid,
    version: u32,
    proto_source: &str,
    supersedes: Option<&str>,
) -> ProtocolEvent {
    let id = format!("game:test:schema:{version}");
    ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "game_schema.published".to_string(),
        issuer: GlobalId::new("game", "test", "self", "schema_published"),
        subject: GlobalId::new("game", "test", "schema", &version.to_string()),
        payload: serde_json::json!({
            "id": id,
            "game_id": game_id,
            "version": version,
            "proto_source": proto_source,
            "supersedes": supersedes,
        }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    }
}

#[tokio::test]
#[ignore]
async fn a_game_with_no_publications_surfaces_an_empty_list() {
    let pool = test_pool().await;
    let game_id = seed_game(&pool).await;

    let versions = list_for_game(&pool, game_id)
        .await
        .expect("list_for_game failed");
    assert!(versions.is_empty());
}

#[tokio::test]
#[ignore]
async fn a_game_with_publications_surfaces_them_oldest_first_with_lineage() {
    let pool = test_pool().await;
    let indexer = PostgresIndexer::new(pool.clone());
    let game_id = seed_game(&pool).await;

    let v1_id = "game:test:schema:1".to_string();
    let v1 = published_event(game_id, 1, "message Character { uint32 level = 1; }", None);
    indexer.apply(&v1).await.expect("apply v1 failed");

    let v2 = published_event(
        game_id,
        2,
        "message Character { uint32 level = 1; uint64 xp = 2; }",
        Some(&v1_id),
    );
    indexer.apply(&v2).await.expect("apply v2 failed");

    let versions = list_for_game(&pool, game_id)
        .await
        .expect("list_for_game failed");
    assert_eq!(versions.len(), 2);

    assert_eq!(versions[0].version, 1);
    assert_eq!(
        versions[0].proto_source,
        "message Character { uint32 level = 1; }"
    );
    assert_eq!(
        versions[0].superseded_by.as_deref(),
        Some("game:test:schema:2")
    );

    assert_eq!(versions[1].version, 2);
    assert_eq!(
        versions[1].proto_source,
        "message Character { uint32 level = 1; uint64 xp = 2; }"
    );
    assert!(versions[1].superseded_by.is_none());
}

#[tokio::test]
#[ignore]
async fn applying_a_publication_twice_does_not_duplicate_the_row() {
    let pool = test_pool().await;
    let indexer = PostgresIndexer::new(pool.clone());
    let game_id = seed_game(&pool).await;

    let event = published_event(game_id, 1, "message Character { uint32 level = 1; }", None);
    indexer.apply(&event).await.expect("first apply failed");
    indexer.apply(&event).await.expect("second apply failed");

    let versions = list_for_game(&pool, game_id)
        .await
        .expect("list_for_game failed");
    assert_eq!(versions.len(), 1);
}
