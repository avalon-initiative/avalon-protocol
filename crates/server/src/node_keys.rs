//! First-boot key generation. Any node key not supplied through the environment is loaded from
//! the data directory, or generated there once and reused on every later start.
//!
//! Files live in `<data dir>/keys/` (mode 0700), each mode 0600, holding lowercase hex. A key
//! that is present is never regenerated; an unreadable or malformed one aborts startup.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;
use rand::Rng;

/// Environment variable, key file name, and whether the value is an Ed25519 seed (else a token).
const KEYS: &[(&str, &str, bool)] = &[
    (
        "AVALON_SETTLEMENT_SIGNING_KEY",
        "settlement_signing.key",
        true,
    ),
    (
        "AVALON_SETTLEMENT_SUBMIT_KEY",
        "settlement_submit.key",
        false,
    ),
    ("AVALON_WITNESS_SIGNING_KEY", "witness_signing.key", true),
    ("AVALON_LIBP2P_IDENTITY_KEY", "libp2p_identity.key", true),
];

#[derive(Debug, thiserror::Error)]
pub enum NodeKeyError {
    #[error("node key file {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("node key file {0} does not hold 32 bytes of hex; refusing to replace it")]
    Malformed(PathBuf),
}

/// What first-boot resolution decided; the caller applies it to the process environment.
#[derive(Debug, Default)]
pub struct ResolvedNodeKeys {
    /// Variables to set because the environment left them unset.
    pub env: Vec<(&'static str, String)>,
    /// Variables whose value was newly generated this start.
    pub generated: Vec<&'static str>,
    /// Variables whose value came from an existing key file.
    pub loaded: Vec<&'static str>,
    /// Self-certifying shard id to author when the operator named none and no remote authority
    /// signs for this node.
    pub own_shard_id: Option<String>,
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> NodeKeyError + '_ {
    move |source| NodeKeyError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), NodeKeyError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(io_err(path))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), NodeKeyError> {
    Ok(())
}

fn ensure_keys_dir(data_dir: &Path) -> Result<PathBuf, NodeKeyError> {
    let dir = data_dir.join("keys");
    std::fs::create_dir_all(&dir).map_err(io_err(&dir))?;
    set_mode(data_dir, 0o700)?;
    set_mode(&dir, 0o700)?;
    Ok(dir)
}

fn read_key(path: &Path) -> Result<Option<String>, NodeKeyError> {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let v = s.trim().to_string();
            match hex::decode(&v) {
                Ok(b) if b.len() == 32 => {
                    set_mode(path, 0o600)?;
                    Ok(Some(v))
                }
                _ => Err(NodeKeyError::Malformed(path.to_path_buf())),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(path)(e)),
    }
}

/// Writes through a private temp file and links it into place without overwriting, so a crash
/// never leaves a partial key and a concurrent starter's key is never replaced.
fn write_new(dir: &Path, path: &Path, value: &str) -> Result<bool, NodeKeyError> {
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(io_err(path))?;
    set_mode(tmp.path(), 0o600)?;
    tmp.write_all(value.as_bytes()).map_err(io_err(path))?;
    tmp.as_file().sync_all().map_err(io_err(path))?;
    match tmp.persist_noclobber(path) {
        Ok(_) => Ok(true),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(io_err(path)(e.error)),
    }
}

fn random_hex() -> String {
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    hex::encode(seed)
}

/// Resolves every node key not already present in the environment. `get_env` reads the
/// environment (a closure so tests need not mutate the process environment).
pub fn resolve(
    data_dir: &Path,
    get_env: &dyn Fn(&str) -> Option<String>,
) -> Result<ResolvedNodeKeys, NodeKeyError> {
    let set = |name: &str| get_env(name).is_some_and(|v| !v.trim().is_empty());
    let mut out = ResolvedNodeKeys::default();
    let mut values: HashMap<&'static str, String> = HashMap::new();
    let mut dir: Option<PathBuf> = None;

    for &(var, file, _) in KEYS {
        if set(var) {
            continue;
        }
        let keys_dir = match &dir {
            Some(d) => d.clone(),
            None => {
                let d = ensure_keys_dir(data_dir)?;
                dir = Some(d.clone());
                d
            }
        };
        let path = keys_dir.join(file);
        let value = loop {
            if let Some(v) = read_key(&path)? {
                out.loaded.push(var);
                break v;
            }
            let candidate = random_hex();
            if write_new(&keys_dir, &path, &candidate)? {
                out.generated.push(var);
                break candidate;
            }
        };
        values.insert(var, value.clone());
        out.env.push((var, value));
    }

    let remote_authority =
        set("AVALON_SETTLEMENT_REMOTE_URL") || set("AVALON_SETTLEMENT_REMOTE_URLS");
    if !set("AVALON_OWN_SHARD_ID") && !remote_authority {
        if let Some(seed_hex) = values.get("AVALON_SETTLEMENT_SIGNING_KEY") {
            let seed: [u8; 32] = hex::decode(seed_hex)
                .ok()
                .and_then(|b| b.try_into().ok())
                .ok_or_else(|| NodeKeyError::Malformed(data_dir.join("keys")))?;
            let key = SigningKey::from_bytes(&seed).verifying_key();
            out.own_shard_id = Some(avalon_protocol::shard_identity::derive_self_certifying_id(
                &key,
            ));
        }
    }
    Ok(out)
}

/// Applies [`resolve`] to the process environment. Must run before anything reads those
/// variables and before worker threads are spawned.
pub fn apply_from_env(data_dir: &Path) -> Result<ResolvedNodeKeys, NodeKeyError> {
    let resolved = resolve(data_dir, &|n| std::env::var(n).ok())?;
    for (var, value) in &resolved.env {
        std::env::set_var(var, value);
    }
    if let Some(id) = &resolved.own_shard_id {
        std::env::set_var("AVALON_OWN_SHARD_ID", id);
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn first_boot_generates_all_keys_and_a_self_certifying_shard() {
        let dir = tempfile::tempdir().unwrap();
        let r = resolve(dir.path(), &no_env).unwrap();
        assert_eq!(r.generated.len(), 4);
        assert!(r.loaded.is_empty());
        let id = r.own_shard_id.unwrap();
        assert!(avalon_protocol::shard_identity::is_self_certifying(&id));
    }

    #[test]
    fn second_boot_reuses_keys_and_shard() {
        let dir = tempfile::tempdir().unwrap();
        let a = resolve(dir.path(), &no_env).unwrap();
        let b = resolve(dir.path(), &no_env).unwrap();
        assert!(b.generated.is_empty());
        assert_eq!(b.loaded.len(), 4);
        assert_eq!(a.env, b.env);
        assert_eq!(a.own_shard_id, b.own_shard_id);
    }

    #[test]
    fn environment_keys_win_and_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let env = |n: &str| Some(format!("value-of-{n}"));
        let r = resolve(dir.path(), &env).unwrap();
        assert!(r.env.is_empty() && r.own_shard_id.is_none());
        assert!(!dir.path().join("keys").exists());
    }

    #[test]
    fn env_signing_key_keeps_default_shard_and_generates_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let env = |n: &str| (n == "AVALON_SETTLEMENT_SIGNING_KEY").then(|| "ab".repeat(32));
        let r = resolve(dir.path(), &env).unwrap();
        assert_eq!(r.generated.len(), 3);
        assert!(r.own_shard_id.is_none());
    }

    #[test]
    fn explicit_shard_or_remote_authority_suppresses_shard_default() {
        for var in [
            "AVALON_OWN_SHARD_ID",
            "AVALON_SETTLEMENT_REMOTE_URL",
            "AVALON_SETTLEMENT_REMOTE_URLS",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let env = |n: &str| (n == var).then(|| "x".to_string());
            assert!(resolve(dir.path(), &env).unwrap().own_shard_id.is_none());
        }
    }

    #[cfg(unix)]
    #[test]
    fn permissions_are_owner_only_and_loose_files_are_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        resolve(dir.path(), &no_env).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let keys = dir.path().join("keys");
        assert_eq!(mode(dir.path()), 0o700);
        assert_eq!(mode(&keys), 0o700);
        let f = keys.join("settlement_signing.key");
        assert_eq!(mode(&f), 0o600);
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        resolve(dir.path(), &no_env).unwrap();
        assert_eq!(mode(&f), 0o600);
    }

    #[test]
    fn malformed_key_file_aborts_and_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        resolve(dir.path(), &no_env).unwrap();
        let f = dir.path().join("keys/witness_signing.key");
        std::fs::write(&f, "garbage").unwrap();
        assert!(matches!(
            resolve(dir.path(), &no_env),
            Err(NodeKeyError::Malformed(_))
        ));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "garbage");
    }

    #[test]
    fn unwritable_data_dir_reports_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, "x").unwrap();
        assert!(matches!(
            resolve(&blocker, &no_env),
            Err(NodeKeyError::Io { .. })
        ));
    }
}
