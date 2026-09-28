//! Setup arguments, the node configuration they resolve to, and the files derived from it.
//! Nothing here touches the terminal, the network or the filesystem.

use std::path::{Path, PathBuf};

use super::envfile::EnvFile;
use super::prompt::Prompter;

pub const USAGE: &str =
    "usage: avalon setup [--yes] [--variant byo|bundled] [--database-url <url>] [--data-dir <path>]
                    [--role replica|author] [--listen-addr <host:port>] [--public-url <url>]
                    [--libp2p-addr <multiaddr>] [--network <network_id>] [--mirror-peers <urls>]
                    [--witness on|off] [--allow-private-peers] [--service | --no-service]
                    [--service-user <user>] [--server-bin <path>] [--no-start] [--no-verify]
                    [--skip-db-check] [--force]

Without --yes, setup asks each question. With --yes every answer comes from a flag, the matching
AVALON_* / DATABASE_URL environment variable, an existing config in the data directory, or the
default. `avalon guide` prints the hosting guide.";

pub const CONFIG_FILE: &str = "avalon.env";
pub const DEFAULT_NETWORK: &str = "avalon-dev-local";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// `avalon-server` against an operator-provided Postgres.
    Byo,
    /// `avalon-server-bundled`, which manages its own Postgres.
    Bundled,
}

impl Variant {
    pub fn binary_name(self) -> &'static str {
        match self {
            Variant::Byo => "avalon-server",
            Variant::Bundled => "avalon-server-bundled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Mirrors and serves the network, authors nothing.
    Replica,
    /// Also serves logins and authors its own shard.
    Author,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceChoice {
    Ask,
    Yes,
    No,
}

/// Everything given on the command line; unset means "ask, or fall back".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupArgs {
    pub yes: bool,
    pub variant: Option<Variant>,
    pub database_url: Option<String>,
    pub data_dir: Option<PathBuf>,
    pub role: Option<Role>,
    pub listen_addr: Option<String>,
    pub public_url: Option<String>,
    pub libp2p_addr: Option<String>,
    pub network: Option<String>,
    pub mirror_peers: Option<String>,
    pub witness: Option<bool>,
    pub allow_private_peers: bool,
    pub service: ServiceChoice,
    pub service_user: Option<String>,
    pub server_bin: Option<PathBuf>,
    pub no_start: bool,
    pub no_verify: bool,
    pub skip_db_check: bool,
    pub force: bool,
}

impl SetupArgs {
    pub fn parse(raw: &[String]) -> Result<Self, String> {
        let mut a = SetupArgs {
            yes: false,
            variant: None,
            database_url: None,
            data_dir: None,
            role: None,
            listen_addr: None,
            public_url: None,
            libp2p_addr: None,
            network: None,
            mirror_peers: None,
            witness: None,
            allow_private_peers: false,
            service: ServiceChoice::Ask,
            service_user: None,
            server_bin: None,
            no_start: false,
            no_verify: false,
            skip_db_check: false,
            force: false,
        };
        let mut it = raw.iter();
        while let Some(flag) = it.next() {
            let mut value = |name: &str| {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{name} needs a value"))
            };
            match flag.as_str() {
                "--yes" | "-y" => a.yes = true,
                "--variant" => {
                    a.variant = Some(match value("--variant")?.as_str() {
                        "byo" | "plain" => Variant::Byo,
                        "bundled" => Variant::Bundled,
                        other => {
                            return Err(format!("--variant must be byo or bundled, got {other}"))
                        }
                    })
                }
                "--database-url" => a.database_url = Some(value("--database-url")?),
                "--data-dir" => a.data_dir = Some(PathBuf::from(value("--data-dir")?)),
                "--role" => {
                    a.role = Some(parse_role(&value("--role")?)?);
                }
                "--listen-addr" => a.listen_addr = Some(value("--listen-addr")?),
                "--public-url" => a.public_url = Some(value("--public-url")?),
                "--libp2p-addr" => a.libp2p_addr = Some(value("--libp2p-addr")?),
                "--network" => a.network = Some(value("--network")?),
                "--mirror-peers" => a.mirror_peers = Some(value("--mirror-peers")?),
                "--witness" => {
                    a.witness = Some(match value("--witness")?.as_str() {
                        "on" | "true" => true,
                        "off" | "false" => false,
                        other => return Err(format!("--witness must be on or off, got {other}")),
                    })
                }
                "--allow-private-peers" => a.allow_private_peers = true,
                "--service" => a.service = ServiceChoice::Yes,
                "--no-service" => a.service = ServiceChoice::No,
                "--service-user" => a.service_user = Some(value("--service-user")?),
                "--server-bin" => a.server_bin = Some(PathBuf::from(value("--server-bin")?)),
                "--no-start" => a.no_start = true,
                "--no-verify" => a.no_verify = true,
                "--skip-db-check" => a.skip_db_check = true,
                "--force" => a.force = true,
                other => return Err(format!("unknown option {other}")),
            }
        }
        Ok(a)
    }
}

fn parse_role(s: &str) -> Result<Role, String> {
    match s {
        "replica" => Ok(Role::Replica),
        "author" => Ok(Role::Author),
        other => Err(format!("--role must be replica or author, got {other}")),
    }
}

/// The resolved node configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeConfig {
    pub variant: Variant,
    pub role: Role,
    /// Only for `Variant::Byo`.
    pub database_url: Option<String>,
    pub data_dir: PathBuf,
    pub listen_addr: String,
    pub public_url: Option<String>,
    pub libp2p_addr: Option<String>,
    pub network_id: String,
    pub mirror_peers: Option<String>,
    /// `Some(false)` disables cosigning; `None` leaves the server default (on).
    pub witness: Option<bool>,
    pub allow_private_peers: bool,
}

impl NodeConfig {
    pub fn config_path(&self) -> PathBuf {
        self.data_dir.join(CONFIG_FILE)
    }

    /// The managed keys and their desired values, in file order. `None` means absent.
    pub fn desired_entries(&self) -> Vec<(&'static str, Option<String>)> {
        let author = self.role == Role::Author;
        let (rp_id, origin) = if author {
            webauthn_settings(self.public_url.as_deref(), &self.listen_addr)
        } else {
            (None, None)
        };
        vec![
            ("AVALON_NETWORK_ID", Some(self.network_id.clone())),
            (
                "DATABASE_URL",
                match self.variant {
                    Variant::Byo => self.database_url.clone(),
                    Variant::Bundled => None,
                },
            ),
            ("AVALON_DATA_DIR", Some(self.data_dir.display().to_string())),
            ("AVALON_SERVER_ADDR", Some(self.listen_addr.clone())),
            ("AVALON_LIBP2P_LISTEN_ADDR", self.libp2p_addr.clone()),
            ("AVALON_NODE_URL", self.public_url.clone()),
            (
                "AVALON_REPLICA_ONLY",
                (self.role == Role::Replica).then(|| "true".to_string()),
            ),
            ("AVALON_WEBAUTHN_RP_ID", rp_id),
            ("AVALON_WEBAUTHN_ORIGIN", origin),
            ("AVALON_MIRROR_PEERS", self.mirror_peers.clone()),
            (
                "AVALON_WITNESS_COSIGNING_ENABLED",
                (self.witness == Some(false)).then(|| "false".to_string()),
            ),
            (
                "AVALON_ALLOW_PRIVATE_PEERS",
                self.allow_private_peers.then(|| "true".to_string()),
            ),
        ]
    }

    /// The URL setup uses to talk to the node it configured.
    pub fn local_base_url(&self) -> String {
        let addr = self.listen_addr.as_str();
        let (host, port) = addr.rsplit_once(':').unwrap_or((addr, "8080"));
        let host = match host {
            "0.0.0.0" | "" => "127.0.0.1",
            "[::]" | "::" => "[::1]",
            h => h,
        };
        format!("http://{host}:{port}")
    }
}

/// The passkey relying-party id and origin: the public URL when there is one, else localhost.
pub fn webauthn_settings(
    public_url: Option<&str>,
    listen_addr: &str,
) -> (Option<String>, Option<String>) {
    match public_url {
        Some(url) => (host_of(url), Some(url.trim_end_matches('/').to_string())),
        None => {
            let port = listen_addr
                .rsplit_once(':')
                .map(|(_, p)| p)
                .unwrap_or("8080");
            (
                Some("localhost".to_string()),
                Some(format!("http://localhost:{port}")),
            )
        }
    }
}

/// The host part of an `http(s)://host[:port][/path]` URL.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if authority.starts_with('[') {
        authority.split_once(']').map(|(h, _)| format!("{h}]"))?
    } else {
        authority.split(':').next()?.to_string()
    };
    (!host.is_empty()).then_some(host)
}

pub fn validate_public_url(url: &str) -> Result<(), String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) || host_of(url).is_none() {
        return Err(format!(
            "{url:?} is not a full http(s) URL such as https://node.example.org"
        ));
    }
    Ok(())
}

pub fn validate_listen_addr(addr: &str) -> Result<(), String> {
    addr.parse::<std::net::SocketAddr>()
        .map(|_| ())
        .map_err(|_| format!("{addr:?} is not a host:port socket address such as 127.0.0.1:8080"))
}

/// Whether a URL's host is a loopback or private-range address.
pub fn is_private_url(url: &str) -> bool {
    let Some(host) = host_of(url) else {
        return false;
    };
    match host.parse::<std::net::Ipv4Addr>() {
        Ok(ip) => ip.is_private() || ip.is_loopback(),
        Err(_) => host == "localhost",
    }
}

/// A trusted-network choice the wizard can offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkChoice {
    pub network_id: String,
    pub label: String,
    pub seed_count: usize,
    pub private_seeds: bool,
}

pub fn network_choices() -> Vec<NetworkChoice> {
    avalon_protocol::network_trust::bundled_trust_anchors()
        .iter()
        .map(|e| NetworkChoice {
            network_id: e.network_id.clone(),
            label: e.label.clone(),
            seed_count: e.seed_nodes.len(),
            private_seeds: e.seed_nodes.iter().any(|s| is_private_url(s)),
        })
        .collect()
}

/// Where each answer comes from: the process environment, using the server's own variable names.
pub struct Env<'a> {
    pub get: &'a dyn Fn(&str) -> Option<String>,
}

impl Env<'_> {
    fn var(&self, name: &str) -> Option<String> {
        (self.get)(name).filter(|v| !v.trim().is_empty())
    }
}

pub fn default_data_dir(home: Option<&str>, is_root: bool) -> PathBuf {
    if is_root {
        PathBuf::from("/var/lib/avalon")
    } else {
        Path::new(home.unwrap_or(".")).join(".avalon")
    }
}

/// Facts about the machine the resolver needs; injectable so tests need no real system.
pub struct Host {
    pub home: Option<String>,
    pub is_root: bool,
    pub cwd: PathBuf,
}

/// Checks a `DATABASE_URL` by connecting; returns a human-readable description on success.
pub type DbCheck<'a> = &'a dyn Fn(&str) -> Result<String, String>;

fn truthy(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn absolute(p: PathBuf, cwd: &Path) -> PathBuf {
    if p.is_absolute() {
        p
    } else {
        cwd.join(p)
    }
}

/// Resolves the data directory alone; needed first because the existing config lives in it.
pub fn resolve_data_dir(
    args: &SetupArgs,
    env: &Env,
    host: &Host,
    p: &mut dyn Prompter,
) -> Result<PathBuf, String> {
    let default = env
        .var("AVALON_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_data_dir(host.home.as_deref(), host.is_root));
    let chosen = match &args.data_dir {
        Some(d) => d.clone(),
        None if p.interactive() => {
            p.say("Data directory: keys, the node's config and, for the bundled variant, its database live here. See `avalon guide backups`.");
            PathBuf::from(p.ask("Data directory", Some(&default.display().to_string()))?)
        }
        None => default,
    };
    Ok(absolute(chosen, &host.cwd))
}

/// Turns arguments, environment, any existing config and (when interactive) answers into a
/// [`NodeConfig`]. Precedence: flag, environment, existing config, default.
pub fn resolve(
    args: &SetupArgs,
    env: &Env,
    host: &Host,
    existing: &EnvFile,
    data_dir: PathBuf,
    p: &mut dyn Prompter,
    db_check: DbCheck,
) -> Result<NodeConfig, String> {
    let pick = |flag: Option<String>, var: &str| -> Option<String> {
        flag.or_else(|| env.var(var))
            .or_else(|| existing.get(var).map(str::to_string))
    };

    // Database path.
    let env_db = pick(args.database_url.clone(), "DATABASE_URL");
    let variant_default = match (args.variant, &env_db, existing.is_empty()) {
        (Some(v), _, _) => v,
        (None, Some(_), _) => Variant::Byo,
        (None, None, false) => Variant::Bundled,
        (None, None, true) => Variant::Byo,
    };
    let variant = match args.variant {
        Some(v) => v,
        None if p.interactive() => {
            p.say("Database: bring your own PostgreSQL 16 (recommended), or use the bundled variant, which starts and supervises its own private Postgres inside the data directory. See `avalon guide bundled`.");
            let idx = p.choose(
                "Which database path?",
                &[
                    "Bring your own PostgreSQL (avalon-server)",
                    "Bundled, managed PostgreSQL (avalon-server-bundled)",
                ],
                usize::from(variant_default == Variant::Bundled),
            )?;
            [Variant::Byo, Variant::Bundled][idx]
        }
        None => variant_default,
    };
    let database_url = match variant {
        Variant::Bundled => {
            if host.is_root {
                return Err("the bundled variant refuses to run as root (PostgreSQL will not); run setup as an ordinary user".to_string());
            }
            None
        }
        Variant::Byo => Some(resolve_database_url(args, env_db, p, db_check)?),
    };

    // Role.
    let role_default = match (
        args.role,
        env.var("AVALON_REPLICA_ONLY"),
        existing.get("AVALON_REPLICA_ONLY"),
        existing.is_empty(),
    ) {
        (Some(r), ..) => r,
        (None, Some(v), ..) => {
            if truthy(&v) {
                Role::Replica
            } else {
                Role::Author
            }
        }
        (None, None, Some(v), _) => {
            if truthy(v) {
                Role::Replica
            } else {
                Role::Author
            }
        }
        (None, None, None, false) => Role::Author,
        (None, None, None, true) => Role::Replica,
    };
    let role = match args.role {
        Some(r) => r,
        None if p.interactive() => {
            p.say("Role: a replica mirrors and serves a network's history and needs no login settings. An authoring node also serves passkey logins and writes its own shard. See `avalon guide configuration`.");
            let idx = p.choose(
                "Node role?",
                &["Replica (recommended)", "Authoring node"],
                usize::from(role_default == Role::Author),
            )?;
            [Role::Replica, Role::Author][idx]
        }
        None => role_default,
    };

    // Addresses.
    let listen_default = pick(args.listen_addr.clone(), "AVALON_SERVER_ADDR")
        .unwrap_or_else(|| "127.0.0.1:8080".to_string());
    let public_default = pick(args.public_url.clone(), "AVALON_NODE_URL");
    let (listen_addr, public_url) = if args.listen_addr.is_none()
        && args.public_url.is_none()
        && p.interactive()
    {
        p.say("Listen address: the HTTP bind. Keep 127.0.0.1 when a TLS proxy runs on this host; use 0.0.0.0 only if this port should be reachable directly. See `avalon guide ports` and `avalon guide tls`.");
        let listen = p.ask_validated(
            "HTTP listen address",
            Some(&listen_default),
            &validate_listen_addr,
        )?;
        p.say("Public URL: the base URL other nodes use to reach this node. Leave empty to not announce it.");
        let public = p.ask_validated(
            "Public URL (empty for none)",
            public_default.as_deref().or(Some("")),
            &|v| {
                if v.is_empty() {
                    Ok(())
                } else {
                    validate_public_url(v)
                }
            },
        )?;
        (listen, (!public.is_empty()).then_some(public))
    } else {
        (listen_default, public_default)
    };
    validate_listen_addr(&listen_addr)?;
    if let Some(u) = &public_url {
        validate_public_url(u)?;
    }
    if role == Role::Author && public_url.is_none() {
        p.say("Authoring node without a public URL: passkey login is configured for http://localhost only.");
    }
    let libp2p_default =
        pick(args.libp2p_addr.clone(), "AVALON_LIBP2P_LISTEN_ADDR").or_else(|| {
            public_url
                .as_ref()
                .map(|_| "/ip4/0.0.0.0/tcp/4001".to_string())
        });
    let libp2p_addr = if args.libp2p_addr.is_none() && p.interactive() && public_url.is_some() {
        p.say("Peer-discovery listener: other nodes dial this port; open it in your firewall. See `avalon guide ports`.");
        let v = p.ask(
            "Peer-discovery listen multiaddr (empty for an ephemeral loopback-unreachable port)",
            libp2p_default.as_deref(),
        )?;
        (!v.is_empty()).then_some(v)
    } else {
        libp2p_default
    };

    // Network.
    let choices = network_choices();
    let network_default = pick(args.network.clone(), "AVALON_NETWORK_ID")
        .unwrap_or_else(|| DEFAULT_NETWORK.to_string());
    let network_id = if args.network.is_none() && p.interactive() {
        p.say("Network: the built-in trust list carries each network's pinned key and seed nodes, which a node uses to announce itself and mirror core. Pick one, or enter a network id of your own. See `avalon guide networks`.");
        let mut labels: Vec<String> = choices
            .iter()
            .map(|c| {
                format!(
                    "{} ({} seed node(s){})",
                    c.network_id,
                    c.seed_count,
                    if c.private_seeds {
                        ", private addresses"
                    } else {
                        ""
                    }
                )
            })
            .collect();
        labels.push("Another network id".to_string());
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let def = choices
            .iter()
            .position(|c| c.network_id == network_default)
            .unwrap_or(choices.len());
        let idx = p.choose("Which network?", &refs, def)?;
        if idx < choices.len() {
            choices[idx].network_id.clone()
        } else {
            p.ask(
                "Network id",
                (def == choices.len()).then_some(network_default.as_str()),
            )?
        }
    } else {
        network_default
    };
    if network_id.trim().is_empty() {
        return Err("a network id is required".to_string());
    }
    let chosen = choices.iter().find(|c| c.network_id == network_id);

    // Witness / mirror / private peers.
    let mirror_default = pick(args.mirror_peers.clone(), "AVALON_MIRROR_PEERS");
    let witness_default = args.witness.or_else(|| {
        env.var("AVALON_WITNESS_COSIGNING_ENABLED")
            .or_else(|| {
                existing
                    .get("AVALON_WITNESS_COSIGNING_ENABLED")
                    .map(str::to_string)
            })
            .map(|v| truthy(&v))
    });
    let (mirror_peers, witness) = if args.mirror_peers.is_none()
        && args.witness.is_none()
        && p.interactive()
        && p.confirm("Change the witness or mirror settings? (defaults: cosign heads, mirror core from the network's seed nodes)", false)?
    {
        p.say("Witness cosigning: the node countersigns network heads it verified. Mirror peers: where it copies core history from. See `avalon guide configuration`.");
        let w = p.confirm("Cosign heads as a witness?", witness_default.unwrap_or(true))?;
        let m = p.ask("Mirror peers, comma-separated URLs (empty for the seed nodes)", mirror_default.as_deref().or(Some("")))?;
        ((!m.is_empty()).then_some(m), Some(w))
    } else {
        (mirror_default, witness_default)
    };
    let private_default = args.allow_private_peers
        || env
            .var("AVALON_ALLOW_PRIVATE_PEERS")
            .is_some_and(|v| truthy(&v))
        || existing
            .get("AVALON_ALLOW_PRIVATE_PEERS")
            .is_some_and(truthy);
    let allow_private_peers = if !private_default
        && chosen.is_some_and(|c| c.private_seeds)
        && p.interactive()
    {
        p.confirm("This network's seed nodes are on private addresses; accept private peers (AVALON_ALLOW_PRIVATE_PEERS)?", true)?
    } else {
        private_default
    };

    Ok(NodeConfig {
        variant,
        role,
        database_url,
        data_dir,
        listen_addr,
        public_url,
        libp2p_addr,
        network_id,
        mirror_peers,
        witness,
        allow_private_peers,
    })
}

fn resolve_database_url(
    args: &SetupArgs,
    current: Option<String>,
    p: &mut dyn Prompter,
    db_check: DbCheck,
) -> Result<String, String> {
    let validate = |url: &str| -> Result<(), String> {
        if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
            return Err("a postgres:// or postgresql:// connection string is required".to_string());
        }
        if args.skip_db_check {
            return Ok(());
        }
        db_check(url).map(|_| ())
    };
    if !p.interactive() {
        let url = current.ok_or_else(|| {
            "no database configured: pass --database-url (or set DATABASE_URL), or --variant bundled to use the managed database".to_string()
        })?;
        validate(&url).map_err(|e| format!("cannot use the database: {e}"))?;
        return Ok(url);
    }
    p.say("Database URL, e.g. postgres://avalon:password@db-host:5432/avalon (percent-encode special characters in the password). Setup connects to check it. See `avalon guide standalone`.");
    let mut default = current;
    loop {
        let url = p.ask("DATABASE_URL", default.as_deref())?;
        match validate(&url) {
            Ok(()) => return Ok(url),
            Err(e) => {
                p.say(&format!("Could not use that database: {e}"));
                if p.confirm("Use it anyway?", false)? {
                    return Ok(url);
                }
                default = Some(url);
            }
        }
    }
}

/// The systemd unit for the chosen variant and configuration.
pub fn systemd_unit(cfg: &NodeConfig, exec: &Path, user: &str) -> String {
    let data = cfg.data_dir.display();
    let mut unit = format!(
        "[Unit]\nDescription=Avalon node ({network})\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nUser={user}\nGroup={user}\nEnvironmentFile={env}\nExecStart={exec}\nRestart=on-failure\nRestartSec=5\nLimitNOFILE=65536\nNoNewPrivileges=true\nProtectSystem=strict\nReadWritePaths={data}\nPrivateTmp=true\n",
        network = cfg.network_id,
        env = cfg.config_path().display(),
        exec = exec.display(),
    );
    if cfg.variant == Variant::Bundled {
        unit.push_str(&format!("Environment=HOME={data}\n"));
    }
    unit.push_str("\n[Install]\nWantedBy=multi-user.target\n");
    unit
}

pub const UNIT_NAME: &str = "avalon.service";
pub const SYSTEM_UNIT_PATH: &str = "/etc/systemd/system/avalon.service";

/// The commands that install `unit_file` and start the service.
pub fn install_commands(
    unit_file: &Path,
    cfg: &NodeConfig,
    user: &str,
    create_user: bool,
) -> Vec<Vec<String>> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let mut cmds = Vec::new();
    if create_user {
        cmds.push(s(&[
            "useradd",
            "--system",
            "--home-dir",
            &cfg.data_dir.display().to_string(),
            "--shell",
            "/usr/sbin/nologin",
            user,
        ]));
    }
    cmds.push(s(&[
        "chown",
        "-R",
        &format!("{user}:{user}"),
        &cfg.data_dir.display().to_string(),
    ]));
    cmds.push(s(&[
        "install",
        "-m",
        "0644",
        &unit_file.display().to_string(),
        SYSTEM_UNIT_PATH,
    ]));
    cmds.push(s(&["systemctl", "daemon-reload"]));
    cmds.push(s(&["systemctl", "enable", "--now", "avalon"]));
    cmds
}

/// Backup and upgrade notes for the chosen variant.
pub fn closing_notes(cfg: &NodeConfig) -> Vec<String> {
    let data = cfg.data_dir.display();
    match cfg.variant {
        Variant::Byo => vec![
            format!("Backup: `pg_dump \"$DATABASE_URL\" -F c -f avalon.dump` plus {data}/keys/ (keep it 0600). See `avalon guide backups`."),
            "Upgrade: take a backup, replace the avalon-server binary and restart; pending migrations apply at start. See `avalon guide upgrading`.".to_string(),
        ],
        Variant::Bundled => vec![
            format!("Backup: stop the node, then copy {data} as a whole (keys and the managed database). See `avalon guide backups`."),
            "Upgrade: back up, replace the avalon-server-bundled binary and restart. A PostgreSQL major-version change is not automated. See `avalon guide upgrading`.".to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::prompt::{Auto, Scripted};

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn host() -> Host {
        Host {
            home: Some("/home/op".into()),
            is_root: false,
            cwd: PathBuf::from("/work"),
        }
    }

    fn ok_db(_: &str) -> Result<String, String> {
        Ok("PostgreSQL 16".into())
    }

    fn args(list: &[&str]) -> SetupArgs {
        SetupArgs::parse(&list.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    fn resolve_auto(a: &SetupArgs, existing: &EnvFile) -> Result<NodeConfig, String> {
        let get = no_env;
        let env = Env { get: &get };
        let dir = resolve_data_dir(a, &env, &host(), &mut Auto).unwrap();
        resolve(a, &env, &host(), existing, dir, &mut Auto, &ok_db)
    }

    #[test]
    fn parse_rejects_unknown_and_missing_values() {
        assert!(SetupArgs::parse(&["--bogus".into()]).is_err());
        assert!(SetupArgs::parse(&["--data-dir".into()]).is_err());
        assert!(SetupArgs::parse(&["--role".into(), "x".into()]).is_err());
    }

    #[test]
    fn yes_with_bundled_needs_no_database_and_defaults_to_a_safe_replica() {
        let cfg = resolve_auto(
            &args(&["--yes", "--variant", "bundled"]),
            &EnvFile::default(),
        )
        .unwrap();
        assert_eq!(cfg.variant, Variant::Bundled);
        assert_eq!(cfg.role, Role::Replica);
        assert_eq!(cfg.listen_addr, "127.0.0.1:8080");
        assert_eq!(cfg.libp2p_addr, None);
        assert_eq!(cfg.data_dir, PathBuf::from("/home/op/.avalon"));
        let entries = cfg.desired_entries();
        assert!(entries.contains(&("DATABASE_URL", None)));
        assert!(entries.contains(&("AVALON_REPLICA_ONLY", Some("true".into()))));
    }

    #[test]
    fn yes_byo_without_a_database_fails_with_a_pointer_to_the_flag() {
        let e = resolve_auto(&args(&["--yes"]), &EnvFile::default()).unwrap_err();
        assert!(e.contains("--database-url"));
    }

    #[test]
    fn database_check_failure_is_fatal_unless_skipped() {
        let a = args(&["--yes", "--database-url", "postgres://u:p@h/db"]);
        let get = no_env;
        let env = Env { get: &get };
        let bad = |_: &str| Err("connection refused".to_string());
        let e = resolve(
            &a,
            &env,
            &host(),
            &EnvFile::default(),
            "/d".into(),
            &mut Auto,
            &bad,
        )
        .unwrap_err();
        assert!(e.contains("connection refused"));
        let a = args(&[
            "--yes",
            "--database-url",
            "postgres://u:p@h/db",
            "--skip-db-check",
        ]);
        assert!(resolve(
            &a,
            &env,
            &host(),
            &EnvFile::default(),
            "/d".into(),
            &mut Auto,
            &bad
        )
        .is_ok());
    }

    #[test]
    fn environment_supplies_answers_and_flags_win() {
        let get = |k: &str| match k {
            "DATABASE_URL" => Some("postgres://u:p@h/db".to_string()),
            "AVALON_NETWORK_ID" => Some("from-env".to_string()),
            "AVALON_SERVER_ADDR" => Some("127.0.0.1:9000".to_string()),
            _ => None,
        };
        let env = Env { get: &get };
        let a = args(&["--yes", "--network", "from-flag"]);
        let cfg = resolve(
            &a,
            &env,
            &host(),
            &EnvFile::default(),
            "/d".into(),
            &mut Auto,
            &ok_db,
        )
        .unwrap();
        assert_eq!(cfg.network_id, "from-flag");
        assert_eq!(cfg.listen_addr, "127.0.0.1:9000");
        assert_eq!(cfg.variant, Variant::Byo);
    }

    #[test]
    fn existing_config_supplies_defaults_on_rerun() {
        let existing = EnvFile::parse(
            "DATABASE_URL=postgres://u:p@h/db\nAVALON_NETWORK_ID=avalon-dev-lan\nAVALON_SERVER_ADDR=127.0.0.1:9000\nAVALON_REPLICA_ONLY=true\n",
        );
        let cfg = resolve_auto(&args(&["--yes"]), &existing).unwrap();
        assert_eq!(cfg.network_id, "avalon-dev-lan");
        assert_eq!(cfg.listen_addr, "127.0.0.1:9000");
        assert_eq!(cfg.role, Role::Replica);
    }

    #[test]
    fn public_url_implies_a_fixed_libp2p_port_and_author_gets_webauthn_settings() {
        let cfg = resolve_auto(
            &args(&[
                "--yes",
                "--variant",
                "bundled",
                "--role",
                "author",
                "--public-url",
                "https://node.example.org/",
            ]),
            &EnvFile::default(),
        )
        .unwrap();
        assert_eq!(cfg.libp2p_addr.as_deref(), Some("/ip4/0.0.0.0/tcp/4001"));
        let entries = cfg.desired_entries();
        assert!(entries.contains(&("AVALON_WEBAUTHN_RP_ID", Some("node.example.org".into()))));
        assert!(entries.contains(&(
            "AVALON_WEBAUTHN_ORIGIN",
            Some("https://node.example.org".into())
        )));
        assert!(entries.contains(&("AVALON_REPLICA_ONLY", None)));
    }

    #[test]
    fn invalid_addresses_are_rejected() {
        assert!(resolve_auto(
            &args(&["--yes", "--variant", "bundled", "--listen-addr", "nope"]),
            &EnvFile::default()
        )
        .is_err());
        assert!(resolve_auto(
            &args(&[
                "--yes",
                "--variant",
                "bundled",
                "--public-url",
                "node.example.org"
            ]),
            &EnvFile::default()
        )
        .is_err());
    }

    #[test]
    fn interactive_walk_follows_the_guide_order() {
        let get = no_env;
        let env = Env { get: &get };
        let a = args(&["--skip-db-check"]);
        // variant, database url, role, listen, public url (empty), network, witness/mirror change?
        let mut p = Scripted::new(&[
            "1",
            "postgres://u:p@h/db",
            "1",
            "127.0.0.1:8081",
            "",
            "1",
            "n",
        ]);
        let cfg = resolve(
            &a,
            &env,
            &host(),
            &EnvFile::default(),
            "/d".into(),
            &mut p,
            &ok_db,
        )
        .unwrap();
        assert_eq!(cfg.variant, Variant::Byo);
        assert_eq!(cfg.role, Role::Replica);
        assert_eq!(cfg.listen_addr, "127.0.0.1:8081");
        assert_eq!(cfg.public_url, None);
        assert_eq!(cfg.network_id, network_choices()[0].network_id);
        assert!(p.said().iter().any(|s| s.contains("avalon guide bundled")));
        assert!(p.said().iter().any(|s| s.contains("avalon guide networks")));
    }

    #[test]
    fn interactive_database_retries_then_accepts_a_working_url() {
        let get = no_env;
        let env = Env { get: &get };
        let a = args(&[
            "--variant",
            "byo",
            "--role",
            "replica",
            "--listen-addr",
            "127.0.0.1:8080",
            "--public-url",
            "https://n.example.org",
            "--libp2p-addr",
            "/ip4/0.0.0.0/tcp/4001",
            "--network",
            "x",
            "--witness",
            "on",
        ]);
        let check = |u: &str| {
            if u.contains("good") {
                Ok("ok".into())
            } else {
                Err("refused".into())
            }
        };
        let mut p = Scripted::new(&["postgres://bad", "n", "postgres://good"]);
        let cfg = resolve(
            &a,
            &env,
            &host(),
            &EnvFile::default(),
            "/d".into(),
            &mut p,
            &check,
        )
        .unwrap();
        assert_eq!(cfg.database_url.as_deref(), Some("postgres://good"));
    }

    #[test]
    fn systemd_unit_reflects_variant_and_paths() {
        let cfg = resolve_auto(
            &args(&["--yes", "--variant", "bundled"]),
            &EnvFile::default(),
        )
        .unwrap();
        let unit = systemd_unit(
            &cfg,
            Path::new("/usr/local/bin/avalon-server-bundled"),
            "avalon",
        );
        assert!(unit.contains("EnvironmentFile=/home/op/.avalon/avalon.env"));
        assert!(unit.contains("ExecStart=/usr/local/bin/avalon-server-bundled\n"));
        assert!(unit.contains("ReadWritePaths=/home/op/.avalon"));
        assert!(unit.contains("Environment=HOME=/home/op/.avalon"));
        assert!(unit.contains("User=avalon"));
        let byo = resolve_auto(
            &args(&["--yes", "--database-url", "postgres://u:p@h/d"]),
            &EnvFile::default(),
        )
        .unwrap();
        assert!(
            !systemd_unit(&byo, Path::new("/usr/local/bin/avalon-server"), "op").contains("HOME=")
        );
    }

    #[test]
    fn install_commands_create_the_user_only_when_asked() {
        let cfg = resolve_auto(
            &args(&["--yes", "--variant", "bundled"]),
            &EnvFile::default(),
        )
        .unwrap();
        let with = install_commands(Path::new("/d/avalon.service"), &cfg, "avalon", true);
        assert_eq!(with[0][0], "useradd");
        assert_eq!(
            with.last().unwrap(),
            &["systemctl", "enable", "--now", "avalon"]
        );
        let without = install_commands(Path::new("/d/avalon.service"), &cfg, "avalon", false);
        assert_eq!(without[0][0], "chown");
    }

    #[test]
    fn host_of_handles_ports_paths_and_credentials() {
        assert_eq!(
            host_of("https://a.example.org:8443/x").as_deref(),
            Some("a.example.org")
        );
        assert_eq!(
            host_of("http://u:p@h.example/").as_deref(),
            Some("h.example")
        );
        assert_eq!(host_of("nonsense"), None);
    }

    #[test]
    fn private_urls_are_detected() {
        assert!(is_private_url("http://192.168.7.113:8080"));
        assert!(!is_private_url("https://node.example.org"));
    }

    #[test]
    fn local_base_url_maps_wildcards_to_loopback() {
        let mut cfg = resolve_auto(
            &args(&[
                "--yes",
                "--variant",
                "bundled",
                "--listen-addr",
                "0.0.0.0:8081",
            ]),
            &EnvFile::default(),
        )
        .unwrap();
        assert_eq!(cfg.local_base_url(), "http://127.0.0.1:8081");
        cfg.listen_addr = "127.0.0.1:8080".into();
        assert_eq!(cfg.local_base_url(), "http://127.0.0.1:8080");
    }

    #[test]
    fn closing_notes_differ_per_variant() {
        let bundled = resolve_auto(
            &args(&["--yes", "--variant", "bundled"]),
            &EnvFile::default(),
        )
        .unwrap();
        assert!(closing_notes(&bundled)[0].contains("as a whole"));
        let byo = resolve_auto(
            &args(&["--yes", "--database-url", "postgres://u:p@h/d"]),
            &EnvFile::default(),
        )
        .unwrap();
        assert!(closing_notes(&byo)[0].contains("pg_dump"));
    }
}
