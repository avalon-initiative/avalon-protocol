//! Runs the reversible SQL migrations under `db/migrations/` against a
//! Postgres pool. `sqlx`'s own migration tracking (`_sqlx_migrations`) is
//! the source of truth for what's applied — this just drives it.
//!
//! Layout is one directory per migration —
//! `db/migrations/<version>_<description>/{up,down}.sql` — rather than
//! sqlx's default flat `<version>_<description>.{up,down}.sql` files, so
//! related migrations are easy to find/browse as a unit (mirrors WorldZero's
//! `crates/common/src/migrate.rs`). sqlx's `Migrator` doesn't support that
//! layout natively, so this builds the `Migration` list by hand and hands it
//! to `Migrator::with_migrations`.

use std::path::Path;

use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::{AssertSqlSafe, PgPool, SqlSafeStr};

fn load_migrations(migrations_dir: &Path) -> anyhow::Result<Vec<Migration>> {
    let mut migrations = Vec::new();

    let mut entries: Vec<_> = std::fs::read_dir(migrations_dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.path());

    for entry in entries {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let dir_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 migration directory name: {path:?}"))?;

        let (version_str, description) = dir_name.split_once('_').ok_or_else(|| {
            anyhow::anyhow!("migration directory must be <version>_<description>: {dir_name}")
        })?;
        let version: i64 = version_str
            .parse()
            .map_err(|_| anyhow::anyhow!("migration version is not a number: {dir_name}"))?;

        let up_sql = std::fs::read_to_string(path.join("up.sql"))?;
        let down_sql = std::fs::read_to_string(path.join("down.sql"))?;

        migrations.push(Migration::new(
            version,
            description.to_string().into(),
            MigrationType::ReversibleUp,
            AssertSqlSafe(up_sql).into_sql_str(),
            false,
        ));
        migrations.push(Migration::new(
            version,
            description.to_string().into(),
            MigrationType::ReversibleDown,
            AssertSqlSafe(down_sql).into_sql_str(),
            false,
        ));
    }

    migrations.sort();
    Ok(migrations)
}

fn migrator(migrations_dir: &Path) -> anyhow::Result<Migrator> {
    Ok(Migrator::with_migrations(load_migrations(migrations_dir)?))
}

/// Applies every pending migration, in order.
pub async fn migrate_up(pool: &PgPool, migrations_dir: &Path) -> anyhow::Result<()> {
    migrator(migrations_dir)?.run(pool).await?;
    Ok(())
}

/// Reverts exactly the most recently applied migration (its `down.sql`).
/// A no-op if nothing has been applied yet.
pub async fn migrate_down_one(pool: &PgPool, migrations_dir: &Path) -> anyhow::Result<()> {
    let migrator = migrator(migrations_dir)?;

    let mut applied: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success ORDER BY version")
            .fetch_all(pool)
            .await?;

    if applied.pop().is_none() {
        return Ok(());
    }
    let target = applied.pop().unwrap_or(0);

    migrator.undo(pool, target).await?;
    Ok(())
}

/// Drops and recreates the `public` schema, then reapplies every migration
/// from scratch. For local/dev resets only — `make db-reset` is the intended
/// entry point. Does not require CREATEDB privilege (unlike dropping/
/// recreating the database itself), only ownership of the schema, which the
/// application's own role already has.
pub async fn reset(pool: &PgPool, migrations_dir: &Path) -> anyhow::Result<()> {
    sqlx::query("DROP SCHEMA public CASCADE")
        .execute(pool)
        .await?;
    sqlx::query("CREATE SCHEMA public").execute(pool).await?;
    migrate_up(pool, migrations_dir).await
}
