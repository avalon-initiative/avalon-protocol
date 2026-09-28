//! The operating-system touch points of setup: privileges, binaries, processes, private files.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Fixed directories system tools are looked up in, never the caller's `PATH`.
const TRUSTED_DIRS: &[&str] = &["/usr/sbin", "/usr/bin", "/sbin", "/bin"];

pub fn is_root() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    false
}

/// An absolute path to a system tool, from the fixed trusted directories.
pub fn trusted_bin(name: &str) -> Option<PathBuf> {
    TRUSTED_DIRS
        .iter()
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
}

pub fn current_user() -> String {
    std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .or_else(|| {
            let id = trusted_bin("id")?;
            let out = Command::new(id).arg("-un").output().ok()?;
            Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
        })
        .unwrap_or_else(|| "avalon".to_string())
}

pub fn user_exists(user: &str) -> bool {
    trusted_bin("id").is_some_and(|id| {
        Command::new(id)
            .args(["-u", "--", user])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

pub fn systemd_available() -> bool {
    cfg!(target_os = "linux") && Path::new("/run/systemd/system").exists()
}

/// An absolute path to the binary: an explicit path, else a sibling of this executable, else a
/// `PATH` entry.
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
            .and_then(|c| std::fs::canonicalize(c).ok())
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

fn open_new(path: &Path, mode: u32) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(mode).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(not(unix))]
    let _ = mode;
    opts.open(path)
}

/// Writes `contents` to `path` with `mode`: a new, randomly named sibling file that is never
/// opened through a symlink, then an atomic rename over the destination.
pub fn write_file(path: &Path, contents: &str, mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let name = path
        .file_name()
        .map_or("file".into(), |n| n.to_string_lossy());
    let tmp = path.with_file_name(format!(".{name}.{}.{nanos:x}.tmp", std::process::id()));
    let result = (|| {
        let mut f = open_new(&tmp, mode)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    result
}

/// Owner-only (0600) variant of [`write_file`].
pub fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    write_file(path, contents, 0o600)
}

/// Opens a log file for appending without following a symlink.
fn open_log(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    opts.open(path)
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

/// Starts `bin` detached from this process with `env` on top of the current environment. If the
/// pid file cannot be written the child is killed rather than left running unrecorded.
pub fn spawn_detached(
    bin: &Path,
    env: &std::collections::BTreeMap<String, String>,
    remove: &[&str],
    log: &Path,
    pid_file: &Path,
) -> Result<Child, String> {
    let log_file = open_log(log).map_err(|e| format!("cannot open {}: {e}", log.display()))?;
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
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", bin.display()))?;
    if let Err(e) = write_private(pid_file, &format!("{}\n", child.id())) {
        child.kill().ok();
        child.wait().ok();
        return Err(format!("cannot write {}: {e}", pid_file.display()));
    }
    Ok(child)
}

/// Runs a system command, resolving its program from the fixed trusted directories.
pub fn run_command(argv: &[String]) -> Result<(), String> {
    let program = trusted_bin(&argv[0]).ok_or_else(|| format!("{} not found", argv[0]))?;
    let status = Command::new(program)
        .args(&argv[1..])
        .status()
        .map_err(|e| format!("{}: {e}", argv[0]))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{}` failed with {status}", argv.join(" ")))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("avalon-sys-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn writes_replace_a_symlink_instead_of_writing_through_it() {
        let d = scratch("write");
        let victim = d.join("victim");
        std::fs::write(&victim, "original").unwrap();
        let dest = d.join("avalon.env");
        std::os::unix::fs::symlink(&victim, &dest).unwrap();
        write_private(&dest, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "original");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "new");
        assert!(!std::fs::symlink_metadata(&dest)
            .unwrap()
            .file_type()
            .is_symlink());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn log_files_are_not_opened_through_a_symlink() {
        let d = scratch("log");
        let victim = d.join("victim");
        std::fs::write(&victim, "original").unwrap();
        let log = d.join("avalon.log");
        std::os::unix::fs::symlink(&victim, &log).unwrap();
        assert!(open_log(&log).is_err());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_failed_write_leaves_no_temporary_file() {
        let d = scratch("tmp");
        assert!(write_file(&d.join("missing-dir").join("f"), "x", 0o600).is_err());
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
        std::fs::remove_dir_all(&d).ok();
    }
}
