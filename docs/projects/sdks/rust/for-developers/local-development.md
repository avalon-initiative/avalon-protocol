# Local Development

Running the milestone-1 vertical slice end to end against a real local
server, no game/app/service client required — `avalon-cli` plus `make`.

## 1. Bring up the server

```bash
make migrate   # apply pending db/migrations/
make start     # run avalon-server in the background
```

Needs `.env` at the repo root with `DATABASE_URL` (a real Postgres) and
`AVALON_SERVER_ADDR` — see `.env.example` and the repo root `README.md`.

## 2. Create and log in an identity

```bash
make create-identity
make login IDENTITY_ID=<uuid>
```

`create-identity` drives a real (software-only, no hardware) WebAuthn
registration ceremony and saves the resulting passkey/signing-key locally
under `_running/keys/`. `login` drives a real authentication ceremony
against that saved passkey and prints a live bearer session token — this is
exactly the kind of token `AvalonClient::authenticate()` expects; see
[`getting-started.md`](getting-started.md).

`avalon pair-device` (`make` has no wrapper target yet) drives the
cross-device pairing flow instead, standing in for a WebAuthn-incapable
client (a console, a headless game engine).

## 3. Register an integrator

```bash
make register-integrator SLUG=my-game NAME="My Game" OWNER="My Studio"
```

Prints a private Ed25519 signing key exactly once (the server only ever
stores the public half) and saves it, plus the credential's `key_id`,
under `_running/keys/integrator-<slug>.*` for later commands to reuse.

## 4. Define and issue an achievement

Defining isn't a CLI command yet (see [`achievements.md`](achievements.md)
for the raw request shape); issuing is:

```bash
make issue-achievement INTEGRATOR=my-game ACHIEVEMENT=dragon_slayer TOKEN=<session-token>
```

Issues through `avalon-sdk` exactly the way a real integration would
(`Session::issue_achievement`) — this is a real, useful way to sanity-check
your achievement definition/consent setup before writing any of your own
integration code.

## 5. Inspect the ledger

```bash
make inspect-ledger        # summary view
make inspect-ledger-full   # + each entry's actual payload
make outbox-status         # pending/oldest-pending count for the settlement outbox
```

## Everything in one table

| Command | What it does |
| --- | --- |
| `make create-identity` | Register a new self-custodied (passkey) identity |
| `make login IDENTITY_ID=<uuid>` | Log in, print a session token |
| `make register-integrator SLUG=… NAME=… OWNER=…` | Register a test integrator, save its key |
| `make issue-achievement INTEGRATOR=… ACHIEVEMENT=… TOKEN=…` | Issue an already-defined achievement |
| `make inspect-ledger[-full]` | Pretty-print the hash-chained ledger |
| `make outbox-status` | Settlement outbox diagnostics |

`avalon-cli`'s mutating commands (everything above except the ledger/outbox
diagnostics) only exist in the default `dev-tools` build — a deployment
build (`cargo build -p avalon-cli --no-default-features`) strips them out
entirely, not just at runtime. See `crates/cli/src/dev_tools.rs`'s own doc
comment.
