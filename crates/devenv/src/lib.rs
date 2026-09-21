//! Loads the workspace root's `.env` from a fixed, compile-time-known
//! location — see issue #671.
//!
//! `dotenvy::dotenv()` searches upward from the process's *current working
//! directory* for a `.env` file. Run from inside a git worktree checked out
//! under the repo root (with no `.env` of its own), that search walks past
//! the worktree and silently loads the real repo root's `.env` instead —
//! including live-infra-pointing values. `env!("CARGO_MANIFEST_DIR")` is
//! resolved at compile time to this crate's own source location, which is
//! unaffected by the process's working directory or which worktree it runs
//! from, so walking up from it to find the workspace root is deterministic.

use std::path::{Path, PathBuf};

/// Loads `.env` from the workspace root, resolved deterministically from
/// this crate's own compile-time source location rather than the process's
/// runtime working directory. Mirrors `dotenvy::dotenv().ok()`'s posture:
/// never a hard failure if `.env` is missing.
pub fn load() {
    if let Some(root) = workspace_root() {
        dotenvy::from_path(root.join(".env")).ok();
    }
}

/// Walks up from this crate's own `CARGO_MANIFEST_DIR` (fixed at compile
/// time) looking for the workspace root — the first ancestor directory
/// whose `Cargo.toml` contains a `[workspace]` table.
fn workspace_root() -> Option<PathBuf> {
    let mut dir = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    loop {
        let candidate = dir.join("Cargo.toml");
        if candidate.is_file() {
            if let Ok(contents) = std::fs::read_to_string(&candidate) {
                if contents.contains("[workspace]") {
                    return Some(dir);
                }
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}
