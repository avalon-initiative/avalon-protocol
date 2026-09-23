# mobile-hub

## What this is, in plain language

The same Hub — your friends, guilds, achievements, and identity — but as an
app on your phone or desktop instead of a website, so you can check in on
your guild or see if a friend is online without having a game open at all.

## What this is, technically (the meta)

`apps/mobile-hub` — a [Tauri](https://tauri.app/) shell (desktop + mobile)
around the same UI as [`../hub/`](../hub/README.md), for guild/friend
presence without a game client open. Its `src-tauri/` is intentionally its
own standalone Cargo package, **not** a member of the root Rust workspace —
it doesn't share compilation or dependency resolution with
`protocol`/`chain`/`indexer`/`server`/`sdk`/`cli`. `src-tauri/src/main.rs`
is a thin entry point only (required by Tauri's build); real logic lives in
`src-tauri/src/lib.rs` so it stays testable outside a bundled app context.

**Status:** further along than pure scaffolding, but a small slice of the
Hub, not a parity build. `src/router/` is a small, standalone router —
mirrors `apps/hub`'s auth-screen shape but doesn't carry its full route
tree. What exists today: `Login`, `CreateIdentity`, `CrossNodeLogin`,
`Home`, `Settings`, behind an `AuthLayout`. Guild/friends/chat views are
separate, later work — not built yet. `Settings` is deliberately reachable
whether logged in or not, since a fresh install needs to be able to point
at a non-default server before an identity even exists.

## In this folder

Nothing yet beyond this overview — once guild/friends/chat views land,
this project's docs will grow the same `for-users.md`
[`../hub/for-users.md`](../hub/for-users.md) already has, adapted for what's
actually different about the mobile/desktop shell (e.g. background presence,
notifications) rather than duplicating Hub content that's identical here.

## Related projects

- [`../hub/`](../hub/README.md) — the web client this wraps; same UI
  patterns, same backend calls, different shell, currently well ahead of
  this project in feature coverage.
- [`../ui/`](../ui/README.md) — the shared component library both Hub apps
  are built from.
- [`../backend-server/`](../backend-server/README.md) — what this talks to,
  same as the Hub, with no backend of its own.
