# Projects

Avalon Protocol lives in one repository today, but it is not one program. It's
several independently deployable things that happen to be developed together
for now. This directory has one folder per deployable — each folder is meant
to be **self-contained enough to move to its own repository later without
untangling it from anything else.**

If you're new here: pick the row below that's you, open that folder's
`README.md`, and stop worrying about the other six until you need them.
Cross-links between folders are relative paths (e.g.
`../rust-sdk/architecture/sdk.md`) — if a project's docs do move to their own
repo, those specific links are what will need re-pointing; everything else
inside a project folder stays valid as-is.

## The seven projects

| Project | What it is | Status (2026-09-21) |
|---|---|---|
| [`backend-server/`](backend-server/README.md) | The network: `protocol` + `chain` + `indexer` + `server` — one compiled binary (`avalon-server`) that can run as one combined process or as several role-specialized node processes talking to each other. This is the thing everything else talks to. | Real, live-tested, the most mature project here. |
| [`rust-sdk/`](rust-sdk/README.md) | The reference SDK (`crates/sdk`) — how a Rust program authenticates, reads, and writes against `backend-server`. | Real; friends/presence/guilds/conversations wired to a live server; achievement issuance still returns `NotImplemented`. |
| [`cli/`](cli/README.md) | `avalon`, the local dev/ops binary (`crates/cli`) — identity creation, login, integrator registration, ledger inspection, outbox status, pruning. | Real, used for day-to-day dev workflows in this repo. |
| [`hub/`](hub/README.md) | `apps/hub` — the web client (Vue3 + Vite + TS). A user's first doorway into Avalon with no game or app open: identity setup, friends, guilds, achievements, integrator discovery/connections. | Real, a persistent app with nested routed pages, not a stub — currently mid-migration onto `@avalon/sdk` (issue #712). |
| [`mobile-hub/`](mobile-hub/README.md) | `apps/mobile-hub` — a Tauri (desktop + mobile) shell around the same Hub UI, for guild/friend presence without a game client open. | Scaffolding/skeleton, not yet built out. |
| [`ui/`](ui/README.md) | `packages/ui` (`@avalon/ui`) — the shared Vue3 component library used by both Hub apps, documented in Storybook. | Partial; grows alongside `hub`/`mobile-hub`. |
| [`csharp-sdk/`](csharp-sdk/README.md) | `bindings/csharp/AvalonSdk` — the flagship *external* SDK for game developers, targeting netstandard2.1 for Unity compatibility. | Real, building, tested (52+ passing tests, opt-in live ones included) as of 2026-09-20. |

## Why these boundaries and not others

Each project folder here corresponds to something that ships or could ship on
its own — a binary, an npm package, a published SDK — not to an arbitrary
folder split. `backend-server` bundles four Rust crates together rather than
splitting them one-for-one, because they compile into a single binary today
and are planned to move together if/when this repo splits (`protocol` +
`chain` + `indexer` + `server` as one unit, everything else moving out
separately) — see that project's own README for the detail on why those four
specifically travel together even though their internal roles (settlement,
indexing, realtime, gateway) can now run as separate *processes* of that one
binary.

`rust-sdk` and `cli` are their own folders rather than folded into
`backend-server` because, while they live in the same Cargo workspace today,
neither is part of what `avalon-server` deploys as — an SDK ships inside
someone else's game or app, and the CLI is a standalone dev-ops tool.

## What's cross-cutting and stays at the top level

A few things describe the whole protocol, not any one deployable, and aren't
project-scoped:

- [`../GLOSSARY.md`](../GLOSSARY.md) — shared vocabulary used across every project's docs.
- [`../WhyAvalon.md`](../WhyAvalon.md) — the case for why Avalon needs to exist at all.
- [`../stakeholders/Proposal.md`](../stakeholders/Proposal.md) — the narrative design document for the whole system.
- [`../trusted-networks.json`](../trusted-networks.json) — the pinned-key trust-anchor list, referenced by `backend-server`'s network-trust-anchors doc but not owned by any one project.

## Architecture docs that live inside a project folder but aren't only about that project

Some topics (identity, trust model, achievements, guilds, settlement) are
protocol-wide concepts, but they're documented under
[`backend-server/architecture/`](backend-server/architecture/README.md)
because that's where the code that defines and enforces them lives —
`protocol`, `chain`, `indexer`, `server`. Other projects (`rust-sdk`, `hub`)
consume those concepts and link back to them rather than redefining them.
