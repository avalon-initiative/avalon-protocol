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
must not become a second server. It's a client of
[`backend-server`](../backend-server/README.md) like any other, just built
for people, not games.

**Two build modes.** Some commands are always available, safe to run
against a real deployment (read-only diagnostics, or operator actions gated
by their own confirmation). Others exist only when the binary is built with
the default `dev-tools` Cargo feature — building with
`--no-default-features` doesn't just refuse those commands at runtime, it
removes them and every dependency only they need from the binary entirely.

**Status:** real, used day to day in this repo's own development workflow.
`crates/cli/tests/milestone_1_walkthrough.rs` uses it to drive the
automated version of the full milestone-1 vertical slice.

## Commands

Always available:

| Command | What it does |
|---|---|
| `avalon inspect-ledger` / `inspect-ledger-full` | Read-only ledger view; `-full` also shows each entry's payload. Safe against a real deployment. |
| `avalon outbox-status` | Diagnostics for the write/settlement outbox pattern identity/guild/achievement writes use to stay atomic with their ledger entry. |
| `avalon prune-ledger [--dry-run]` | The operator-facing entry point for node-tiered retention pruning — reads `AVALON_RETENTION_*` from the environment and reports or executes exactly what that config says. |
| `avalon rebuild-index` | Rebuilds the indexer's projections from durable ledger history — the practical proof that query state is genuinely reconstructable. |
| `avalon migrate-network --target-database-url <url> --target-network-id <id>` | Migrates ledger data toward a new network deployment. |
| `avalon discover-mirror-peers` | Peer discovery for mirror-watching nodes. |
| `avalon check-switch-readiness <old-host-url> <new-host-url> [--shard-id <id>] [--verify-key <hex>]` | Checks whether it's safe to switch a mirror/client over to a new host. |
| `avalon verify-mirror-convergence <network_id> [--shard-id <id>] [--source <url>]` | Offline check that a mirror's copy of a shard equals the authority's history (recomputed root vs. an observed STH, hash chain, open equivocations); exits non-zero unless converged. |
| `avalon promote-mirror <network_id> [--shard-id <id>] [--source <url>] --target-database-url <url> [--dry-run]` | Seeds a fresh, migrated database with this mirror's converged copy of a shard (entries with their original `seq`, batch records, matching Signed Tree Heads) so a replacement settlement authority can continue the hash chain. Refuses unless converged; one transaction; re-verifies after commit. `--dry-run` writes nothing. |
| `avalon list-equivocations [network_id]` | Lists recorded equivocation events (conflicting Signed Tree Heads) for a network. |
| `avalon resolve-equivocation <network_id> <tree_size> <legitimate_root_hash> [--shard-id <id>] [--discard-mirrored]` | The operator action that resolves a detected equivocation — see [`../backend-server/for-maintainers/equivocation-response.md`](../backend-server/for-maintainers/equivocation-response.md). |
| `avalon logs export [<file>] [--file <path>] [--tail <n>] [--since <rfc3339-timestamp>]` | Reads `avalon-server`'s own log file, strips ANSI codes, redacts known-sensitive values, and normalizes to line-delimited JSON — safe to attach when filing a bug report. |

Only in `dev-tools` builds (the default):

| Command | What it does |
|---|---|
| `avalon create-identity` | Creates a test identity, no real WebAuthn ceremony needed. |
| `avalon login <identity_id>` | Logs in as an existing identity. |
| `avalon pair-device` | Drives the `start`/`poll` side of cross-device pairing — stands in for a real WebAuthn-incapable client so that flow is testable without a real console/engine. |
| `avalon register-integrator --slug <slug> --name <name> --owner-name <owner> [--capability <cap>]... [--server <url>]` | Registers a test integrator (game/app/service). `register-game` is a working deprecated alias for the same command. |
| `avalon issue-achievement --integrator <slug> --achievement <key> --token <session-token> [--key <path>] [--key-id <uuid>] [--server <url>]` | Issues an already-defined achievement to the identity behind `--token`, through the Rust SDK, resolving the issuer's signing key/key id from what `register-integrator` saved unless overridden. |
| `avalon register-issuer --integrator <slug> (--network-id <network_id> \| --env <dev\|int\|mainnet>) [--issuer-ref <ref>] [--key <path>] [--server <url>]` | Registers an integrator's key as an issuer on an explicitly declared target network — refuses client-side on a mismatch against the server's independently STH-verified network rather than trusting its bare `network_id`. |

Run `avalon` with no arguments (or an unrecognized one) for the exact
current usage string, which is generated from the same source as this
table and will stay more current if the two ever drift.

## Find your door

| I am... | Start here |
|---|---|
| A contributor to this repository wanting to exercise the network locally | [`../backend-server/for-hosters/hosting-quickstart.md`](../backend-server/for-hosters/hosting-quickstart.md) to get a node running, then the command table above |
| A maintainer running the milestone-1 walkthrough by hand | [`../backend-server/for-maintainers/milestone-1-walkthrough.md`](../backend-server/for-maintainers/milestone-1-walkthrough.md) |
| Responding to a mirror equivocation report | `avalon list-equivocations` / `resolve-equivocation`, and [`../backend-server/for-maintainers/equivocation-response.md`](../backend-server/for-maintainers/equivocation-response.md) |
| Looking for the equivalent tool as a library instead of a binary | [`../sdks/rust/README.md`](../sdks/rust/README.md) |

## Related projects

- [`../backend-server/`](../backend-server/README.md) — what this CLI talks to.
- [`../sdks/rust/`](../sdks/rust/README.md) — the library this CLI is built
  on top of (`issue-achievement` goes through it directly), for the same
  network calls embedded in a program instead of run from a terminal.
