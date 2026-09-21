# cli

## What this is, in plain language

A toolbox for the people building Avalon itself, not for players or for the
games that integrate it. It's how someone working on this repository creates
a test identity, logs in, registers a test game, or looks inside the ledger
to check that something actually got recorded correctly — from a terminal,
without opening a browser or writing a throwaway script.

## What this is, technically (the meta)

`crates/cli` — the `avalon` binary. Local dev/ops tooling: it may know about
dev/ops workflows (registration, inspection, diagnostics, migrations); it
must not become a second server. It's a client of `backend-server` like any
other, just built for people, not games.

**Status (2026-09-21):** real, used day to day in this repo's own
development workflow. Commands include `avalon create-identity`, `login`,
`register-integrator`, `inspect-ledger` / `inspect-ledger-full`,
`outbox-status`, `prune-ledger`. `crates/cli/tests/milestone_1_walkthrough.rs`
uses it to drive the automated version of the full milestone-1 vertical
slice.

## Find your door

| I am... | Start here |
|---|---|
| A contributor to this repository wanting to exercise the network locally | [`../backend-server/for-hosters/hosting-quickstart.md`](../backend-server/for-hosters/hosting-quickstart.md) to get a node running, then `avalon --help` |
| A maintainer running the milestone-1 walkthrough by hand | [`../backend-server/for-maintainers/milestone-1-walkthrough.md`](../backend-server/for-maintainers/milestone-1-walkthrough.md) |
| Looking for the equivalent tool as a library instead of a binary | [`../rust-sdk/README.md`](../rust-sdk/README.md) |

## Related projects

- [`../backend-server/`](../backend-server/README.md) — what this CLI talks to.
- [`../rust-sdk/`](../rust-sdk/README.md) — the library this CLI is built on
  top of, for the same network calls embedded in a program instead of run
  from a terminal.
