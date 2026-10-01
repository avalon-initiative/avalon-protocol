# Hosting a node: standalone binary

Run `avalon-server` as a single binary against a Postgres database you provide.
No Docker, no repository checkout at runtime. For the Docker route see
[`hosting-quickstart.md`](hosting-quickstart.md).

## Quick start: `avalon setup`

The `avalon` CLI walks through this guide for you. Install the binaries first
([Get the binary](#get-the-binary)): `avalon` comes in the `avalon-<version>-<target>`
tarball together with `avalon-server`; `avalon-server-bundled` is a separate
tarball that holds only that one binary, so for the bundled variant install both
tarballs' binaries. Setup looks for the server next to `avalon`, then on `PATH`
(or use `--server-bin`).

```bash
avalon setup
```

It asks, in the order of this page: the database path (your own Postgres, whose
`DATABASE_URL` it checks by connecting, or the bundled variant), the data
directory, the listen address and public URL, which network to join from the
built-in trust list, optional witness and mirror settings, and whether to
install a systemd service. It writes the answers to `avalon.env` in the data
directory (mode `0600`, the same `KEY=value` format the systemd unit reads),
starts the node, waits for it to answer, checks discovery and that the served
core head verifies against the network's pinned key, and prints the backup and
upgrade notes for your variant. Keys and registration need no step: the node
creates its keys on first start.

For scripts, containers and provisioning, the same flow runs without prompts:

```bash
avalon setup --yes --variant bundled --data-dir /var/lib/avalon \
  --network avalon-dev-local --public-url https://node.example.org
```

`--yes` takes each answer from a flag, then from the environment (the server's
own `DATABASE_URL`, `AVALON_NETWORK_ID`, `AVALON_SERVER_ADDR` and so on), then
from an existing `avalon.env`, then from a default; run `avalon setup --help` for
the flags. Without a terminal and without `--yes`, setup stops instead of
waiting for input.

Things `--yes` does that a prompt would have asked about:

- The node is a replica, which needs no login settings. Pass `--role author` to
  author a shard and serve passkey logins (interactively, "Node role" asks; a
  replica is the recommended choice).
- With no `--network`, the node uses `avalon-dev-local`, which has no seed nodes:
  it is a standalone node, not joined to any network, and setup prints a notice
  saying so. Pass `--network <id>` (see `avalon guide networks`) to join one.
  Interactively there is no default; the network prompt lists which choices have
  seed nodes. When the chosen network has seed nodes, setup exits non-zero unless
  the node knows peers and serves a core head that verifies against the pinned
  key.
- `--service` installs and starts the systemd unit with no confirmation when run
  as root, and creates the service account when it does not exist. On a host
  without a running systemd it writes no unit and says so, and the exit status
  is still 0. The bundled variant refuses to run as root, so its setup runs as an
  ordinary user, writes `avalon.service` and prints the `sudo` commands that
  install it; the generated unit is validated with `systemd-analyze verify`.
- `--public-url` also opens the peer-discovery listener on `0.0.0.0:4001` (the
  HTTP listener stays on `127.0.0.1:8080` unless `--listen-addr` says otherwise).
- `--database-url` puts the password in the process arguments and shell history;
  set the `DATABASE_URL` environment variable instead. The interactive prompt does
  not echo the URL and never prints an existing one.

Setup refuses shared directories (`/`, `/etc`, `/var/lib`, your home, ...) and
non-empty directories that are not an earlier Avalon data directory as
`--data-dir`, since it may change ownership of the directory when installing the
service. Passkey settings (`AVALON_WEBAUTHN_RP_ID`, `AVALON_WEBAUTHN_ORIGIN`) are
never replaced on a rerun, even with `--force`, because that invalidates
registered passkeys; only `--force-webauthn` changes them.

Rerunning is safe. Existing keys are never touched, and a value in `avalon.env`
that differs from what you now ask for is kept and reported; pass `--force` to
change it. On a host running systemd, a unit is generated next to the config
(`avalon.service`) and installed and started only when you confirm and run as
root; otherwise setup prints the commands to run. `--no-start`, `--no-verify` and `--skip-db-check`
skip those steps.

The whole guide is embedded in the binary from these files at build time:
`avalon guide` lists topics and `avalon guide <topic>` prints one (for example
`avalon guide backups`). The rest of this page is the manual route and the
reference for what setup writes.

## Availability today

- No release has been tagged yet, so there is no prebuilt download. Releases
  will appear on the
  [Releases page](https://github.com/avalon-initiative/avalon-protocol/releases)
  as `avalon-<version>-<target>.tar.gz` for `x86_64-unknown-linux-gnu`,
  `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin` and `aarch64-apple-darwin`,
  each holding `avalon-server` and the `avalon` CLI, and (Linux targets only)
  `avalon-server-bundled-<version>-<target>.tar.gz` holding the bundled variant.
  Until then, [build the same binary from source](#build-from-source).
- A prebuilt, multi-arch container image is published alongside the tarballs
  at `ghcr.io/avalon-initiative/avalon-protocol` — see
  [`hosting-quickstart.md`](hosting-quickstart.md#pulling-and-running-the-image-directly)
  for the Docker route.
- A variant that bundles its own database, `avalon-server-bundled`, is
  available as of this release — see [Bundled variant](#bundled-variant).
- Not available yet: Windows binaries, a bundled variant on macOS, and a
  public network entry to join (#994). NAT traversal is planned; a node still needs an inbound
  port, see [Ports](#ports).
- The Linux x86_64 steps below were run against the release tarballs built by
  the release workflow's dry run; see [Verified](#verified).

## What you need

- A Linux host with `curl`, `tar` and CA certificates (`ca-certificates`); a
  minimal image has none of them. `curl` is also what the checks below use.
- For the plain binary: a PostgreSQL 16 database and a role that owns it. Any Postgres you operate
  or rent works; the server needs no extensions. To create one on a server you
  administer:

  ```sql
  CREATE ROLE avalon LOGIN PASSWORD 'choose-a-password';
  CREATE DATABASE avalon OWNER avalon;
  ```

  The connection string is `postgres://avalon:choose-a-password@db-host:5432/avalon`
  (percent-encode special characters in the password). To run Postgres
  yourself, see the [PostgreSQL install docs](https://www.postgresql.org/download/).
  The backup commands below need the `pg_dump`/`pg_restore` client tools of the
  same major version (`postgresql-client-16` on Ubuntu 24.04).
- For the [bundled variant](#bundled-variant) instead of a database: a
  non-root user, `libxml2` and `tzdata` (a bare `ubuntu:24.04` has neither, and
  the first start fails without them), and outbound HTTPS on the first start to
  download PostgreSQL.
- One inbound TCP port for HTTP (`AVALON_SERVER_ADDR`, default `127.0.0.1:8080`)
  and one for the peer-discovery swarm (`AVALON_LIBP2P_LISTEN_ADDR`).
- A TLS-terminating reverse proxy in front of the HTTP port before it is
  reachable beyond loopback: [`deployment.md`](deployment.md).

## Get the binary

### From a release

Download the tarball for your platform and `SHA256SUMS`, verify them as
described in [`verifying-a-release.md`](verifying-a-release.md), then:

```bash
tar -xzf avalon-<version>-<target>.tar.gz
sudo install -m 0755 avalon-<version>-<target>/avalon-server avalon-<version>-<target>/avalon /usr/local/bin/
```

The tarball has no `migrate` binary; the server applies migrations itself (see
[Upgrading](#upgrading)). The [bundled variant](#bundled-variant) is a
separate download, `avalon-server-bundled-<version>-<target>.tar.gz` (Linux
only), which holds only that binary; install it the same way and keep `avalon`
from the first tarball for `avalon setup`.

The version is in the tarball's name, in `--version`, and in
`protocol_version` of `GET /nodes/discover`.

`avalon-server`, `avalon-server-bundled` and `avalon` all answer `--version` and
`--help` without reading configuration or touching the data directory. The two
server binaries take no other arguments (an unknown argument prints a usage
line and exits with status 2); configuration is environment-driven.

### Build from source

```bash
git clone https://github.com/avalon-initiative/avalon-protocol.git
cd avalon-protocol
cargo build --release --locked -p avalon-server --bin avalon-server --bin migrate \
  --features vendored-openssl
```

`--features vendored-openssl` builds OpenSSL in and matches the release build;
it needs a C toolchain and `perl`. Without it the binary links the system
`libssl` and the build needs `pkg-config` and the OpenSSL development package.
The binaries are `target/release/avalon-server` and `target/release/migrate`.

The [bundled variant](#bundled-variant), `avalon-server-bundled`, is a
separate binary target behind its own `bundled-postgres` feature — plain
`avalon-server` never depends on it:

```bash
cargo build --release --locked -p avalon-server --bin avalon-server-bundled \
  --features bundled-postgres,vendored-openssl
```

A binary built this way also reads a `.env` file at the root of the checkout it
was built from, if one exists. Variables already in the environment win over it.

## Configuration

Configuration is environment variables. `.env.example` in the repository lists
every setting. The minimum, with the defaults for everything else:

| Variable | Meaning |
|---|---|
| `DATABASE_URL` | Postgres connection string. Required. |
| `AVALON_NETWORK_ID` | The network this node belongs to. Required, no default. It must match the `network_id` of the network's entry in [`docs/trusted-networks.json`](../../../trusted-networks.json), or be a new name for a network of your own. A database remembers the id it was created with and refuses to start under a different one. |
| `AVALON_DATA_DIR` | Where generated keys live (`keys/`). Default `./data` relative to the working directory; set an absolute path for a service. |
| `AVALON_SERVER_ADDR` | HTTP bind address. Default `127.0.0.1:8080`. |
| `AVALON_LIBP2P_LISTEN_ADDR` | Peer-discovery bind, e.g. `/ip4/0.0.0.0/tcp/4001`. Default is an ephemeral port, which cannot be opened in a firewall ahead of time; set a fixed one. |
| `AVALON_AUTONAT_ENABLED` | Reachability detection and dial-back service on the peer-discovery swarm. Default `true`. |
| `AVALON_AUTONAT_DIALBACKS_PER_MINUTE` | Most dial-backs this node performs for other nodes per minute, in total. Default `30`; `0` answers none. |
| `AVALON_AUTONAT_ALLOW_PRIVATE_DIALBACK` | Dial back and probe through private and loopback peers, for a fleet on one LAN. Default `false`; needs `AVALON_ALLOW_PRIVATE_PEERS=true` too. |
| `AVALON_RELAY_SERVER_ENABLED` | Serve as a circuit relay for nodes that cannot be dialed. Default `false`. Only useful on a node that is publicly reachable (set `AVALON_LIBP2P_EXTERNAL_ADDR`); it costs bandwidth, capped by the limits below. |
| `AVALON_RELAY_MAX_RESERVATIONS`, `AVALON_RELAY_MAX_RESERVATIONS_PER_PEER` | Reservations held at once, and per peer. Defaults `128` and `2`. |
| `AVALON_RELAY_RESERVATION_SECS` | Reservation lifetime before a client must renew. Default `3600`. |
| `AVALON_RELAY_MAX_CIRCUITS`, `AVALON_RELAY_MAX_CIRCUITS_PER_PEER` | Relayed connections open at once, and per peer. Defaults `16` and `4`. |
| `AVALON_RELAY_MAX_CIRCUIT_SECS`, `AVALON_RELAY_MAX_CIRCUIT_BYTES` | Lifetime and byte cap of one relayed connection. Defaults `120` and `524288`. Every relay limit must be at least 1 and has a ceiling. A relay reports its limits and current use (reservations and circuits held now, and totals accepted, denied and closed) in the `relay_server` section of `GET /nodes/status`. |
| `AVALON_RELAY_CLIENT_ENABLED` | A node that detects it is not dialable reserves a slot on a relay. Default `true`. |
| `AVALON_RELAY_CLIENT_MAX_RESERVATIONS` | Relay reservations to hold at once. Default `2`, at most `8`. |
| `AVALON_RELAY_ADDRS` | Comma-separated relay addresses to use first, each ending in `/p2p/<relay peer id>`. Relays found among connected peers are used after these. |
| `AVALON_NODE_HTTP_MAX_REQUEST_BYTES`, `AVALON_NODE_HTTP_MAX_RESPONSE_BYTES` | Largest request body this node accepts, and response body it reads, on node-to-node HTTP carried over libp2p streams. Defaults 1 MiB and 8 MiB, ceilings 16 MiB and 64 MiB. A relay's own circuit byte cap still applies to relayed streams. |
| `AVALON_NODE_HTTP_TIMEOUT_SECS` | Time bound on one stream exchange, dial included. Default `30`, at most `300`. |
| `AVALON_NODE_HTTP_CONNECT_TIMEOUT_SECS` | How long a stream request waits for the dial to a peer before it counts as never sent, so a read can fall back to the peer's URL and the stream is tried later than the URL. Default `5`, at most `60`. |
| `AVALON_NODE_HTTP_MAX_INFLIGHT`, `AVALON_NODE_HTTP_MAX_INFLIGHT_PER_PEER` | Stream requests served at once, in total and per peer; beyond them the answer is an immediate 429. Defaults `64` and `8`. |
| `AVALON_NODE_HTTP_BUFFER_BUDGET_BYTES` | Request-body bytes buffered at once across all stream requests; a request that cannot get its share within 2 seconds is answered 429 unread. Default 64 MiB, at most 1 GiB. |
| `AVALON_LIBP2P_MAX_CONNECTIONS`, `AVALON_LIBP2P_MAX_CONNECTIONS_PER_PEER`, `AVALON_LIBP2P_MAX_PENDING_INCOMING` | Swarm connection limits: established in total and per peer, and incoming connections still handshaking. Defaults `512`, `4` and `64`. |
| `AVALON_DCUTR_ENABLED` | Try to replace a relayed connection with a direct one by hole punching. Default `true`. A failed attempt leaves the relayed connection in use. |
| `AVALON_NODE_URL` | The public base URL other nodes use to reach this one. Without it a node running libp2p announces itself as `p2p://<its peer id>` and is reached only over libp2p streams (see "Running without a public URL" below); with neither it does not announce itself. |
| `AVALON_WEBAUTHN_RP_ID`, `AVALON_WEBAUTHN_ORIGIN` | Relying-party id and origin for passkey login. Required unless the node is a replica. |

Keys are generated on first start and stored under `AVALON_DATA_DIR/keys/`;
see "Keys generated on first start" in
[`hosting-quickstart.md`](hosting-quickstart.md). Two nodes must never share a
data directory.

Migrations are embedded in the binary and applied on every start.
`AVALON_MIGRATIONS_DIR` optionally points at a directory of migrations instead;
it is meant for development.

### A replica that joins a network

A replica mirrors and serves a network's history. It authors nothing, holds no
signing key and needs no registration.

```bash
export DATABASE_URL='postgres://avalon:...@db-host:5432/avalon'
export AVALON_NETWORK_ID='<network_id from the trust entry>'
export AVALON_DATA_DIR=/var/lib/avalon
export AVALON_REPLICA_ONLY=true
export AVALON_SERVER_ADDR=0.0.0.0:8080
export AVALON_LIBP2P_LISTEN_ADDR=/ip4/0.0.0.0/tcp/4001
export AVALON_NODE_URL=https://node.example.org
avalon-server
```

With `AVALON_REPLICA_ONLY=true` the node generates only its witness and
peer-discovery keys, needs no WebAuthn settings, and answers any write with
`403 REPLICA_ONLY`. Setting it together with `AVALON_OWN_SHARD_ID` is refused
at start.

### A node that authors its own shard

Leave `AVALON_REPLICA_ONLY` unset and set the WebAuthn pair:

```bash
export AVALON_WEBAUTHN_RP_ID=example.org
export AVALON_WEBAUTHN_ORIGIN=https://example.org
```

With no `AVALON_OWN_SHARD_ID` and no remote authority configured, the node
authors a self-certifying shard, `node:<hash of its public key>`, using its
generated signing key. To author a named shard or `core`, see
[`choosing-your-shard.md`](choosing-your-shard.md). A node that authors a named
shard should also mirror `core` (`AVALON_MIRROR_PEERS`), or clients pinned to
the network cannot verify it; the start-up log warns when this is missing.

### Connecting to a network

A network is described by its entry in
[`docs/trusted-networks.json`](../../../trusted-networks.json): the
`network_id`, the pinned `verify_key` and the `seed_nodes`. Set
`AVALON_NETWORK_ID` to the entry's id and the node uses its seed nodes to
announce itself and, when it does not author `core` and
`AVALON_MIRROR_PEERS` is unset, to mirror `core` (`AVALON_MIRROR_PEERS`,
`AVALON_BOOTSTRAP_PEERS` override). See [`seed-nodes.md`](seed-nodes.md) and
[`avalon-docs: protocol/network-trust-anchors.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/protocol/network-trust-anchors.md).

The entries in the file today:

- `avalon-dev-lan` is the maintainers' private-network test bed; its seeds are
  on private addresses and unreachable from elsewhere. Nodes only admit peers on
  private or loopback addresses when `AVALON_ALLOW_PRIVATE_PEERS=true`.
- `avalon-dev-local` is a template with no seeds, for a single local node or
  a network of your own.

There is no public network entry yet (#994).

## First start

```bash
avalon-server
```

On the first start of a self-authoring node the log shows, in order:

```
generated node keys on first boot keys=["AVALON_SETTLEMENT_SIGNING_KEY", ...]
authoring this node's self-certifying shard shard=node:<hash>
avalon-server: ledger network_id network_id=<your network>
avalon-dht: listening address=/ip4/.../tcp/<port> peer_id=...
avalon-server listening addr=<AVALON_SERVER_ADDR>
```

A replica logs `generated node keys on first boot` for two keys, then
`replica-only mode, authoring no shard and signing no tree heads` and, when
mirroring, `mirror-watcher: watching 1 peer(s) ... core=<url>`. On later starts
`loaded node keys from the data directory` replaces the first line. A `WARN`
about `Failed to trigger bootstrap: No known peers` is normal on a node with no
peers yet.

Check that it is serving:

```bash
curl -s http://127.0.0.1:8080/nodes/discover
```

The response lists this node's own status and the peers it knows. On a
replica, a well-formed write is refused with `403` and code `REPLICA_ONLY`:

```bash
curl -s -X POST http://127.0.0.1:8080/identities/register/start \
  -H 'content-type: application/json' \
  -d '{"identity_id":"3f2b1c94-7d1e-4a55-9c1a-2b6f0e8d7a10","display_name":"probe"}'
```

(A body that fails validation is answered `422` before the replica check.)

## Ports

Both listeners must be reachable by other nodes: the HTTP port (through your
TLS proxy, as `AVALON_NODE_URL`) and the `AVALON_LIBP2P_LISTEN_ADDR` port. If
the host sits behind NAT or a container network, also set
`AVALON_LIBP2P_EXTERNAL_ADDR` to the address peers should dial; it is advertised as
given and checked by reachability detection. Only confirmed addresses are announced
otherwise, so a seed node must set it. `GET /nodes/status` shows the result as
`reachability` (`unknown`, `public` or `private`) and `confirmed_external_addrs`. Nodes
behind NAT with no forwarded port report `private` and cannot yet take part as
dialable peers; NAT traversal is planned.

## Running without a public URL

A node with no `AVALON_NODE_URL` and libp2p enabled announces itself as `p2p://<its libp2p peer id>`.
Its neighbors admit it only when the announce arrives on a libp2p stream authenticated as that same
peer id, so `AVALON_ANNOUNCE_VERIFY_REACHABILITY` stays on and nothing is dialed back. An announce
that names a `p2p://` URL over plain HTTP, or on a stream authenticated as another peer, stores
nothing. Peers reach the node over the connection it opened, a relay circuit or a hole-punched
connection; the node's own outbound requests (mirror polling, settlement submit) are unchanged.
It shows in `/nodes/peers`, `/nodes/discover` and `/nodes/topology` under its `p2p://` URL,
usually with `connectivity` `relayed` or `outbound_only`. The node needs one reachable seed in
`AVALON_BOOTSTRAP_PEERS` to start from. See
[`running-without-an-open-port.md`](running-without-an-open-port.md).

Limitations: a `p2p://` peer proves only that it holds its key, so it cannot call the routes that
inject data without their own credential (`/nodes/relay`, `/nodes/replicate-chat`,
`/mirror/notify`); neighbors refuse its pushes there until it has real credentials, and it
receives no chat or mirror pushes of its own because its URL is not advertised for interest
lookups, so it falls back to polling. At most 64 `p2p://` entries are kept per node and they are
evicted first. The node's `p2p://` URL is never given to browsers or used in signed grants.

## Running under systemd

Create the service account and configuration:

```bash
sudo useradd --system --home-dir /var/lib/avalon --shell /usr/sbin/nologin avalon
sudo install -d -m 0750 -o root -g avalon /etc/avalon
sudo install -m 0640 -o root -g avalon /dev/null /etc/avalon/avalon.env
```

`/etc/avalon/avalon.env` holds one `KEY=value` per line, no quotes and no
`export`; a `%` in a value (a percent-encoded password) is taken literally:

```
DATABASE_URL=postgres://avalon:...@db-host:5432/avalon
AVALON_NETWORK_ID=avalon-dev-local
AVALON_SERVER_ADDR=127.0.0.1:8080
AVALON_LIBP2P_LISTEN_ADDR=/ip4/0.0.0.0/tcp/4001
AVALON_NODE_URL=https://node.example.org
AVALON_WEBAUTHN_RP_ID=example.org
AVALON_WEBAUTHN_ORIGIN=https://example.org
```

`/etc/systemd/system/avalon.service`:

```ini
[Unit]
Description=Avalon node
After=network-online.target postgresql.service
Wants=network-online.target

[Service]
Type=simple
User=avalon
Group=avalon
EnvironmentFile=/etc/avalon/avalon.env
Environment=AVALON_DATA_DIR=/var/lib/avalon
StateDirectory=avalon
StateDirectoryMode=0700
ExecStart=/usr/local/bin/avalon-server
Restart=on-failure
RestartSec=5
LimitNOFILE=65536
NoNewPrivileges=true
ProtectSystem=strict
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now avalon
journalctl -u avalon -f
```

`ExecStart` must be an absolute path. `StateDirectory=avalon` creates
`/var/lib/avalon` owned by the service user and keeps it writable under
`ProtectSystem=strict`. `systemctl stop avalon` sends `SIGTERM`; the node stops
accepting connections, finishes in-flight requests and its current outbox batch,
and exits with status 0. Remove the `postgresql.service` ordering if the
database is on another host.

`avalon setup --service` generates a similar unit instead: its
`EnvironmentFile` is `avalon.env` in the data directory, it adds
`ReadWritePaths=` for that directory and it has no `StateDirectory=`.

### Shutdown

On the first `SIGTERM` or `SIGINT` once the node is serving, it stops accepting
connections, sends websocket clients a close frame (1001, going away), waits
for in-flight requests to finish, and lets the ledger outbox complete its
current batch, then exits with status 0. The request drain and the outbox
wait are each bounded by `AVALON_SHUTDOWN_TIMEOUT_SECS` (default `30`, read
after `.env` is loaded); requests still running at the bound are cut, and
multi-step writes rely on their database transactions, which roll back. If both
phases together exceed twice the bound plus a second, or a second signal
arrives, the node exits immediately with status 1. A signal during start-up
(before it is serving, including migrations, each of which runs in a
transaction) exits at once with status 128 plus the signal number (143 for
`SIGTERM`), and the bundled variant terminates any PostgreSQL it had started. In
the bundled variant the managed PostgreSQL is stopped after the drain, outside
that deadline, with its own bound of `AVALON_SHUTDOWN_TIMEOUT_SECS` for a fast
shutdown followed by up to ten seconds of forced termination, so no `postgres`
process is left behind.

## Upgrading

Take a [backup](#backup) first, then replace the binary and restart:

```bash
sudo install -m 0755 avalon-server /usr/local/bin/avalon-server
sudo systemctl restart avalon
```

Pending migrations are applied at start, before the node serves requests. To
apply them on their own first, run the `migrate` binary (built from source; it is
not in the release tarball) with the same `DATABASE_URL`:

```bash
DATABASE_URL=... ./migrate up      # apply pending migrations
DATABASE_URL=... ./migrate down    # revert the most recent one
```

`up` is safe to repeat. `reset` drops and recreates the schema and must never be
run against a database you care about. For choosing versions, rolling back and
multi-node rollouts, see [`upgrading.md`](upgrading.md); its commands assume
the Docker layout.

## Backup

Back up two things together:

- The database: `pg_dump "$DATABASE_URL" -F c -f avalon.dump`.
- `AVALON_DATA_DIR/keys/`. The signing key lets the node extend its ledger; a
  node restored without it cannot continue its shard. Keep the copy `0600`.

To move a node, restore both and start the binary with the same
`AVALON_DATA_DIR` contents:

```bash
pg_restore --no-owner -d "$NEW_DATABASE_URL" avalon.dump   # into an empty database
mkdir -p /var/lib/avalon && chmod 0700 /var/lib/avalon
tar -xzf keys.tgz -C /var/lib/avalon    # made with: tar -C /var/lib/avalon -czf keys.tgz keys
```

The restored node serves the same shard and peer id as the original.

## Bundled variant

`avalon-server-bundled` is the same server with one difference: if
`DATABASE_URL` is not set, it starts, initializes and supervises its own
private PostgreSQL instance instead of requiring one you provide. It is for
an operator who wants a single command and no separate database step — a
quick evaluation node, a small personal deployment, or anywhere standing up
a separate Postgres isn't worth it. For anything you're already running
Postgres for (a fleet, a managed database service, an existing cluster),
use the plain `avalon-server` binary with your own `DATABASE_URL` instead;
the bundled instance is not tuned or intended for that.

If `DATABASE_URL` is set in the environment, `avalon-server-bundled` behaves
exactly like `avalon-server` and starts no managed database at all — it is
never overridden.

With no `DATABASE_URL`, on first start it:

- Downloads a PostgreSQL distribution (17.11 in the 0.1.0 build) into
  `AVALON_DATA_DIR/postgres/install/<version>`, unless it is already there.
  This needs outbound HTTPS and takes about half a minute on a fast link.
- Initializes a data directory at `AVALON_DATA_DIR/postgres/data`, with the
  superuser password generated once and kept at
  `AVALON_DATA_DIR/postgres/superuser_password` (mode `0600`).
- Starts PostgreSQL bound to `127.0.0.1` on an available local port chosen
  at random each start (never `5432`, and never reachable except from this
  process on the same host) and creates an `avalon` database.
- Exports the resulting `DATABASE_URL` into its own process environment
  before doing anything else, so every step after this is identical to the
  plain binary's.

On every later start with the same `AVALON_DATA_DIR` and still no
`DATABASE_URL`, it reuses the existing data directory and password rather
than reinitializing. Stopping the process (`SIGTERM` or `SIGINT`) drains the
server as described under [systemd](#running-under-systemd), then stops the
managed PostgreSQL cleanly before the process exits with status 0; killing it
(`SIGKILL`) does not, and can leave PostgreSQL running under
`AVALON_DATA_DIR/postgres/data` — check for a stray `postgres` process
under that data directory and stop it manually if this happens
(`pg_ctl -D AVALON_DATA_DIR/postgres/data stop`, using the
`pg_ctl` under `AVALON_DATA_DIR/postgres/install`).

It refuses to start as root: PostgreSQL itself refuses to run as root, and
this fails with a clear error at startup instead of a confusing one from
deep inside PostgreSQL's own start sequence. Run it as an ordinary user,
same as [systemd](#running-under-systemd) already does for the plain
binary.

The embedded PostgreSQL also needs host packages that a minimal image may lack.
Before it starts, the server checks the host:

- Blocking: running as root; a timezone database (`tzdata`); after the download,
  every shared library `ldd` reports as unresolved for the PostgreSQL binary
  (typically `libxml2`); and, only while PostgreSQL still has to be downloaded,
  outbound HTTPS to `github.com` (the probe has an 8 second overall limit and is
  reported as skipped, not passed, when `HTTPS_PROXY`/`ALL_PROXY` is set).
- Warning only: before the download, whether `libxml2.so.2` is present (checked
  with `ldconfig -p` and the standard library directories, which cannot see every
  layout such as NixOS), and that `ldd` itself is missing so the downloaded
  binary could not be checked.

A blocking finding stops the start with the package and the install command, for
example `sudo apt-get install -y libxml2 tzdata` on Debian and Ubuntu or
`sudo dnf install -y libxml2 tzdata` on Fedora and RHEL. `avalon setup --variant
bundled` runs the same check for the user the node will run as and prints the
result. If PostgreSQL still fails to start, the error includes the tail of its
log and, for a missing library or timezone data, the same hint. Set
`AVALON_BUNDLED_SKIP_PREFLIGHT=1` to skip every check.
`AVALON_BUNDLED_PREFLIGHT_SIMULATE_MISSING` (a comma-separated list of `root`,
`libxml2`, `tzdata`, `network`, `ldd`) reports those as missing, for testing the
messages.

### Run and service

The simplest route is `avalon setup --variant bundled` as the ordinary user
that will own the node; it writes `avalon.env` in the data directory and starts
the server. To start it yourself from that file:

```bash
set -a; . ~/avalon-data/avalon.env; set +a; exec avalon-server-bundled
```

To run it under systemd, run `avalon setup --variant bundled --service` as that
user; it writes `avalon.service` and prints the `chown`, `install` and
`systemctl` commands to run with `sudo`. Stopping the unit stops the managed
PostgreSQL and the process exits with status 0.

### Upgrading the binary

Back up, then replace `avalon-server-bundled` with the new build (`sudo install`
works while it runs) and restart it. The data directory, keys and database are
reused and migrations apply at start.

### Backup

Back up `AVALON_DATA_DIR` as a whole — it now holds both the keys
directory (as above) and the managed database's entire data directory
(`AVALON_DATA_DIR/postgres/data`) and generated password
(`AVALON_DATA_DIR/postgres/superuser_password`), so one directory is the
whole backup, instead of a `pg_dump` plus the keys directory separately.
Stop the process (`SIGTERM`; no `postgres` process is left) before copying
the data directory for a consistent on-disk snapshot, or use PostgreSQL's own
[continuous archiving](https://www.postgresql.org/docs/current/continuous-archiving.html)
against it if you need backups without downtime — the bundled variant does
not set this up for you.

To restore, extract the copy as the same non-root user, point
`AVALON_DATA_DIR` in `avalon.env` at the new location and start the binary. The
copy includes the downloaded PostgreSQL, so nothing is downloaded again; the
node comes back with the same keys and peer id.

### PostgreSQL major-version upgrades

This is not solved yet. The bundled variant pins a specific PostgreSQL major
version internally, and there is no in-place major-version upgrade path for
the managed data directory today — moving to a later major version means
the standard PostgreSQL dump/restore path (`pg_dump` from the old version,
`pg_restore` into a freshly initialized data directory under the new
version), done by hand, not something this binary automates. If this
matters to you before it's addressed, treat the bundled variant as
disposable state you can rebuild from a fresh registration rather than
data you upgrade in place, or run the plain `avalon-server` binary against
a Postgres you manage yourself, where you already control this.

## Troubleshooting

Each of these was produced by running the binary.

| Symptom | Cause and fix |
|---|---|
| `DATABASE_URL must be set: NotPresent` (panic) | The variable is not in the process environment. Under systemd, check `EnvironmentFile=` points at the file and it is readable by the service user. |
| `AVALON_NETWORK_ID must be set` (panic) | Set it; it has no default. |
| `AVALON_WEBAUTHN_RP_ID must be set` (panic) | A node that serves logins needs both WebAuthn variables. A replica does not. |
| `refusing to start: bundled-postgres cannot run as root` | Run `avalon-server-bundled` (and `avalon setup --variant bundled`) as an ordinary user. |
| Bundled: `error while loading shared libraries: libxml2.so.2` in `avalon.log` | Install `libxml2`. |
| Bundled: `invalid value for parameter "TimeZone": "UTC"` | Install `tzdata`. |
| `failed to connect to Postgres: Database(PgDatabaseError ... password authentication failed` | The role or password in `DATABASE_URL` is wrong. |
| `failed to build Webauthn instance ... relative URL without a base` | `AVALON_WEBAUTHN_ORIGIN` must be a full URL such as `https://example.org`. |
| `failed to connect to Postgres: PoolTimedOut` after about 30 seconds | The database host or port is unreachable or refuses the role. |
| `refusing to start: node key file .../settlement_signing.key does not hold 32 bytes of hex; refusing to replace it` | A file in `keys/` is malformed. The node never overwrites it. Restore it from backup, or if the key is disposable and the node has authored nothing, delete the file to have it regenerated. |
| `refusing to start: AVALON_REPLICA_ONLY=true conflicts with AVALON_OWN_SHARD_ID` | A replica authors no shard. Unset one of the two. |
| `refusing to start: ledger genesis network_id ... does not match configured AVALON_NETWORK_ID ...` | The database was created under another network id. Use that id, or a new database. |
| `failed to bind address: ... AddrInUse` | Another process holds `AVALON_SERVER_ADDR`. |
| Two nodes log the same `shard=node:<hash>` and the same `peer_id` | They share an `AVALON_DATA_DIR`. This is not refused. Give each node its own directory and its own database. |
| Another node's `node-announce` to yours logs `HTTP 403 Forbidden` | The announcing URL is a private or loopback address and `AVALON_ALLOW_PRIVATE_PEERS` is not `true` on the receiving node. |
| A replica logs `mirror-watcher: ... 404 Not Found` for `/ledger/sth/latest?shard_id=core` | The node it mirrors from has no `core` head: it is not a `core` author or has written nothing yet. Point `AVALON_MIRROR_PEERS` at the network's `core` source. |

## Verified

Walked through on 2026-09-28 in bare `ubuntu:24.04` containers (no Rust
toolchain, no checkout), with the `x86_64-unknown-linux-gnu` tarballs
(`avalon-0.1.0-...` and `avalon-server-bundled-0.1.0-...`) built by a manual
dry run of the release workflow on `main` (`7c37e2b`). The checksums matched the
workflow's `SHA256SUMS`. Exercised as written, with the corrections in this
page:

- Plain variant against a separate `postgres:16` container: checksum, extract,
  install, `avalon setup` interactively (through a pty) and with `--yes`, rerun
  (existing values kept), first start and migrations, `GET /nodes/discover`, a
  replica refusing writes, the manual systemd unit from this page and the unit
  `avalon setup --service` generates (both pass `systemd-analyze verify` and
  start and stop under systemd in a privileged container), binary replacement
  and restart, `pg_dump` plus `keys/` restored into a fresh database and
  container (same shard), and the startup errors in the troubleshooting table.
- Bundled variant as a non-root user: refusal as root, `avalon setup --yes
  --variant bundled`, first start, restart reusing the data directory (a new
  random PostgreSQL port each start), `SIGTERM` stopping the managed
  PostgreSQL, the generated service unit under systemd, in-place binary
  replacement, and a whole-directory backup restored into a fresh container at
  a different path (same keys and peer id).

Not exercised yet, and waiting on the first real release: `gh attestation
verify` against a published artifact, downloading from a GitHub Releases URL,
the published container image, joining a network with seed nodes (the
walk-through was standalone on `avalon-dev-local`), the `aarch64` and macOS
tarballs, and the interactive bundled and service prompts.
