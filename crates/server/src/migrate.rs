//! Runs the reversible SQL migrations (embedded from `db/migrations/`, or read
//! from a directory override) against a
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

use std::path::{Path, PathBuf};

use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::{AssertSqlSafe, PgPool, SqlSafeStr};

include!(concat!(env!("OUT_DIR"), "/embedded_migrations.rs"));

/// Where the migration set comes from: the copy compiled into the binary,
/// or a directory on disk (development override).
#[derive(Clone, Copy, Debug)]
pub enum MigrationSource<'a> {
    Embedded,
    Dir(&'a Path),
}

impl<'a> From<&'a Path> for MigrationSource<'a> {
    fn from(p: &'a Path) -> Self {
        Self::Dir(p)
    }
}

impl<'a> From<&'a PathBuf> for MigrationSource<'a> {
    fn from(p: &'a PathBuf) -> Self {
        Self::Dir(p.as_path())
    }
}

/// Reads `AVALON_MIGRATIONS_DIR`; unset or empty selects the embedded set.
pub fn dir_override_from_env() -> Option<PathBuf> {
    std::env::var("AVALON_MIGRATIONS_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn build(
    name: &str,
    up_sql: String,
    down_sql: String,
    out: &mut Vec<Migration>,
) -> anyhow::Result<()> {
    let (version_str, description) = name.split_once('_').ok_or_else(|| {
        anyhow::anyhow!("migration directory must be <version>_<description>: {name}")
    })?;
    let version: i64 = version_str
        .parse()
        .map_err(|_| anyhow::anyhow!("migration version is not a number: {name}"))?;
    for (ty, sql) in [
        (MigrationType::ReversibleUp, up_sql),
        (MigrationType::ReversibleDown, down_sql),
    ] {
        out.push(Migration::new(
            version,
            description.to_string().into(),
            ty,
            AssertSqlSafe(sql).into_sql_str(),
            false,
        ));
    }
    Ok(())
}

fn load_migrations(source: MigrationSource<'_>) -> anyhow::Result<Vec<Migration>> {
    let mut migrations = Vec::new();

    match source {
        MigrationSource::Embedded => {
            for (name, up, down) in EMBEDDED {
                build(name, up.to_string(), down.to_string(), &mut migrations)?;
            }
        }
        MigrationSource::Dir(migrations_dir) => {
            let mut entries: Vec<_> =
                std::fs::read_dir(migrations_dir)?.collect::<Result<_, _>>()?;
            entries.sort_by_key(|e| e.path());
            for entry in entries {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let dir_name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
                    anyhow::anyhow!("non-UTF-8 migration directory name: {path:?}")
                })?;
                build(
                    dir_name,
                    std::fs::read_to_string(path.join("up.sql"))?,
                    std::fs::read_to_string(path.join("down.sql"))?,
                    &mut migrations,
                )?;
            }
        }
    }

    migrations.sort();
    Ok(migrations)
}

fn migrator(source: MigrationSource<'_>) -> anyhow::Result<Migrator> {
    Ok(Migrator::with_migrations(load_migrations(source)?))
}

/// Applies every pending migration, in order.
pub async fn migrate_up(
    pool: &PgPool,
    source: impl Into<MigrationSource<'_>>,
) -> anyhow::Result<()> {
    migrator(source.into())?.run(pool).await?;
    Ok(())
}

/// Reverts exactly the most recently applied migration (its `down.sql`).
/// A no-op if nothing has been applied yet.
pub async fn migrate_down_one(
    pool: &PgPool,
    source: impl Into<MigrationSource<'_>>,
) -> anyhow::Result<()> {
    let migrator = migrator(source.into())?;

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

/// Drops and recreates the connection's current schema (`public` by default), then reapplies every migration
/// from scratch. For local/dev resets only — `make db-reset` is the intended
/// entry point. Does not require CREATEDB privilege (unlike dropping/
/// recreating the database itself), only ownership of the schema, which the
/// application's own role already has.
pub async fn reset(pool: &PgPool, source: impl Into<MigrationSource<'_>>) -> anyhow::Result<()> {
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(pool)
        .await?;
    sqlx::query(AssertSqlSafe(format!("DROP SCHEMA \"{schema}\" CASCADE")))
        .execute(pool)
        .await?;
    sqlx::query(AssertSqlSafe(format!("CREATE SCHEMA \"{schema}\"")))
        .execute(pool)
        .await?;
    migrate_up(pool, source).await
}
