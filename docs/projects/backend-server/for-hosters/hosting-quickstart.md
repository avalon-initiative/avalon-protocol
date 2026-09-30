# Hosting a node: quick start

The fastest path from a fresh checkout to a running `avalon-server` node —
no Rust or Node toolchain needed, only [Docker](https://docs.docker.com/get-docker/)
(with Compose, bundled with Docker Desktop and modern Docker Engine
installs).

To run the binary directly instead of Docker, start with `avalon setup`, the
guided first-run flow described in [`standalone-binary.md`](standalone-binary.md)
(this guide is also embedded in the CLI: `avalon guide quickstart`).

This is the "get a node running to see it work, or to actually host for your
community" path. If you're contributing code to this repository itself, see
[`../../../maintainers/local-development.md`](../../../maintainers/local-development.md)
instead — that one runs `avalon-server` natively via `cargo` for faster
edit/rebuild cycles.

Running this node makes you part of the Avalon Initiative's actual
infrastructure, not a downstream consumer of someone else's — the network is
only as real and as decentralized as the operators actually running it.

No release has been published yet, so the image `make stack-up` pulls does not
exist (`docker pull` answers `denied`). Until the first release, use
`make stack-up-source`, which builds `avalon-server` from the checkout; this page's
image steps have not been run against a published image.

## One command

```bash
git clone https://github.com/avalon-initiative/avalon-protocol.git
cd avalon-protocol
make stack-up
```

That's it for a first-time, single-node bring-up. What it does:

1. **Generates `.env`** from `.env.compose.example` if none exists yet —
   filling in the two values that genuinely can't have a shared default:
   a fresh `AVALON_SETTLEMENT_SIGNING_KEY` (required; there is no fallback —
   this is the key your node signs its ledger's tree heads with) and a
   `AVALON_NETWORK_ID` unique to this deployment. Every other value gets a
   safe, working default for a single local node. The generated `.env` is
   `chmod`ed to owner read/write only (`600`) — not world-readable at
   whatever the shell's umask happened to leave it. If `AVALON_BOOTSTRAP_PEERS`
   (or a network's bundled seed nodes) resolves to a reachable peer on this
   first run, you're prompted interactively to pick which discovered peer(s),
   if any, to mirror — the selection is written straight into
   `AVALON_MIRROR_PEERS`, no hand-copying URLs. Skipped silently
   (no prompt, no behavior change) if nothing's discoverable or this isn't
   running in a real terminal.
2. **Starts Postgres and Redis** (`docker compose ... up -d postgres redis`)
   and waits for both to report healthy. Redis backs the rate limit/
   concurrency ceiling — it's on by default so a node that's
   later scaled to more than one `avalon-server` replica already has it
   running, rather than silently reverting to per-process limits the
   moment it does; a single-instance node gets no functional difference
   from having it. `make stack-up-no-redis` skips it entirely — no Redis
   container, and `avalon-server` runs with its original, purely
   per-process limits.
3. **Runs migrations** (`docker compose ... run --rm migrate`) — a one-shot
   container that applies the migration set embedded in the binary
   (sourced from `crates/server/db/migrations/`) and exits.
4. **Starts `avalon-server`**, pulling the release image published by each
   tagged release (`ghcr.io/avalon-initiative/avalon-protocol`), listening on
   `127.0.0.1:8080` on the host.

Re-running `make stack-up` against an already-running stack is safe — it
skips `.env` generation once one exists, and `docker compose up -d` is
itself a no-op (or a clean restart, if the image changed) against containers
that are already running.

```bash
make stack-logs   # follow avalon-server's logs
make stack-down   # stop everything this started
```

## Pulling and running the image directly

`make stack-up` is the recommended path (it also brings up Postgres and
runs migrations), but the image itself is a normal container and works with
plain `docker run` against a Postgres you already have:

```bash
docker pull ghcr.io/avalon-initiative/avalon-protocol:v<version>
docker run -d --name avalon-server -p 127.0.0.1:8080:8080 \
  -e DATABASE_URL=postgres://avalon:...@db-host:5432/avalon \
  -e AVALON_NETWORK_ID=avalon-dev-local \
  -e AVALON_WEBAUTHN_RP_ID=localhost \
  -e AVALON_WEBAUTHN_ORIGIN=http://localhost:8080 \
  ghcr.io/avalon-initiative/avalon-protocol:v<version>
```

Replace `v<version>` with the release you're on — see the
[Releases page](https://github.com/avalon-initiative/avalon-protocol/releases).
`docker-compose.yml`'s `avalon-server` service reads the same tag from
`AVALON_IMAGE_TAG` (default `latest`); set it in `.env` to pin a specific
release instead of always tracking the newest one. Migrations are applied by
`avalon-server` itself on every start (see
[`standalone-binary.md`](standalone-binary.md#upgrading)), so there's nothing
extra to run first.

Before running it, verify the image the same way you'd verify a downloaded
tarball — see [`verifying-a-release.md`](verifying-a-release.md#provenance)
for the recipe, using the image reference in place of a tarball path.

Building `avalon-server` from source instead of pulling this image is still
supported: layer `docker-compose.source.yml` on top of `docker-compose.yml`
(see that file's own comment), or use `docker build .` directly with the
repository's root `Dockerfile`.

## What you get, and what you don't

- A single node running every role together (Settlement, Indexer, Realtime,
  Gateway) — see [`avalon-docs: architecture/nodes/README.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/architecture/nodes/README.md) for
  what that means today versus the target multi-role topology.
- **Plain HTTP, bound to `127.0.0.1` only.** Fine for trying this out or for
  a node that only ever talks to other processes on the same machine.
  **Before this is reachable from anywhere else — another machine, the
  public internet — see [`deployment.md`](deployment.md)**: TLS is a hard
  requirement the moment this leaves loopback, and that doc covers exposing
  the port behind a reverse proxy correctly.
- A single-node deployment, not a mirrored/multi-Settlement-node network —
  see [`avalon-docs: architecture/settlement.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/architecture/settlement.md) if
  you're looking to run alongside other operators on the same network.

## Node roles at a glance

Three independent choices, not one — see
[`avalon-docs: architecture/nodes/README.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/architecture/nodes/README.md)'s "A node's three
configuration axes are independent" section for the full reference. This
`make stack-up` quickstart leaves all three at their defaults (below);
change them by adding the corresponding line to `.env`.

| Axis | Default here | Env var | Other value(s) |
|---|---|---|---|
| Capability | Combined (everything in one process) | `AVALON_NODE_ROLES` | e.g. `settlement,indexer` for a specialized deployment |
| Shard role | Authority for `core` (this node authors what it's configured to) | `AVALON_SETTLEMENT_SIGNING_KEY` (authority) / `AVALON_MIRROR_PEERS` (mirror) | A node can be an authority for one shard and a mirror of another at once |
| Retention tier | Full/archive (keeps everything, never prunes) | `AVALON_RETENTION_TIER` | `hot` + `AVALON_RETENTION_HOT_WINDOW_DAYS` for a bounded local window |

## Keys generated on first start

A node started with none of its keys configured generates them itself: the
settlement signing key, the settlement submit key, the witness signing key
and the libp2p identity. They are written to `keys/` inside `AVALON_DATA_DIR`
(default `./data`; the directory is mode `0700`, each file `0600`) and reused
on every later start. A key file that exists is never replaced; a malformed
one stops the node from starting. A variable set in the environment
(`AVALON_SETTLEMENT_SIGNING_KEY`, `AVALON_SETTLEMENT_SUBMIT_KEY`,
`AVALON_WITNESS_SIGNING_KEY`, `AVALON_LIBP2P_IDENTITY_KEY`) always wins and
nothing is written for it.

Two nodes must never share a data directory: they would load the same keys, and
two nodes with one libp2p identity cannot see each other. Give every node on a
machine its own `AVALON_DATA_DIR`.

When `AVALON_OWN_SHARD_ID` is unset and no remote authority is configured
(`AVALON_SETTLEMENT_REMOTE_URL(S)`), a node with a generated signing key authors
its own self-certifying shard, `node:<sha256 of its public key>`. That id
needs no registration and no other node online. Back up `keys/` together with
the database: the signing key is what lets the node extend its ledger.

## Generated `.env` values are yours to keep

`AVALON_SETTLEMENT_SIGNING_KEY` and `AVALON_NETWORK_ID` are generated once,
into `.env`, and never regenerated on a later `make stack-up` as long as that
file still exists. Back it up somewhere safe — losing the signing key means
losing the ability to extend this node's ledger under its existing history.
Never commit `.env` or share the signing key value. Need to replace this key
later — routine hygiene or a suspected compromise? See
[`../for-maintainers/key-rotation.md`](../for-maintainers/key-rotation.md).

Plain `.env` storage is the accepted floor for a single-operator deployment
at this project's current scale. A local, filesystem-level `.env` is a weaker guarantee
than a real secrets store; keep any backup copy of it under the same `600`
permissions `make stack-up` sets, rather than treating it as an ordinary
config file.

## If something goes wrong

`make stack-logs` is the first thing to check — `avalon-server` logs its own
startup failures (a bad `AVALON_SETTLEMENT_SIGNING_KEY` format, a Postgres
connection failure) clearly. `docker compose ps` shows which containers are
actually up. If migrations fail, `make stack-up` again re-runs the one-shot
`migrate` container safely — it only applies whatever hasn't already been
applied.

## Current implementation

- `ghcr.io/avalon-initiative/avalon-protocol` — the published, multi-arch
  (`linux/amd64`, `linux/arm64`) release image, assembled by
  `.github/workflows/release.yml`'s `image`/`image-publish` jobs from the
  same `avalon-server`/`avalon` binaries as that release's tarballs — no
  compiler or source tree in the image, and no separate rebuild that could
  drift from what the tarballs contain. Tagged with the release version and
  `latest`; pushed only from a release tag on `main`. `docker-compose.yml`
  defaults `avalon-server` to it, pinned by `AVALON_IMAGE_TAG`.
- `Dockerfile` — from-source multi-stage build producing `avalon-server`,
  `avalon`, and the `migrate` companion binary. Still used for
  `docker-compose.source.yml`'s from-source override and for `migrate`/
  `discover-mirror-peers`, which aren't part of the published release
  artifact. Not yet layer-cached for fast incremental rebuilds (no
  `cargo-chef`); a first correct build, not an optimized one.
- `Dockerfile.release` — the minimal runtime image the `image` job builds
  from a release tarball's already-built binaries: no Rust toolchain, just
  the two binaries on a slim base.
- `docker-compose.yml` — `postgres` (also usable standalone for the native
  dev flow — see `../../../maintainers/local-development.md`), plus `migrate` and `avalon-server`
  behind a `stack` Compose profile so a plain `docker compose up -d` (the
  existing postgres-only flow) doesn't also try to build/start them.
  `avalon-server`'s published port binds to `AVALON_HOST_IP` (unset
  defaults to `127.0.0.1`, loopback-only, same as the native dev flow) —
  set it to this machine's LAN IP so a peer node elsewhere on the LAN can
  reach it, e.g. for `AVALON_MIRROR_PEERS`/`AVALON_BOOTSTRAP_PEERS` on
  another deployment. Docker refuses to bind a port to an IP not actually
  assigned to a local interface.
- `docker-compose.source.yml` — override that switches `avalon-server` back
  to building from source (the root `Dockerfile`), for testing an
  unreleased change; layer it on with `-f` or use `make stack-up-source`/
  `make stack-up-no-redis-source`.
- `docker-compose.redis.yml` — the Redis-backed rate limit/
  concurrency ceiling, applied as a Compose *override* file (not folded
  into `docker-compose.yml` directly — `avalon-server` hard-fails at
  startup if `AVALON_REDIS_URL` is set but unreachable, so it has to be
  genuinely absent for `make stack-up-no-redis`, not just unused). `make
  stack-up` layers it on automatically; `make stack-up-no-redis` doesn't.
- `make stack-up`/`stack-up-no-redis`/`stack-down`/`stack-logs` (root
  `Makefile`) — the bring-up/teardown/logs commands described above.
- Every other `AVALON_*` value not mentioned here already has a working
  default via `.env.compose.example` — see `.env.example` for the full,
  commented list of what's configurable and why.

See also [`deployment.md`](deployment.md), required reading before exposing
this node beyond `127.0.0.1`.
