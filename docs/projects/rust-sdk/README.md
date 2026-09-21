# rust-sdk

## What this is, in plain language

If [`backend-server`](../backend-server/README.md) is the passport office,
this is the phone line a game or app uses to actually call it — log a
player in, check who their friends are, hand out an achievement. A game
developer writing in Rust doesn't talk to the backend directly; they add
this crate, and it handles the network calls, the cryptographic signing,
and the retry logic for them.

## What this is, technically (the meta)

`crates/sdk` — the Rust reference SDK, and the first of two SDKs (alongside
[`../csharp-sdk/`](../csharp-sdk/README.md)) that exist to let a game, app,
or service talk to `backend-server` without reimplementing its protocol,
auth, or wire format. "Reference" means: it's the one built alongside the
server itself, so it's the most complete and the one other SDKs are checked
against.

**Design principle:** the SDK exposes protocol *capabilities*, not
infrastructure *topology* — a caller authenticates and calls methods; it
never needs to know whether it's talking to a combined node or a
role-specialized one, or how many Postgres instances sit behind it. See
[`architecture/sdk.md`](architecture/sdk.md) for the full design reference.

**Status (2026-09-21):** real, not stubbed. `authenticate()` is wired to a
live server; friends/presence, guilds (roster/channels/chat), and
conversations work end to end; `sync_journal`/`submission` implement
offline durability and deferred submission. Achievement issuance still
returns `NotImplemented` — the one known gap in an otherwise live surface.

## Find your door

| I am... | Start here |
|---|---|
| Building a game/app/service in Rust that talks to Avalon | [`for-developers/getting-started.md`](for-developers/getting-started.md) — ten minutes to a working call |
| Wondering if I should build on Avalon at all | [`for-developers/WhyBuildOnAvalon.md`](for-developers/WhyBuildOnAvalon.md) |
| Looking for the design reference — what a capability grant is, error taxonomy, why the SDK is shaped this way | [`architecture/sdk.md`](architecture/sdk.md) |
| Using C#/Unity instead of Rust | [`../csharp-sdk/README.md`](../csharp-sdk/README.md) |
| Wondering what's actually on the other end of these calls | [`../backend-server/README.md`](../backend-server/README.md) |

## In this folder

- [`architecture/sdk.md`](architecture/sdk.md) — the normative reference:
  design principles, the capability/grant model, error taxonomy, what's
  intentionally not exposed.
- [`for-developers/`](for-developers/README.md) — the integration guide:
  getting started, capabilities, achievements, guilds and friends, errors
  and retries, and running the whole stack locally with no game client.

## Related projects

- [`../backend-server/`](../backend-server/README.md) — what this SDK talks to.
- [`../csharp-sdk/`](../csharp-sdk/README.md) — the other SDK, for Unity/C#
  developers; mirrors most of this surface but doesn't have its own
  parallel developer guide yet.
- [`../cli/`](../cli/README.md) — a separate Rust program that talks to the
  same backend, but is a dev/ops tool, not something you'd embed in a game.
