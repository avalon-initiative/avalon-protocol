//! Embedded migration set versus the source tree, on isolated schemas.
//! Run with `cargo test -p avalon-server --test embedded_migrations -- --ignored`.

use std::path::{Path, PathBuf};

use avalon_server::migrate::{self, MigrationSource};
use sqlx::postgres::PgPoolOptions;
use sqlx::{AssertSqlSafe, PgPool};

fn source_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("db/migrations")
}

struct Scratch {
    admin: PgPool,
    name: String,
    pool: PgPool,
}

impl Scratch {
    async fn new(tag: &str) -> Self {
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        let name = format!("avalon_embed_{tag}_{}", std::process::id());
        sqlx::query(AssertSqlSafe(format!(
            "DROP SCHEMA IF EXISTS {name} CASCADE"
        )))
        .execute(&admin)
        .await
        .unwrap();
        sqlx::query(AssertSqlSafe(format!("CREATE SCHEMA {name}")))
            .execute(&admin)
            .await
            .unwrap();
        let sep = if url.contains('?') { "&" } else { "?" };
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&format!("{url}{sep}options=-c%20search_path%3D{name}"))
            .await
            .unwrap();
        Self { admin, name, pool }
    }

    async fn drop(self) {
        self.pool.close().await;
        sqlx::query(AssertSqlSafe(format!(
            "DROP SCHEMA IF EXISTS {} CASCADE",
            self.name
        )))
        .execute(&self.admin)
        .await
        .unwrap();
    }
}

async fn schema_dump(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT table_name || '.' || column_name || ':' || data_type || ':' || is_nullable \
         FROM information_schema.columns WHERE table_schema = current_schema() \
         AND table_name <> '_sqlx_migrations' ORDER BY 1",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn applied(pool: &PgPool) -> Vec<(i64, Vec<u8>)> {
    sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore]
async fn embedded_set_matches_source_tree() {
    let a = Scratch::new("emb").await;
    let b = Scratch::new("dir").await;
    migrate::migrate_up(&a.pool, MigrationSource::Embedded)
        .await
        .unwrap();
    migrate::migrate_up(&b.pool, &source_dir()).await.unwrap();

    let dump = schema_dump(&a.pool).await;
    assert!(!dump.is_empty());
    assert_eq!(dump, schema_dump(&b.pool).await);
    assert_eq!(applied(&a.pool).await, applied(&b.pool).await);

    // A database migrated from the source tree still migrates from the embedded set.
    migrate::migrate_up(&b.pool, MigrationSource::Embedded)
        .await
        .unwrap();
    a.drop().await;
    b.drop().await;
}

#[tokio::test]
#[ignore]
async fn checksum_mismatch_is_detected() {
    let s = Scratch::new("sum").await;
    migrate::migrate_up(&s.pool, MigrationSource::Embedded)
        .await
        .unwrap();
    sqlx::query("UPDATE _sqlx_migrations SET checksum = '\\x00' WHERE version = 1")
        .execute(&s.pool)
        .await
        .unwrap();
    let err = migrate::migrate_up(&s.pool, MigrationSource::Embedded)
        .await
        .unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("checksum") || err.to_string().contains("modified"),
        "{err}"
    );
    s.drop().await;
}

#[tokio::test]
#[ignore]
async fn down_up_and_reset_work_from_embedded_set() {
    let s = Scratch::new("rst").await;
    migrate::migrate_up(&s.pool, MigrationSource::Embedded)
        .await
        .unwrap();
    let full = applied(&s.pool).await;

    migrate::migrate_down_one(&s.pool, MigrationSource::Embedded)
        .await
        .unwrap();
    assert_eq!(applied(&s.pool).await.len(), full.len() - 1);
    migrate::migrate_up(&s.pool, MigrationSource::Embedded)
        .await
        .unwrap();
    assert_eq!(applied(&s.pool).await, full);

    migrate::reset(&s.pool, MigrationSource::Embedded)
        .await
        .unwrap();
    assert_eq!(applied(&s.pool).await, full);
    s.drop().await;
}
