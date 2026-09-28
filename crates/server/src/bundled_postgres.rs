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
    pg_root: std::path::PathBuf,
}

/// Where the managed instance lives under the node's data directory.
pub fn pg_root(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("postgres")
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
        let pg_root = pg_root(data_dir);
        preflight(&pg_root).await?;
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
            .map_err(|e| describe_failure("set up", &e.to_string(), &pg_root))?;
        // The download has happened by now, so the installed binary's own dependencies can be checked.
        preflight(&pg_root).await?;
        instance
            .start()
            .await
            .map_err(|e| describe_failure("start", &e.to_string(), &pg_root))?;

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

        Ok(Some(Self { instance, pg_root }))
    }

    /// Stops the managed instance with a fast-mode shutdown bounded by `bound`; if that fails or
    /// times out, every process of the instance is terminated so none is left behind.
    pub async fn stop(self, bound: std::time::Duration) {
        // The stop runs on its own task because it can block its thread waiting on `pg_ctl`,
        // which a timeout on this task could not interrupt.
        let instance = self.instance;
        let mut stopping = tokio::spawn(async move { instance.stop().await });
        match tokio::time::timeout(bound, &mut stopping).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(e))) => {
                tracing::warn!("bundled-postgres: error stopping embedded Postgres: {e}");
                force_stop_blocking(&self.pg_root).await;
            }
            Ok(Err(e)) => {
                tracing::warn!("bundled-postgres: stop task failed: {e}");
                force_stop_blocking(&self.pg_root).await;
            }
            Err(_) => {
                tracing::warn!("bundled-postgres: stop exceeded {bound:?}; terminating it");
                force_stop_blocking(&self.pg_root).await;
            }
        }
    }
}

static EXIT_ROOT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

extern "C" fn force_stop_at_exit() {
    if let Some(root) = EXIT_ROOT.get() {
        force_stop(root);
    }
}

/// Makes any process exit (including `process::exit` from a startup failure) terminate the
/// instance's processes first, so a node that dies never leaves its database running.
pub fn force_stop_on_exit(pg_root: &Path) {
    if EXIT_ROOT.set(pg_root.to_path_buf()).is_ok() {
        // SAFETY: registers an `extern "C"` function that takes no arguments.
        unsafe { libc::atexit(force_stop_at_exit) };
    }
}

async fn force_stop_blocking(pg_root: &Path) {
    let root = pg_root.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || force_stop(&root)).await;
}

/// Terminates every process that belongs to the managed instance under `pg_root`: the
/// postmaster gets SIGINT (fast shutdown), helpers such as `initdb` and `pg_ctl` SIGTERM, and
/// whatever remains after a grace period is killed. Blocking; bounded at about ten seconds.
pub fn force_stop(pg_root: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    let postmaster = std::fs::read_to_string(pg_root.join("data").join("postmaster.pid"))
        .ok()
        .and_then(|t| t.lines().next().and_then(|l| l.trim().parse::<i32>().ok()));
    let mut signalled = false;
    while std::time::Instant::now() < deadline {
        let pids = processes_under(pg_root);
        if pids.is_empty() {
            return;
        }
        if !signalled {
            for pid in &pids {
                let sig = if Some(*pid) == postmaster {
                    libc::SIGINT
                } else {
                    libc::SIGTERM
                };
                // SAFETY: kill takes plain integers; a stale pid at worst fails with ESRCH.
                unsafe { libc::kill(*pid, sig) };
            }
            signalled = true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    for _ in 0..20 {
        let pids = processes_under(pg_root);
        if pids.is_empty() {
            return;
        }
        for pid in pids {
            // SAFETY: as above.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Pids (excluding this process) whose executable path starts with `pg_root`, which is how the
/// instance's `postgres`, `pg_ctl` and `initdb` are launched.
fn processes_under(pg_root: &Path) -> Vec<i32> {
    let needle = pg_root.to_string_lossy().to_string();
    let me = std::process::id() as i32;
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().to_string_lossy().parse::<i32>().ok())
        .filter(|pid| *pid != me)
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline"))
                .map(|raw| {
                    let argv0 = raw.split(|b| *b == 0).next().unwrap_or_default();
                    String::from_utf8_lossy(argv0).starts_with(&needle)
                })
                .unwrap_or(false)
        })
        .collect()
}

/// Fails fast, naming the missing package, when the host cannot run the embedded Postgres.
async fn preflight(pg_root: &Path) -> Result<(), String> {
    let install_dir = pg_root.join("install");
    let root = is_root();
    let problems =
        tokio::task::spawn_blocking(move || crate::bundled_prereq::check_host(root, &install_dir))
            .await
            .map_err(|e| format!("prerequisite check failed: {e}"))?;
    if problems.iter().any(|p| p.is_blocking()) {
        return Err(format!(
            "{}(set {} to skip these checks)",
            crate::bundled_prereq::format_problems(&problems),
            crate::bundled_prereq::SKIP_ENV
        ));
    }
    if !problems.is_empty() {
        tracing::warn!("{}", crate::bundled_prereq::format_problems(&problems));
    }
    Ok(())
}

/// Builds the startup-failure message: the underlying error, the tail of Postgres's own log,
/// and an install hint when the output matches a known host problem.
fn describe_failure(stage: &str, error: &str, pg_root: &Path) -> String {
    let log = std::fs::read_to_string(pg_root.join("data").join("start.log")).unwrap_or_default();
    let mut msg = format!("failed to {stage} embedded Postgres: {error}");
    let log_tail = crate::bundled_prereq::tail(&log, 15);
    if !log_tail.is_empty() {
        msg.push_str(&format!(
            "\nlast lines of {}:\n{log_tail}",
            pg_root.join("data").join("start.log").display()
        ));
    }
    if let Some(problem) = crate::bundled_prereq::explain_failure(&format!("{error}\n{log}")) {
        msg.push_str(&format!("\nhint: {}", problem.message));
    }
    msg
}

#[cfg(unix)]
fn is_root() -> bool {
    // SAFETY: geteuid takes no arguments and always succeeds.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(not(unix))]
fn is_root() -> bool {
    false
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

    #[cfg(target_os = "linux")]
    #[test]
    fn force_stop_terminates_processes_launched_from_the_instance_directory() {
        let root = std::env::temp_dir().join(format!("avalon-forcestop-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let exe = root.join("install-bin-postgres");
        let mut child = std::process::Command::new("bash")
            .args(["-c", "exec -a \"$0\" sleep 300"])
            .arg(&exe)
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!processes_under(&root).is_empty(), "child must be found");
        let unrelated = std::process::Command::new("sleep")
            .arg("300")
            .spawn()
            .unwrap();
        force_stop(&root);
        let status = child.wait().unwrap();
        assert!(!status.success(), "child must have been signalled");
        let mut unrelated = unrelated;
        assert!(
            unrelated.try_wait().unwrap().is_none(),
            "an unrelated process must be left alone"
        );
        unrelated.kill().unwrap();
        unrelated.wait().unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

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
