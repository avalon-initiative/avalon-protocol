# For Maintainers

Documentation for people maintaining or contributing to this repository
itself — distinct from people building *on* Avalon (see
[developer docs](../projects/sdks/README.md)) and distinct from people hosting a
node without contributing code (see
[`../projects/backend-server/for-hosters/`](../projects/backend-server/for-hosters/README.md)).

This page covers the whole repository. Each project under
[`../projects/`](../projects/README.md) also has its own maintainer-facing
docs for things specific to that project (e.g. rotating the backend's
settlement signing key) — see that project's own README.

## Start here

- [`../../.github/CONTRIBUTING.md`](../../.github/CONTRIBUTING.md) —
  contribution workflow: branching, commit/PR title format, issue claiming,
  the docs-first rule, and the local `make` commands
- [`../projects/README.md`](../projects/README.md) — the project map: what's
  deployable in this repo and how the docs are split around it
- [`../projects/backend-server/architecture/README.md`](../projects/backend-server/architecture/README.md) —
  the normative architecture reference: invariants, authority boundaries,
  the three verticals, what exists in each crate today, and which issue
  governs each area. Read this before proposing or reviewing anything
  non-trivial.
- Root `README.md` — repository layout and current build status
- [`../../.github/CODE_OF_CONDUCT.md`](../../.github/CODE_OF_CONDUCT.md) and
  [`../../.github/SECURITY.md`](../../.github/SECURITY.md)

## How work is tracked

Open work lives in GitHub Issues, claimed via `/claim`. Architecture
decisions that are settled are closed issues labeled
`architecture-decision-record`; questions still being decided are open
issues labeled `decision`. There is no separate backlog file in the repo —
git and the issue tracker are the history.

Ticket bodies (work items under epics) use a fixed structure: Motivation /
Design / Invariants / Affected crates / Tests / Documentation / Acceptance
criteria (checkboxes). Decision tickets use Context / Decision / Why /
Alternatives considered / Consequences / Related. ADR issues use Context /
Decision / Consequences / Related.

## Repo state and process

- **Private for now.** GitHub Actions workflows under `.github/workflows/`
  (claim-issue, claim-check, stale-claim-check, PR title lint, the
  Dependabot security-title bot, the welcome bot) are already committed but
  stay disabled at the repo-settings level until the repo actually goes
  public — see the note in `CONTRIBUTING.md`. GitHub Discussions is off for
  the same reason; use Issues for everything in the meantime.
- **No live database in most sandboxes.** `crates/server` and `crates/chain`
  use runtime-checked `sqlx::query` rather than the compile-time `sqlx::query!`
  macro specifically so `cargo build`/`test`/`clippy` all pass without a
  reachable Postgres. `make test-live` is the real, `--ignored` test suite;
  it starts its own isolated servers against an actual dev database — see the
  root `.env.example`.
- **Merges are squash-only by convention** — the PR title becomes the commit
  message on `main`, so it's the one place the `[#<issue>] - ...` format is
  strictly enforced (branch commit messages aren't).

## Local dev setup

See [`local-development.md`](local-development.md) for the full, ordered
path from a clean clone — Postgres via `docker compose`, `make migrate`,
running the server, creating an identity, and the Hub/Storybook/C# SDK
setup, with what "it worked" looks like at each step.

## Hosting a node

See
[`../projects/backend-server/for-hosters/hosting-quickstart.md`](../projects/backend-server/for-hosters/hosting-quickstart.md)
for the fastest path to a running node — `make stack-up`, no Rust/Node
toolchain needed, just Docker — and
[`../projects/backend-server/for-hosters/deployment.md`](../projects/backend-server/for-hosters/deployment.md)
for putting `avalon-server` behind TLS (required before any non-local
deployment), recommended reverse-proxy setup, example configs, and which
existing env vars need production values. Written for anyone standing up a
node, not just people contributing to this repository — see
[`../projects/backend-server/for-hosters/README.md`](../projects/backend-server/for-hosters/README.md).

If you're running a node that watches peers (`AVALON_MIRROR_PEERS` set) and
its mirror-watcher reports equivocation, see
[`equivocation-response.md`](../projects/backend-server/for-maintainers/equivocation-response.md)
for the investigation and recovery steps. See
[`key-rotation.md`](../projects/backend-server/for-maintainers/key-rotation.md)
for rotating the settlement signing key itself, routine or emergency.

Quick reference once you've read that page:

```bash
docker compose up -d && cp .env.compose.example .env   # Postgres + env
make migrate && make start                              # db + server
```

Run `make help` for the full command list, including the `avalon-cli` dev/ops
commands (`create-identity`, `inspect-ledger`, `outbox-status`, ...).

## Milestone 1 — the end-to-end vertical slice

See
[`milestone-1-walkthrough.md`](../projects/backend-server/for-maintainers/milestone-1-walkthrough.md)
for the hand-run, numbered walkthrough of the full vertical slice (plus two
architecture checks beyond it) — two players, a friendship, a guild with a
channel, two games, an issued and verified achievement, all visible in the
Hub, ending with a ledger inspection and a full projection rebuild.
`crates/cli/tests/milestone_1_walkthrough.rs` is the automated equivalent,
runnable via `make test-live`.

## Related projects

`world_zero` is a related Rust MMO server framework and a candidate
external integration for Avalon, evaluated as an opt-in `AuthProvider`/
account system there.
