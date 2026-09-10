# Local development setup

Everything needed to get the full stack — server, CLI, Hub web client,
Storybook, and the C# SDK — running from a clean clone. This page is
operational only; for what the code actually does and why, see
[`../architecture/`](../architecture/) (linked from each section below, not
repeated here).

## Prerequisites

- **Rust** (stable toolchain; no `rust-toolchain.toml` pins a specific
  version, workspace edition is 2021) — `cargo`, `rustc`.
- **Docker** with the `docker compose` CLI plugin — used only to run
  Postgres locally; the app itself runs natively via `make start`.
- **Node.js and npm** for `apps/hub`, `apps/mobile-hub`, `packages/ui` (npm
  workspaces from the repo root). No version is pinned in `package.json`;
  a current LTS Node works.
- **.NET SDK** for `bindings/csharp` (targets `netstandard2.1` for Unity
  compatibility — any modern .NET SDK that can build a netstandard2.1
  library works).

None of these are enforced by a version-lock file in the repo today; if you
hit a version-specific build failure, it's worth ticketing so a pin can be
added.

## 1. Postgres

```bash
docker compose up -d
```

Starts a single `postgres` container (see `docker-compose.yml`) with the
`avalon` role, password, and database already created via the image's
standard `POSTGRES_USER`/`POSTGRES_PASSWORD`/`POSTGRES_DB` env vars, exposed
on the default port `5432`, backed by a named volume so data survives a
restart.

**It worked if:** `docker compose ps` shows `avalon-postgres` as `healthy`.

## 2. Environment

```bash
cp .env.compose.example .env
```

`.env.compose.example` is pre-filled with values that match
`docker-compose.yml` exactly (`DATABASE_URL=postgres://avalon:avalon@localhost:5432/avalon`,
plus the WebAuthn/network settings needed to boot) — no hand-editing needed
for this path. `.env.example` is the fuller, commented reference for every
variable the server reads (settlement signing key, recovery delay, retention
tier, ...) if you need to point at a different Postgres or change a default.

## 3. Migrate

```bash
make migrate
```

Applies every migration under `crates/server/db/migrations/` in order.

**It worked if:** the command exits 0 with no error; there's no separate
"list migrations" target, but `make db-reset` (drops and recreates the
schema, then reapplies everything — see below) is the fastest way to confirm
the full set applies cleanly from empty.

## 4. Start the server

```bash
make start
make status
```

`make start` runs `avalon-server` in the background; pid and logs live under
`_running/` (`_running/pids/avalon-server.pid`, `_running/logs/avalon-server.log`
— tail the log file if something doesn't come up). `make status` reports
whether it's running. `make stop` stops it, `make restart` does both.

**It worked if:** `make status` prints `running (pid ...)` and
`_running/logs/avalon-server.log` shows the server bound to
`AVALON_SERVER_ADDR` (`127.0.0.1:8080` by default) with no error.

## 5. Create an identity and inspect the ledger

```bash
make create-identity
make inspect-ledger
```

`make create-identity` registers a new self-custodied identity via
`avalon-cli` (a WebAuthn passkey ceremony plus its own Ed25519 signing key —
see [`../architecture/identity.md`](../architecture/identity.md)).
`make inspect-ledger` pretty-prints the hash-chained settlement ledger
(`make inspect-ledger-full` includes each entry's payload).

**It worked if:** `make inspect-ledger` shows an `identity.created` entry for
the identity you just created, with `issuer = identity:<id>:self:created`.
`make outbox-status` should show zero pending entries once the outbox worker
has caught up — identity creation and its ledger entry are committed
atomically via an outbox pattern (see
[`../architecture/settlement.md`](../architecture/settlement.md)).

To log back in with an identity `create-identity` already saved locally:

```bash
make login IDENTITY_ID=<uuid>
```

## 6. Web workspace (Hub, mobile-hub, shared UI)

```bash
make web-install    # npm install at the workspace root, once
make hub-dev         # Vite dev server for apps/hub
make storybook       # Storybook for packages/ui
```

**It worked if:** `make hub-dev` prints a local Vite URL (default
`http://localhost:5173`) that loads the Hub in a browser, and — with the
server from step 4 running — the identity created in step 5 can log in
through it. `make storybook` prints a local Storybook URL serving
`packages/ui`'s component stories.

`make mobile-dev` (Tauri dev build of `apps/mobile-hub`) needs Tauri's own
native prerequisites beyond Node — see
[Tauri's prerequisites guide](https://tauri.app/start/prerequisites/) if you
need that app specifically; it's not required for the Hub web client.

## 7. C# SDK

```bash
make csharp-build
make csharp-test
```

Builds/tests `bindings/csharp/AvalonSdk.sln` (the `AvalonSdk` library plus
its `AvalonSdk.Tests` project) via `dotnet build`/`dotnet test`.

**It worked if:** both commands exit 0.

## Everything at once

```bash
make check-all
```

Runs the Rust `check` target (fmt-check + lint + test), `web-lint`,
`web-test`, `csharp-build`, and `csharp-test` — the closest single local
approximation of what CI runs (`make check` alone is just the Rust part).

## Resetting and live tests

```bash
make db-reset     # drop + recreate the public schema, reapply all migrations
make test-live    # cargo test --workspace -- --ignored, needs `make start` running
```

`make test-live` runs the tests that are `#[ignore]`d by default because
they need a real server and database — a real WebAuthn ceremony over HTTP,
not a mock. Run `make start` first.

## Tearing down

```bash
make stop            # stop avalon-server
docker compose down   # stop and remove the Postgres container (add -v to also drop the volume/data)
```

## Logs and run state

Everything `make start` produces lives under `_running/` at the repo root
(gitignored): `_running/pids/avalon-server.pid` and
`_running/logs/avalon-server.log`. `make clean` removes it along with Rust
build artifacts; `make clean-all` also removes `node_modules`, JS build
output, and the C# `bin`/`obj` directories.
