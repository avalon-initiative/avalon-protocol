//! `avalon setup`: the hosting guide as a guided, idempotent first-run flow.
//!
//! The decisions live in `config`, `envfile` and `verify`; `prompt` is the only code that reads a
//! terminal, and this module sequences them.

mod config;
mod envfile;
mod prompt;
mod sys;
mod verify;

use std::io::IsTerminal;
use std::time::Duration;

use config::{
    closing_notes, install_commands, resolve, resolve_data_dir, systemd_unit, Env, Host,
    NodeConfig, ServiceChoice, SetupArgs, Variant, UNIT_NAME,
};
use envfile::{plan, redact, Action, EnvFile};
use prompt::{Auto, Prompter, Stdio};

pub use config::USAGE;

pub fn parse_args(raw: &[String]) -> Result<SetupArgs, String> {
    SetupArgs::parse(raw)
}

pub async fn run(args: SetupArgs) -> Result<(), String> {
    if !args.yes && !std::io::stdin().is_terminal() {
        return Err("stdin is not a terminal, so setup cannot ask questions; rerun with --yes and flags (see `avalon setup --help`) or the AVALON_* environment variables".to_string());
    }
    let mut prompter: Box<dyn Prompter> = if args.yes {
        Box::new(Auto)
    } else {
        Box::new(Stdio)
    };
    let p = prompter.as_mut();
    let interactive = p.interactive();

    let get = |k: &str| std::env::var(k).ok();
    let env = Env { get: &get };
    let host = Host {
        home: std::env::var("HOME").ok(),
        is_root: sys::is_root(),
        cwd: std::env::current_dir().map_err(|e| e.to_string())?,
    };

    if interactive {
        p.say("Avalon node setup. This follows the hosting guide (`avalon guide standalone`) and can be rerun safely; existing keys and settings are kept.");
    }
    let data_dir = resolve_data_dir(&args, &env, &host, p)?;
    config::check_data_dir(&data_dir, host.home.as_deref())?;
    let config_path = data_dir.join(config::CONFIG_FILE);
    let existing = match std::fs::read_to_string(&config_path) {
        Ok(text) => EnvFile::parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => EnvFile::default(),
        Err(e) => return Err(format!("cannot read {}: {e}", config_path.display())),
    };
    let mut cfg = resolve(
        &args,
        &env,
        &host,
        &existing,
        data_dir,
        p,
        &sys::check_database,
    )?;

    if cfg.network_defaulted {
        println!(
            "\nNOTICE: no --network was given, so this node uses {} and is standalone: it is not joined to any network and has no peers. To join one, rerun with `--network <id>` (`avalon guide networks` lists the built-in ones).\n",
            cfg.network_id
        );
    }
    let (merged, changed) = write_config(&cfg, &existing, &args, interactive)?;
    // Later steps act on what the file holds, which can differ from what was requested.
    if let Some(v) = merged.get("AVALON_NETWORK_ID") {
        cfg.network_id = v.to_string();
    }
    if let Some(v) = merged.get("AVALON_SERVER_ADDR") {
        cfg.listen_addr = v.to_string();
    }

    let mut service_running = false;
    let want_service = match args.service {
        ServiceChoice::Yes => true,
        ServiceChoice::No => false,
        ServiceChoice::Ask if interactive && sys::systemd_available() => {
            p.say("Service: a systemd unit keeps the node running across reboots and restarts it on failure. See `avalon guide systemd`.");
            p.confirm("Install a systemd service?", false)?
        }
        ServiceChoice::Ask => false,
    };
    if want_service {
        service_running = install_service(&cfg, &args, p)?;
    }

    let base = cfg.local_base_url();
    let running = service_running || node_answers(&base).await;
    let mut child = None;
    let started = if running {
        if !service_running {
            println!("Node already answers at {base}; leaving it running.");
            if changed {
                println!("The configuration changed; restart the node to apply it.");
            }
        }
        true
    } else if args.no_start
        || (interactive && !p.confirm("Start the node now in the background?", true)?)
    {
        print_foreground_command(&cfg, &args);
        false
    } else {
        child = start_node(&cfg, &args)?;
        child.is_some()
    };

    let mut failed = None;
    if started && !args.no_verify {
        failed = verify_node(&cfg, &base, child).await.err();
    }

    println!("\nNext steps:");
    for note in closing_notes(&cfg) {
        println!("  - {note}");
    }
    println!("  - Logs and problems: `avalon guide troubleshooting`.");
    println!("  - The whole guide is built in: `avalon guide`.");
    failed.map_or(Ok(()), Err)
}

/// Passkey relying-party settings: replacing them invalidates every registered passkey.
const WEBAUTHN_KEYS: [&str; 2] = ["AVALON_WEBAUTHN_RP_ID", "AVALON_WEBAUTHN_ORIGIN"];

fn write_config(
    cfg: &NodeConfig,
    existing: &EnvFile,
    args: &SetupArgs,
    interactive: bool,
) -> Result<(EnvFile, bool), String> {
    sys::ensure_private_dir(&cfg.data_dir)
        .map_err(|e| format!("cannot create {}: {e}", cfg.data_dir.display()))?;
    if args.force_webauthn {
        println!("WARNING: --force-webauthn replaces the passkey relying-party settings; passkeys registered under the old values stop working.");
    }
    let replace_others = args.force || interactive;
    let may_replace = |key: &str| {
        if WEBAUTHN_KEYS.contains(&key) {
            args.force_webauthn
        } else {
            replace_others
        }
    };
    // The pair belongs together: with either one already set, neither is derived.
    let pinned_webauthn =
        !args.force_webauthn && WEBAUTHN_KEYS.iter().any(|k| existing.get(k).is_some());
    let mut desired = cfg.desired_entries();
    if pinned_webauthn {
        desired.retain(|(k, _)| !WEBAUTHN_KEYS.contains(k));
        println!("  kept     existing passkey settings (change them only with --force-webauthn)");
    }
    let (merged, items) = plan(existing, &desired, &may_replace);
    println!("Configuration: {}", cfg.config_path().display());
    for i in &items {
        let show = |v: &Option<String>| {
            v.as_deref()
                .map_or("(unset)".to_string(), |v| redact(i.key, v))
        };
        match i.action {
            Action::Added => println!("  added    {}={}", i.key, show(&i.value)),
            Action::Kept => println!("  kept     {}", i.key),
            Action::Replaced => println!("  changed  {}={}", i.key, show(&i.value)),
            Action::Removed => println!("  removed  {}", i.key),
            Action::KeptDifferent => println!(
                "  kept     {}={} (requested {}; rerun with {} to change it)",
                i.key,
                show(&i.value),
                i.requested
                    .as_deref()
                    .map_or("removal".to_string(), |v| redact(i.key, v)),
                if WEBAUTHN_KEYS.contains(&i.key) {
                    "--force-webauthn"
                } else {
                    "--force"
                },
            ),
        }
    }
    let text = merged.render()?;
    let changed = existing.render()? != text || existing.is_empty();
    if !changed {
        println!("  configuration unchanged");
    } else {
        sys::write_private(&cfg.config_path(), &text)
            .map_err(|e| format!("cannot write {}: {e}", cfg.config_path().display()))?;
        println!("  wrote {} (owner-only)", cfg.config_path().display());
    }
    if cfg.data_dir.join("keys").is_dir() {
        println!(
            "  keys: existing keys in {}/keys are kept",
            cfg.data_dir.display()
        );
    } else {
        println!(
            "  keys: the node generates its keys on first start, under {}/keys",
            cfg.data_dir.display()
        );
    }
    Ok((merged, changed))
}

/// Returns whether the service is now running.
fn install_service(
    cfg: &NodeConfig,
    args: &SetupArgs,
    p: &mut dyn Prompter,
) -> Result<bool, String> {
    if !sys::systemd_available() {
        println!("Service: systemd is not available on this host; skipping (launchd and Windows services are not supported yet).");
        return Ok(false);
    }
    let is_root = sys::is_root();
    let user = args.service_user.clone().unwrap_or_else(|| {
        if is_root {
            "avalon".to_string()
        } else {
            sys::current_user()
        }
    });
    if !config::valid_service_user(&user) {
        return Err(format!(
            "{user:?} is not a valid service user name; pass --service-user with a name matching [a-z_][a-z0-9_-]{{0,31}}"
        ));
    }
    if user == "root" && cfg.variant == Variant::Bundled {
        return Err(
            "the bundled variant cannot run as root; pass --service-user with an ordinary account"
                .to_string(),
        );
    }
    let name = cfg.variant.binary_name();
    let bin = sys::find_binary(name, args.server_bin.as_deref());
    let exec = bin
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("/usr/local/bin").join(name));
    if bin.is_none() {
        println!("Service: {name} was not found next to avalon or on PATH; the unit points at {}. Install it there or pass --server-bin.", exec.display());
    }
    let unit_file = cfg.data_dir.join(UNIT_NAME);
    let unit = systemd_unit(cfg, &exec, &user)?;
    if unit_file.exists() && std::fs::read_to_string(&unit_file).ok().as_deref() == Some(&unit) {
        println!("Service: {} is unchanged", unit_file.display());
    } else {
        sys::write_file(&unit_file, &unit, 0o644)
            .map_err(|e| format!("cannot write {}: {e}", unit_file.display()))?;
        println!("Service: wrote {}", unit_file.display());
    }
    let create_user = is_root && !sys::user_exists(&user);
    let cmds = install_commands(&unit_file, cfg, &user, create_user);
    if !is_root {
        println!("  Installing needs root. Run:");
        for c in &cmds {
            println!("    sudo {}", c.join(" "));
        }
        println!("  The unit runs as {user}; the data directory must be owned by that account.");
        return Ok(false);
    }
    if p.interactive()
        && !p.confirm(
            &format!("Install the unit and start the service as {user}?"),
            true,
        )?
    {
        println!("  Not installed. To install later:");
        for c in &cmds {
            println!("    {}", c.join(" "));
        }
        return Ok(false);
    }
    for c in &cmds {
        sys::run_command(c)?;
    }
    println!("  Service enabled and started: `systemctl status avalon`, `journalctl -u avalon -f`");
    Ok(true)
}

fn print_foreground_command(cfg: &NodeConfig, args: &SetupArgs) {
    let name = cfg.variant.binary_name();
    let bin = sys::find_binary(name, args.server_bin.as_deref())
        .map_or(name.to_string(), |b| b.display().to_string());
    println!(
        "\nTo start the node yourself:\n  set -a; . {}; set +a; exec {bin}",
        cfg.config_path().display()
    );
}

async fn node_answers(base: &str) -> bool {
    verify::wait_for_node(base, Duration::from_secs(1), false, || None)
        .await
        .is_ok()
}

fn start_node(cfg: &NodeConfig, args: &SetupArgs) -> Result<Option<std::process::Child>, String> {
    let name = cfg.variant.binary_name();
    let Some(bin) = sys::find_binary(name, args.server_bin.as_deref()) else {
        println!("\n{name} was not found next to avalon or on PATH, so the node was not started. Install it (see `avalon guide standalone`) or pass --server-bin, then rerun setup.");
        return Ok(None);
    };
    let text = std::fs::read_to_string(cfg.config_path()).map_err(|e| e.to_string())?;
    let env = EnvFile::parse(&text).to_map();
    let remove: &[&str] = if cfg.variant == Variant::Bundled {
        &["DATABASE_URL"]
    } else {
        &[]
    };
    let log = cfg.data_dir.join("avalon.log");
    let pid_file = cfg.data_dir.join("avalon.pid");
    let mut child = sys::spawn_detached(&bin, &env, remove, &log, &pid_file)?;
    let pid = child.id();
    std::thread::sleep(Duration::from_millis(1500));
    if let Ok(Some(status)) = child.try_wait() {
        std::fs::remove_file(&pid_file).ok();
        return Err(format!(
            "{} exited immediately ({status}); see {}",
            bin.display(),
            log.display()
        ));
    }
    println!(
        "\nStarted {} (pid {pid}); log: {}. Stop it with `kill {pid}`.",
        bin.display(),
        log.display()
    );
    Ok(Some(child))
}

async fn verify_node(
    cfg: &NodeConfig,
    base: &str,
    mut child: Option<std::process::Child>,
) -> Result<(), String> {
    println!("\nVerifying the node at {base}");
    let wait = if cfg.variant == Variant::Bundled {
        300
    } else {
        90
    };
    if cfg.variant == Variant::Bundled {
        println!("  the bundled variant downloads and initializes PostgreSQL on its first start; this can take a few minutes");
    }
    let body = verify::wait_for_node(base, Duration::from_secs(wait), true, || {
        let status = child.as_mut()?.try_wait().ok()??;
        Some(format!("the node process exited ({status})"))
    })
    .await
    .map_err(|e| {
        format!(
            "{e}; see {}/avalon.log or `avalon guide troubleshooting`",
            cfg.data_dir.display()
        )
    })?;
    let summary = verify::summarize_discover(&body, &cfg.network_id)?;
    println!("  answers: yes (roles: {})", summary.roles.join(","));
    if !summary.network_matches {
        return Err(format!(
            "the node reports network {} but {} was configured",
            summary.network_id, cfg.network_id
        ));
    }
    println!("  network: {}", summary.network_id);

    let entry = avalon_protocol::network_trust::bundled_trust_anchors()
        .iter()
        .find(|e| e.network_id == cfg.network_id);
    let Some(entry) = entry.filter(|e| !e.seed_nodes.is_empty()) else {
        println!("  standalone: not joined to any network (no seed nodes for {} in the built-in trust list); there are no peers or head to verify", cfg.network_id);
        return Ok(());
    };
    let peers = verify::wait_for_peers(base, &cfg.network_id, Duration::from_secs(45))
        .await
        .map_or(0, |s| s.peers);
    let head = verify::wait_for_head(base, &entry.verify_key, Duration::from_secs(60)).await;
    let summary = verify::judge_join(&cfg.network_id, peers, head.as_ref())?;
    println!("  {summary}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_webauthn_settings_survive_forced_and_interactive_reruns() {
        let dir = std::env::temp_dir().join(format!("avalon-mod-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let raw = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let args = SetupArgs::parse(&raw(&[
            "--yes",
            "--variant",
            "bundled",
            "--role",
            "author",
            "--force",
            "--public-url",
            "https://new.example.org",
            "--network",
            "n",
        ]))
        .unwrap();
        let get = |_: &str| None;
        let env = Env { get: &get };
        let host = Host {
            home: None,
            is_root: false,
            cwd: dir.clone(),
        };
        let existing = EnvFile::parse(
            "AVALON_WEBAUTHN_RP_ID=old.example.org\nAVALON_WEBAUTHN_ORIGIN=https://old.example.org\nAVALON_NODE_URL=https://old.example.org\n",
        );
        let cfg = resolve(
            &args,
            &env,
            &host,
            &existing,
            dir.clone(),
            &mut Auto,
            &|_| Ok(String::new()),
        )
        .unwrap();
        let (merged, _) = write_config(&cfg, &existing, &args, true).unwrap();
        assert_eq!(merged.get("AVALON_WEBAUTHN_RP_ID"), Some("old.example.org"));
        assert_eq!(
            merged.get("AVALON_NODE_URL"),
            Some("https://new.example.org")
        );

        let forced = SetupArgs {
            force_webauthn: true,
            ..args
        };
        let (merged, _) = write_config(&cfg, &existing, &forced, true).unwrap();
        assert_eq!(merged.get("AVALON_WEBAUTHN_RP_ID"), Some("new.example.org"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
