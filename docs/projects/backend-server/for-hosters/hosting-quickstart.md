# Hosting a node: quick start

The fastest path from a fresh checkout to a running `avalon-server` node —
no Rust or Node toolchain needed, only [Docker](https://docs.docker.com/get-docker/)
(with Compose, bundled with Docker Desktop and modern Docker Engine
installs). Issue [#289](https://github.com/LunarVagabond/avalon-protocol/issues/289),
part of the self-hosting epic [#288](https://github.com/LunarVagabond/avalon-protocol/issues/288).

This is the "get a node running to see it work, or to actually host for your
community" path. If you're contributing code to this repository itself, see
[`../../../maintainers/local-development.md`](../../../maintainers/local-development.md)
instead — that one runs `avalon-server` natively via `cargo` for faster
edit/rebuild cycles.

Running this node makes you part of the Avalon Initiative's actual
infrastructure, not a downstream consumer of someone else's — the network is
only as real and as decentralized as the operators actually running it.

## One command

```bash
git clone https://github.com/LunarVagabond/avalon-protocol.git
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
   `AVALON_MIRROR_PEERS`, no hand-copying URLs (issue #511). Skipped silently
   (no prompt, no behavior change) if nothing's discoverable or this isn't
   running in a real terminal.
2. **Starts Postgres and Redis** (`docker compose ... up -d postgres redis`)
   and waits for both to report healthy. Redis backs the rate limit/
   concurrency ceiling (issue #545) — it's on by default so a node that's
   later scaled to more than one `avalon-server` replica already has it
   running, rather than silently reverting to per-process limits the
   moment it does; a single-instance node gets no functional difference
   from having it. `make stack-up-no-redis` skips it entirely — no Redis
   container, and `avalon-server` runs with its original, purely
   per-process limits, same as before #545 existed.
3. **Runs migrations** (`docker compose ... run --rm migrate`) — a one-shot
   container that applies everything under `crates/server/db/migrations/`
   and exits.
4. **Starts `avalon-server`**, built from the repo's `Dockerfile`, listening
   on `127.0.0.1:8080` on the host.

Re-running `make stack-up` against an already-running stack is safe — it
skips `.env` generation once one exists, and `docker compose up -d` is
itself a no-op (or a clean restart, if the image changed) against containers
that are already running.

```bash
make stack-logs   # follow avalon-server's logs
make stack-down   # stop everything this started
```

## What you get, and what you don't

- A single node running every role together (Settlement, Indexer, Realtime,
  Gateway) — see [`../architecture/nodes.md`](../architecture/nodes.md) for
  what that means today versus the target multi-role topology.
- **Plain HTTP, bound to `127.0.0.1` only.** Fine for trying this out or for
  a node that only ever talks to other processes on the same machine.
  **Before this is reachable from anywhere else — another machine, the
  public internet — see [`deployment.md`](deployment.md)**: TLS is a hard
  requirement the moment this leaves loopback, and that doc covers exposing
  the port behind a reverse proxy correctly.
- A single-node deployment, not a mirrored/multi-Settlement-node network —
  see [`../architecture/settlement.md`](../architecture/settlement.md) if
  you're looking to run alongside other operators on the same network.

## Node roles at a glance

Three independent choices, not one — see
[`../architecture/nodes.md`](../architecture/nodes.md)'s "A node's three
configuration axes are independent" section for the full reference. This
`make stack-up` quickstart leaves all three at their defaults (below);
change them by adding the corresponding line to `.env`.

| Axis | Default here | Env var | Other value(s) |
|---|---|---|---|
| Capability | Combined (everything in one process) | `AVALON_NODE_ROLES` | e.g. `settlement,indexer` for a specialized deployment |
| Shard role | Authority for `core` (this node authors what it's configured to) | `AVALON_SETTLEMENT_SIGNING_KEY` (authority) / `AVALON_MIRROR_PEERS` (mirror) | A node can be an authority for one shard and a mirror of another at once |
| Retention tier | Full/archive (keeps everything, never prunes) | `AVALON_RETENTION_TIER` | `hot` + `AVALON_RETENTION_HOT_WINDOW_DAYS` for a bounded local window |

## Generated `.env` values are yours to keep

`AVALON_SETTLEMENT_SIGNING_KEY` and `AVALON_NETWORK_ID` are generated once,
into `.env`, and never regenerated on a later `make stack-up` as long as that
file still exists. Back it up somewhere safe — losing the signing key means
losing the ability to extend this node's ledger under its existing history.
Never commit `.env` or share the signing key value. Need to replace this key
later — routine hygiene or a suspected compromise? See
[`../for-maintainers/key-rotation.md`](../for-maintainers/key-rotation.md).

Plain `.env` storage is the accepted floor for a single-operator deployment
at this project's current scale — see
[issue #352](https://github.com/LunarVagabond/avalon-protocol/issues/352)
for the reasoning. A local, filesystem-level `.env` is a weaker guarantee
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

## Today in the repo

- `Dockerfile` — multi-stage build producing `avalon-server` and its
  `migrate` companion binary. Not yet layer-cached for fast incremental
  rebuilds (no `cargo-chef`); a first correct build, not an optimized one.
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
- `docker-compose.redis.yml` — issue #545's Redis-backed rate limit/
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

## Decisions and tickets

- [#289](https://github.com/LunarVagabond/avalon-protocol/issues/289) — this
  ticket; implements the bring-up path described above.
- [#288](https://github.com/LunarVagabond/avalon-protocol/issues/288) — the
  epic this is the first concrete step under. Resource-limit defaults
  (`AVALON_*` tuning) are explicitly out of scope here — see
  [#287](https://github.com/LunarVagabond/avalon-protocol/issues/287).
- [`deployment.md`](deployment.md) — required reading before exposing this
  node beyond `127.0.0.1`.
