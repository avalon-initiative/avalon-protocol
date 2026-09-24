//! Creates or drops isolated Postgres schemas for the live-test harness
//! (`scripts/live-tests.sh`): `live_schema create|drop <schema>...`.

use sqlx::postgres::PgPoolOptions;
use sqlx::{AssertSqlSafe, SqlSafeStr};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let action = args.next().unwrap_or_default();
    let schemas: Vec<String> = args.collect();
    if !matches!(action.as_str(), "create" | "drop") || schemas.is_empty() {
        anyhow::bail!("usage: live_schema create|drop <schema>...");
    }
    for s in &schemas {
        if !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            anyhow::bail!("invalid schema name: {s}");
        }
    }

    avalon_devenv::load();
    let pool = PgPoolOptions::new()
        .connect(&std::env::var("DATABASE_URL")?)
        .await?;
    for s in schemas {
        let sql = match action.as_str() {
            "create" => format!("CREATE SCHEMA IF NOT EXISTS {s}"),
            _ => format!("DROP SCHEMA IF EXISTS {s} CASCADE"),
        };
        sqlx::query(AssertSqlSafe(sql).into_sql_str())
            .execute(&pool)
            .await?;
    }
    Ok(())
}
