# For Maintainers

Documentation for people maintaining or contributing to this repository
itself — distinct from people building *on* Avalon (see
[`../developers/`](../developers/)) and distinct from people hosting a
node without contributing code (see [`../hosters/`](../hosters/)).

## Start here

- [`../../.github/CONTRIBUTING.md`](../../.github/CONTRIBUTING.md) —
  contribution workflow: branching, commit/PR title format, issue claiming,
  the docs-first rule, and the local `make` commands
- [`../architecture/README.md`](../architecture/README.md) — the normative
  architecture reference: invariants, authority boundaries, the three
  verticals, what exists in each crate today, and which issue governs each
  area. Read this before proposing or reviewing anything non-trivial.
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
  reachable Postgres. `make test-live` is the real, `--ignored` test suite
  that needs `make start` running against an actual dev database — see the
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

See [`../hosters/hosting-quickstart.md`](../hosters/hosting-quickstart.md)
for the fastest path to a running node — `make stack-up`, no Rust/Node
toolchain needed, just Docker — and
[`../hosters/deployment.md`](../hosters/deployment.md) for putting
`avalon-server` behind TLS (required before any non-local deployment),
recommended reverse-proxy setup, example configs, and which existing env
vars need production values. Written for anyone standing up a node, not
just people contributing to this repository — see
[`../hosters/README.md`](../hosters/README.md).

If you're running a node that watches peers (`AVALON_MIRROR_PEERS` set) and
its mirror-watcher reports equivocation, see
[`equivocation-response.md`](equivocation-response.md) for the
investigation and recovery steps. See
[`key-rotation.md`](key-rotation.md) for rotating the settlement signing
key itself, routine or emergency.

Quick reference once you've read that page:

```bash
docker compose up -d && cp .env.compose.example .env   # Postgres + env
make migrate && make start                              # db + server
make web-install                                        # JS workspace, once
```

Run `make help` for the full command list, including the C# SDK
(`bindings/csharp`) build/test targets and the `avalon-cli` dev/ops commands
(`create-identity`, `inspect-ledger`, `outbox-status`, ...).

## Related projects

`world_zero` is a related Rust MMO server framework and the first candidate
external integration for Avalon (see WorldZero issue #309 — an opt-in
`AuthProvider`/account system there).
