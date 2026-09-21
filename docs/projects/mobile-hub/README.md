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
`protocol`/`chain`/`indexer`/`server`/`sdk`/`cli`.

**Status (2026-09-21):** scaffolding/skeleton, not yet built out. This page
will grow real content once there's real behavior to document — see
[`../hub/README.md`](../hub/README.md) in the meantime, since the UI it
wraps is the same UI.

## Related projects

- [`../hub/`](../hub/README.md) — the web client this wraps; same UI, same
  backend calls, different shell.
- [`../ui/`](../ui/README.md) — the shared component library both Hub apps
  are built from.
- [`../backend-server/`](../backend-server/README.md) — what this talks to,
  same as the Hub, with no backend of its own.
