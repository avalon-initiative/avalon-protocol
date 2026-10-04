//! Migration 0084 on a populated database, on an isolated schema.
//! Run with `cargo test -p avalon-server --test projection_proof_migration -- --ignored`
//! against a throwaway database (`DATABASE_URL`).

use avalon_protocol::identity_id::TestIdentity;
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

async fn insert_key(
    pool: &PgPool,
    who: &TestIdentity,
    key: &[u8],
    id: Uuid,
    added_secs: i64,
    revoked: bool,
) {
    sqlx::query(
        "INSERT INTO indexer_identity_signing_keys \
         (signing_key_id, identity_id, public_key, added_at, revoked_at) \
         VALUES ($1, $2, $3, to_timestamp($4), CASE WHEN $5 THEN now() END)",
    )
    .bind(id)
    .bind(who.id)
    .bind(key)
    .bind(added_secs as f64)
    .bind(revoked)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
#[ignore]
async fn the_unique_key_index_migration_collapses_existing_duplicates() {
    let (admin, pool, name) = scratch("dups").await;
    migrate::migrate_down_one(&pool, MigrationSource::Embedded)
        .await
        .unwrap();

    let who = TestIdentity::new();
    sqlx::query("INSERT INTO identities (id, inception_public_key) VALUES ($1, $2)")
        .bind(who.id)
        .bind(who.public_key().to_vec())
        .execute(&pool)
        .await
        .unwrap();
    let (revoked_pair, active_pair, single) = ([1u8; 32], [2u8; 32], [3u8; 32]);
    let ids: Vec<Uuid> = (0..6).map(|_| Uuid::new_v4()).collect();
    // One public key twice active and once revoked: the revoked row is kept.
    insert_key(&pool, &who, &revoked_pair, ids[0], 100, false).await;
    insert_key(&pool, &who, &revoked_pair, ids[1], 200, true).await;
    insert_key(&pool, &who, &revoked_pair, ids[2], 300, false).await;
    // Two active rows: the earliest is kept.
    insert_key(&pool, &who, &active_pair, ids[3], 500, false).await;
    insert_key(&pool, &who, &active_pair, ids[4], 400, false).await;
    insert_key(&pool, &who, &single, ids[5], 600, false).await;

    sqlx::raw_sql(include_str!(
        "../db/migrations/0084_projection_proof_guards/up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();

    let kept: Vec<Uuid> = sqlx::query_scalar(
        "SELECT signing_key_id FROM indexer_identity_signing_keys ORDER BY added_at",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(kept, vec![ids[1], ids[4], ids[5]]);
    // The guard is in force afterwards.
    let again = sqlx::query(
        "INSERT INTO indexer_identity_signing_keys (signing_key_id, identity_id, public_key, added_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(Uuid::new_v4())
    .bind(who.id)
    .bind(single.to_vec())
    .execute(&pool)
    .await;
    assert!(again.is_err());

    pool.close().await;
    sqlx::query(AssertSqlSafe(format!("DROP SCHEMA {name} CASCADE")))
        .execute(&admin)
        .await
        .unwrap();
}
