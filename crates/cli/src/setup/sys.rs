//! The operating-system touch points of setup: privileges, binaries, processes, private files.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
}

pub fn current_user() -> String {
    std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .or_else(|| {
            Command::new("id")
                .arg("-un")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .unwrap_or_else(|| "avalon".to_string())
}

pub fn user_exists(user: &str) -> bool {
    Command::new("id")
        .arg("-u")
        .arg(user)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

pub fn systemd_available() -> bool {
    cfg!(target_os = "linux") && Path::new("/run/systemd/system").exists()
}

/// An absolute path to the binary: an explicit path, else a sibling of this executable, else a `PATH` entry.
pub fn find_binary(name: &str, explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return std::fs::canonicalize(p).ok();
    }
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join(name)));
    if let Some(s) = sibling.filter(|s| s.is_file()) {
        return Some(s);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|c| c.is_file())
    })
}

/// Creates `path` (and parents) as owner-only when it does not exist yet.
pub fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Writes `contents` to `path` through a private temporary file and a rename, mode 0600.
pub fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(contents.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// Connects to the database and reports its version, within a short timeout.
pub fn check_database(url: &str) -> Result<String, String> {
    let url = url.to_string();
    let work = async move {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(10))
            .connect(&url)
            .await
            .map_err(|e| e.to_string())?;
        let version: String = sqlx::query_scalar("SHOW server_version")
            .fetch_one(&pool)
            .await
            .map_err(|e| e.to_string())?;
        pool.close().await;
        Ok::<String, String>(format!("PostgreSQL {version}"))
    };
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(work))
}

/// Starts `bin` detached from this process with `env` on top of the current environment.
pub fn spawn_detached(
    bin: &Path,
    env: &std::collections::BTreeMap<String, String>,
    remove: &[&str],
    log: &Path,
    pid_file: &Path,
) -> Result<u32, String> {
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|e| format!("cannot open {}: {e}", log.display()))?;
    let mut cmd = Command::new(bin);
    for k in remove {
        cmd.env_remove(k);
    }
    cmd.envs(env)
        .stdin(Stdio::null())
        .stdout(log_file.try_clone().map_err(|e| e.to_string())?)
        .stderr(log_file);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", bin.display()))?;
    let pid = child.id();
    write_private(pid_file, &format!("{pid}\n")).map_err(|e| e.to_string())?;
    Ok(pid)
}

pub fn run_command(argv: &[String]) -> Result<(), String> {
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .map_err(|e| format!("{}: {e}", argv[0]))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{}` failed with {status}", argv.join(" ")))
    }
}
