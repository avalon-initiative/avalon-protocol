//! Managed, embedded Postgres for `avalon-server-bundled`.
//!
//! Only compiled with the `bundled-postgres` feature (see
//! `src/bin/bundled_main.rs`). Starts a private, loopback-only Postgres
//! instance under the node's own data directory when no `DATABASE_URL` is
//! already set in the environment, exporting the resulting connection
//! string before the rest of startup reads it. An operator-supplied
//! `DATABASE_URL` is always used as-is; no managed instance starts.

use std::path::Path;

use postgresql_embedded::{PostgreSQL, Settings};

const BUNDLED_DATABASE_NAME: &str = "avalon";

/// A running managed Postgres instance. Dropping this without calling
/// [`stop`](Self::stop) leaves the process running; callers should call
/// `stop` on shutdown.
pub struct BundledPostgres {
    instance: PostgreSQL,
}

impl BundledPostgres {
    /// Starts a managed Postgres under `data_dir/postgres` unless
    /// `DATABASE_URL` is already set, in which case this does nothing and
    /// returns `Ok(None)`. Sets `DATABASE_URL` in this process's own
    /// environment on success.
    pub async fn start_if_needed(data_dir: &Path) -> Result<Option<Self>, String> {
        if std::env::var("DATABASE_URL").is_ok() {
            return Ok(None);
        }
        refuse_if_root()?;

        let pg_root = data_dir.join("postgres");
        std::fs::create_dir_all(&pg_root)
            .map_err(|e| format!("failed to create {}: {e}", pg_root.display()))?;

        let password = load_or_generate_password(&pg_root)?;

        let settings = Settings {
            installation_dir: pg_root.join("install"),
            data_dir: pg_root.join("data"),
            // Loopback only — this instance is private to this process,
            // never meant to be reachable externally.
            host: "127.0.0.1".to_string(),
            // 0 asks the embedded-postgres crate to pick a free local port
            // itself, so this never collides with an operator's own
            // Postgres on the default 5432.
            port: 0,
            // `initdb` always creates the bootstrap superuser under
            // `postgresql_embedded::BOOTSTRAP_SUPERUSER` ("postgres"),
            // ignoring `Settings::username` entirely — left at its default
            // rather than set to something that would silently go unused.
            password,
            temporary: false,
            version: postgresql_embedded::V17.clone(),
            ..Settings::default()
        };

        let mut instance = PostgreSQL::new(settings);
        instance
            .setup()
            .await
            .map_err(|e| format!("failed to set up embedded Postgres: {e}"))?;
        instance
            .start()
            .await
            .map_err(|e| format!("failed to start embedded Postgres: {e}"))?;

        let exists = instance
            .database_exists(BUNDLED_DATABASE_NAME)
            .await
            .map_err(|e| format!("failed to check for the bundled database: {e}"))?;
        if !exists {
            instance
                .create_database(BUNDLED_DATABASE_NAME)
                .await
                .map_err(|e| format!("failed to create the bundled database: {e}"))?;
        }

        let url = instance.settings().url(BUNDLED_DATABASE_NAME);
        std::env::set_var("DATABASE_URL", &url);
        tracing::info!(
            port = instance.settings().port,
            data_dir = %pg_root.join("data").display(),
            "bundled-postgres: embedded Postgres ready"
        );

        Ok(Some(Self { instance }))
    }

    /// Stops the managed instance cleanly. Idempotent-in-intent: errors are
    /// logged, not propagated — a stop failure during shutdown shouldn't
    /// block the rest of the process from exiting.
    pub async fn stop(self) {
        if let Err(e) = self.instance.stop().await {
            tracing::warn!("bundled-postgres: error stopping embedded Postgres: {e}");
        }
    }
}

/// Postgres refuses to run its own server process as root; failing here
/// gives a clear error instead of a confusing one from deep inside `initdb`
/// or `postgres` itself.
#[cfg(unix)]
fn refuse_if_root() -> Result<(), String> {
    // SAFETY: geteuid takes no arguments and always succeeds.
    if unsafe { libc::geteuid() } == 0 {
        return Err(
            "bundled-postgres cannot run as root — PostgreSQL refuses to start as root; run \
             avalon-server-bundled as a non-root user"
                .to_string(),
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn refuse_if_root() -> Result<(), String> {
    Ok(())
}

/// The bootstrap superuser's password must be the same across restarts —
/// `initdb` bakes it into the data directory once, so a freshly-generated
/// password on a later boot would just be a real password mismatch.
/// Generated once and persisted next to the data directory it belongs to.
fn load_or_generate_password(pg_root: &Path) -> Result<String, String> {
    let path = pg_root.join("superuser_password");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    let password = {
        use rand::Rng;
        let mut bytes = [0u8; 16];
        rand::rng().fill_bytes(&mut bytes);
        hex::encode(bytes)
    };
    std::fs::write(&path, &password)
        .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("failed to set permissions on {}: {e}", path.display()))?;
    }
    Ok(password)
}

#[cfg(test)]
mod tests {
    //! No live Postgres needed — this only proves the early-return path
    //! that skips starting a managed instance entirely; the real
    //! setup/start/stop path is proven live, see
    //! `crates/server/tests/bundled_postgres.rs`.
    use super::*;

    #[tokio::test]
    async fn does_nothing_when_database_url_is_already_set() {
        // The guard itself must not be held across the `.await` below
        // (clippy's `await_holding_lock`) — scoped to just the env
        // mutation; no other test in this binary touches `DATABASE_URL`.
        {
            let _guard = crate::test_env::guard();
            std::env::set_var("DATABASE_URL", "postgres://example-operator-supplied/db");
        }
        let result = BundledPostgres::start_if_needed(&std::env::temp_dir()).await;
        std::env::remove_var("DATABASE_URL");
        assert!(
            matches!(result, Ok(None)),
            "an already-set DATABASE_URL must short-circuit before touching Postgres at all"
        );
    }
}
