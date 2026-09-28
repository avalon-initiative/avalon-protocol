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
use verify::HeadCheck;

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

    let (merged, changed) = write_config(&cfg, &existing, args.force || interactive)?;
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
        start_node(&cfg, &args)?
    };

    let mut failed = None;
    if started && !args.no_verify {
        failed = verify_node(&cfg, &base).await.err();
    }

    println!("\nNext steps:");
    for note in closing_notes(&cfg) {
        println!("  - {note}");
    }
    println!("  - Logs and problems: `avalon guide troubleshooting`.");
    println!("  - The whole guide is built in: `avalon guide`.");
    failed.map_or(Ok(()), Err)
}

fn write_config(
    cfg: &NodeConfig,
    existing: &EnvFile,
    replace: bool,
) -> Result<(EnvFile, bool), String> {
    sys::ensure_private_dir(&cfg.data_dir)
        .map_err(|e| format!("cannot create {}: {e}", cfg.data_dir.display()))?;
    let (merged, items) = plan(existing, &cfg.desired_entries(), replace);
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
                "  kept     {}={} (requested {}; rerun with --force to change it)",
                i.key,
                show(&i.value),
                i.requested
                    .as_deref()
                    .map_or("removal".to_string(), |v| redact(i.key, v)),
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
    let unit = systemd_unit(cfg, &exec, &user);
    if unit_file.exists() && std::fs::read_to_string(&unit_file).ok().as_deref() == Some(&unit) {
        println!("Service: {} is unchanged", unit_file.display());
    } else {
        std::fs::write(&unit_file, &unit)
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
    verify::wait_for_node(base, Duration::from_secs(1))
        .await
        .is_ok()
}

fn start_node(cfg: &NodeConfig, args: &SetupArgs) -> Result<bool, String> {
    let name = cfg.variant.binary_name();
    let Some(bin) = sys::find_binary(name, args.server_bin.as_deref()) else {
        println!("\n{name} was not found next to avalon or on PATH, so the node was not started. Install it (see `avalon guide standalone`) or pass --server-bin, then rerun setup.");
        return Ok(false);
    };
    let text = std::fs::read_to_string(cfg.config_path()).map_err(|e| e.to_string())?;
    let env = EnvFile::parse(&text).to_map();
    let remove: &[&str] = if cfg.variant == Variant::Bundled {
        &["DATABASE_URL"]
    } else {
        &[]
    };
    let log = cfg.data_dir.join("avalon.log");
    let pid = sys::spawn_detached(&bin, &env, remove, &log, &cfg.data_dir.join("avalon.pid"))?;
    println!(
        "\nStarted {} (pid {pid}); log: {}. Stop it with `kill {pid}`.",
        bin.display(),
        log.display()
    );
    Ok(true)
}

async fn verify_node(cfg: &NodeConfig, base: &str) -> Result<(), String> {
    println!("\nVerifying the node at {base}");
    let wait = if cfg.variant == Variant::Bundled {
        600
    } else {
        120
    };
    if cfg.variant == Variant::Bundled {
        println!("  the bundled variant downloads and initializes PostgreSQL on its first start; this can take a few minutes");
    }
    let body = verify::wait_for_node(base, Duration::from_secs(wait))
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
        println!("  peers and head: skipped, the network has no seed nodes in the built-in trust list (a standalone node)");
        return Ok(());
    };
    match verify::wait_for_peers(base, &cfg.network_id, Duration::from_secs(45)).await {
        Some(s) if s.peers > 0 => println!("  discovery: {} peer(s) known", s.peers),
        _ => println!("  discovery: no peers yet; they fill in within an announce interval. If it stays empty see `avalon guide networks`."),
    }
    match verify::wait_for_head(base, &entry.verify_key, Duration::from_secs(60)).await {
        Some(HeadCheck::Verified { tree_size }) => {
            println!("  head: core head (tree size {tree_size}) verifies against the pinned key");
            Ok(())
        }
        Some(HeadCheck::BadSignature) => {
            Err("the node's core head does not verify against the network's pinned key".to_string())
        }
        Some(HeadCheck::Malformed(e)) => {
            Err(format!("the node's core head could not be read: {e}"))
        }
        None => {
            println!("  head: no core head served yet; mirroring can take a while. Check again with `curl {base}/ledger/sth/latest?shard_id=core`.");
            Ok(())
        }
    }
}
