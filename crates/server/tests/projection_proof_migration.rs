//! Migration 0084 on a populated database, on an isolated schema.
//! Run with `cargo test -p avalon-server --test projection_proof_migration -- --ignored`
//! against a throwaway database (`DATABASE_URL`).

use avalon_server::migrate::{self, MigrationSource};
use sqlx::postgres::PgPoolOptions;
use sqlx::{AssertSqlSafe, PgPool};
use uuid::Uuid;

async fn scratch(tag: &str) -> (PgPool, PgPool, String) {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let name = format!("avalon_m0084_{tag}_{}", std::process::id());
    for sql in [
        format!("DROP SCHEMA IF EXISTS {name} CASCADE"),
        format!("CREATE SCHEMA {name}"),
    ] {
        sqlx::query(AssertSqlSafe(sql))
            .execute(&admin)
            .await
            .unwrap();
    }
    let sep = if url.contains('?') { "&" } else { "?" };
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&format!("{url}{sep}options=-c%20search_path%3D{name}"))
        .await
        .unwrap();
    migrate::migrate_up(&pool, MigrationSource::Embedded)
        .await
        .unwrap();
    (admin, pool, name)
}

#[tokio::test]
#[ignore]
async fn the_migration_refuses_a_database_with_projected_events() {
    let (admin, pool, name) = scratch("populated").await;
    migrate::migrate_down_one(&pool, MigrationSource::Embedded)
        .await
        .unwrap();

    // An empty projection migrates cleanly (and back).
    let up = include_str!("../db/migrations/0084_projection_proof_guards/up.sql");
    sqlx::raw_sql(up).execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../db/migrations/0084_projection_proof_guards/down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();

    // Projected history makes it refuse, with the reset instructions.
    sqlx::query("INSERT INTO indexer_applied_events (event_id) VALUES ($1)")
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();
    let refused = sqlx::raw_sql(up)
        .execute(&pool)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("indexer_applied_events") && refused.contains("make db-reset"),
        "{refused}"
    );
    let untouched: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns WHERE table_schema = current_schema() \
         AND table_name = 'mirrored_entries' AND column_name = 'projection_rejection'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(untouched, 0, "a refused migration changes nothing");

    // Mirrored entries alone trigger it too, and the operator override lets it through.
    sqlx::query("DELETE FROM indexer_applied_events")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO mirrored_entries (source_url, network_id, shard_id, seq, event_id, kind, \
         issuer, subject, payload, event_timestamp, version, prev_hash, entry_hash, batch_id, \
         verified_tree_size) \
         VALUES ('http://peer.invalid', 'net', 'core', 1, $1, 'x.y', 'a', 'b', '{}', now(), 1, \
                 'p', 'h', $2, 1)",
    )
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .unwrap();
    let refused = sqlx::raw_sql(up)
        .execute(&pool)
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains("mirrored_entries"), "{refused}");
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("SET avalon.allow_projection_reset = 'on'")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::raw_sql(up).execute(&mut *conn).await.unwrap();
    drop(conn);

    pool.close().await;
    sqlx::query(AssertSqlSafe(format!("DROP SCHEMA {name} CASCADE")))
        .execute(&admin)
        .await
        .unwrap();
}
