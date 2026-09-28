//! `avalon guide` and the non-interactive edges of `avalon setup`, driven through the real binary.

use std::process::{Command, Stdio};

fn avalon() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_avalon"));
    c.stdin(Stdio::null());
    c
}

#[test]
fn guide_prints_a_section_of_the_published_docs() {
    let out = avalon().args(["guide", "systemd"]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("## Running under systemd"));
    assert!(text.contains("ExecStart"));
}

#[test]
fn guide_rejects_an_unknown_topic() {
    let out = avalon().args(["guide", "nonsense"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8(out.stderr).unwrap().contains("topics:"));
}

#[test]
fn setup_without_a_terminal_points_at_yes_instead_of_hanging() {
    let out = avalon().arg("setup").output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8(out.stderr).unwrap().contains("--yes"));
}

#[test]
fn setup_yes_writes_an_owner_only_config_and_is_idempotent() {
    let dir = std::env::temp_dir().join(format!("avalon-setup-test-{}", std::process::id()));
    let run = || {
        avalon()
            .args([
                "setup",
                "--yes",
                "--variant",
                "bundled",
                "--no-start",
                "--data-dir",
            ])
            .arg(&dir)
            .output()
            .unwrap()
    };
    let first = run();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let config = dir.join("avalon.env");
    let written = std::fs::read_to_string(&config).unwrap();
    assert!(written.contains("AVALON_REPLICA_ONLY=true"));
    assert!(!written.contains("DATABASE_URL"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let second = run();
    assert!(second.status.success());
    assert!(String::from_utf8(second.stdout)
        .unwrap()
        .contains("configuration unchanged"));
    assert_eq!(std::fs::read_to_string(&config).unwrap(), written);
    std::fs::remove_dir_all(&dir).ok();
}
