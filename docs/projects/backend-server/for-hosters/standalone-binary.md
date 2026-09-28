# Hosting a node: standalone binary

Run `avalon-server` as a single binary against a Postgres database you provide.
No Docker, no repository checkout at runtime. For the Docker route see
[`hosting-quickstart.md`](hosting-quickstart.md).

## Availability today

- No release has been tagged yet, so there is no prebuilt download. Releases
  will appear on the
  [Releases page](https://github.com/avalon-initiative/avalon-protocol/releases)
  as `avalon-<version>-<target>.tar.gz` for `x86_64-unknown-linux-gnu` and
  `aarch64-unknown-linux-gnu`, each holding `avalon-server` and the `avalon`
  CLI. Until then, [build the same binary from source](#build-from-source).
- A prebuilt, multi-arch container image is published alongside the tarballs
  at `ghcr.io/avalon-initiative/avalon-protocol` — see
  [`hosting-quickstart.md`](hosting-quickstart.md#pulling-and-running-the-image-directly)
  for the Docker route.
- A variant that bundles its own database, `avalon-server-bundled`, is
  available as of this release — see [Bundled variant](#bundled-variant).
- Not available yet: macOS and Windows binaries, and a public network entry
  to join (#994). NAT traversal is planned; a node still needs an inbound
  port, see [Ports](#ports).
- Everything below was run against the binary built from `main`.

## What you need

- A Linux host.
- A PostgreSQL 16 database and a role that owns it. Any Postgres you operate
  or rent works; the server needs no extensions. To create one on a server you
  administer:

  ```sql
  CREATE ROLE avalon LOGIN PASSWORD 'choose-a-password';
  CREATE DATABASE avalon OWNER avalon;
  ```

  The connection string is `postgres://avalon:choose-a-password@db-host:5432/avalon`
  (percent-encode special characters in the password). To run Postgres
  yourself, see the [PostgreSQL install docs](https://www.postgresql.org/download/).
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
sudo install -m 0755 avalon-<version>-<target>/avalon-server /usr/local/bin/
```

The tarball has no `migrate` binary; the server applies migrations itself (see
[Upgrading](#upgrading)). The [bundled variant](#bundled-variant) is a
separate download, `avalon-server-bundled-<version>-<target>.tar.gz`.

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
| `AVALON_NODE_URL` | The public base URL other nodes use to reach this one. Without it the node does not announce itself. |
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
[`../architecture/network-trust-anchors.md`](../architecture/network-trust-anchors.md).

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
replica, `POST /identities/register/start` returns `403` with code
`REPLICA_ONLY`.

## Ports

Both listeners must be reachable by other nodes: the HTTP port (through your
TLS proxy, as `AVALON_NODE_URL`) and the `AVALON_LIBP2P_LISTEN_ADDR` port. If
the host sits behind NAT or a container network, also set
`AVALON_LIBP2P_EXTERNAL_ADDR` to the address peers should dial. Nodes behind
NAT with no forwarded port cannot yet take part; NAT traversal is planned.

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
`ProtectSystem=strict`. `systemctl stop avalon` sends `SIGTERM`; the node exits
cleanly with status 0. Remove the `postgresql.service` ordering if the database
is on another host.

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
`AVALON_DATA_DIR` contents.

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

- Downloads a PostgreSQL distribution (cached under `~/.theseus/postgresql`
  by the embedded-Postgres library, not under `AVALON_DATA_DIR`) unless
  already cached.
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
than reinitializing. Stopping the process (`SIGTERM` or `SIGINT`) stops the
managed PostgreSQL cleanly before the process exits; killing it
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

### Backup

Back up `AVALON_DATA_DIR` as a whole — it now holds both the keys
directory (as above) and the managed database's entire data directory
(`AVALON_DATA_DIR/postgres/data`) and generated password
(`AVALON_DATA_DIR/postgres/superuser_password`), so one directory is the
whole backup, instead of a `pg_dump` plus the keys directory separately.
Stop the process before copying the data directory for a consistent
on-disk snapshot, or use PostgreSQL's own
[continuous archiving](https://www.postgresql.org/docs/current/continuous-archiving.html)
against it if you need backups without downtime — the bundled variant does
not set this up for you.

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
| `failed to build Webauthn instance ... relative URL without a base` | `AVALON_WEBAUTHN_ORIGIN` must be a full URL such as `https://example.org`. |
| `failed to connect to Postgres: PoolTimedOut` after about 30 seconds | The database host or port is unreachable or refuses the role. |
| `refusing to start: node key file .../settlement_signing.key does not hold 32 bytes of hex; refusing to replace it` | A file in `keys/` is malformed. The node never overwrites it. Restore it from backup, or if the key is disposable and the node has authored nothing, delete the file to have it regenerated. |
| `refusing to start: AVALON_REPLICA_ONLY=true conflicts with AVALON_OWN_SHARD_ID` | A replica authors no shard. Unset one of the two. |
| `refusing to start: ledger genesis network_id ... does not match configured AVALON_NETWORK_ID ...` | The database was created under another network id. Use that id, or a new database. |
| `failed to bind address: ... AddrInUse` | Another process holds `AVALON_SERVER_ADDR`. |
| Two nodes log the same `shard=node:<hash>` and the same `peer_id` | They share an `AVALON_DATA_DIR`. This is not refused. Give each node its own directory and its own database. |
| Another node's `node-announce` to yours logs `HTTP 403 Forbidden` | The announcing URL is a private or loopback address and `AVALON_ALLOW_PRIVATE_PEERS` is not `true` on the receiving node. |
| A replica logs `mirror-watcher: ... 404 Not Found` for `/ledger/sth/latest?shard_id=core` | The node it mirrors from has no `core` head: it is not a `core` author or has written nothing yet. Point `AVALON_MIRROR_PEERS` at the network's `core` source. |
