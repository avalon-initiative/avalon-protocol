# Hosting a node: quick start

The fastest path from a fresh checkout to a running `avalon-server` node —
no Rust or Node toolchain needed, only [Docker](https://docs.docker.com/get-docker/)
(with Compose, bundled with Docker Desktop and modern Docker Engine
installs). Issue [#289](https://github.com/LunarVagabond/avalon-protocol/issues/289),
part of the self-hosting epic [#288](https://github.com/LunarVagabond/avalon-protocol/issues/288).

This is the "get a node running to see it work, or to actually host for your
community" path. If you're contributing code to this repository itself, see
[`local-development.md`](local-development.md) instead — that one runs
`avalon-server` natively via `cargo` for faster edit/rebuild cycles.

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
   safe, working default for a single local node.
2. **Starts Postgres** (`docker compose ... up -d postgres`) and waits for
   it to report healthy.
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

## Generated `.env` values are yours to keep

`AVALON_SETTLEMENT_SIGNING_KEY` and `AVALON_NETWORK_ID` are generated once,
into `.env`, and never regenerated on a later `make stack-up` as long as that
file still exists. Back it up somewhere safe — losing the signing key means
losing the ability to extend this node's ledger under its existing history.
Never commit `.env` or share the signing key value.

Plain `.env` storage is the accepted floor for a single-operator deployment
at this project's current scale — see
[issue #352](https://github.com/LunarVagabond/avalon-protocol/issues/352)
for the reasoning. A local, filesystem-level `.env` is a weaker guarantee
than a real secrets store; restrict its permissions (owner read/write only)
and keep any backup copy under the same restriction rather than treating it
as an ordinary config file.

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
  dev flow — see `local-development.md`), plus `migrate` and `avalon-server`
  behind a `stack` Compose profile so a plain `docker compose up -d` (the
  existing postgres-only flow) doesn't also try to build/start them.
- `make stack-up`/`stack-down`/`stack-logs` (root `Makefile`) — the
  bring-up/teardown/logs commands described above.
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
