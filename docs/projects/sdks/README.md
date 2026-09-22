# sdks

## What this is, in plain language

If [`backend-server`](../backend-server/README.md) is the passport office,
an SDK is the phone line a game or app uses to actually call it — log a
player in, check who their friends are, hand out an achievement. A game
developer doesn't talk to the backend directly; they add the SDK for
whatever language they're building in, and it handles the network calls,
the cryptographic signing, and the retry logic for them. There's one SDK
per language, not one SDK overall — pick the one for your language and
ignore the rest.

## What this is, technically (the meta)

This folder covers every official Avalon SDK, across every language, as one
project rather than one folder per language. They share a design (one
capability/grant model, one error taxonomy, one wire protocol) documented
once in [`architecture/sdk.md`](architecture/sdk.md); each language gets a
thin subfolder for what's actually language-specific.

**Why one folder, not one per language:** SDKs are planned to move to
generated bindings off a shared protobuf/schema definition rather than
hand-written per-language code — at that point "the C# SDK" and "the Rust
SDK" stop being separately maintained projects and become two output
targets of the same generator, likely living in one repo together. Keeping
them as one project here now, rather than splitting language-by-language,
matches where this is actually headed instead of a structure that would
need undoing later.

## Languages

Implemented first, in shipping order:

| Language | Folder | Status (2026-09-21) |
|---|---|---|
| Rust (`avalon-sdks`' `rust/`, moved from `crates/sdk` by #775) | [`rust/`](rust/README.md) | Reference implementation. Real, live-tested; friends/presence/guilds/conversations/achievement issuance all work end to end against a live server, with no dependency on `avalon-protocol` at all (#774) — verified against the same conformance suite the server side asserts. |
| C# (`bindings/csharp/AvalonSdk`, netstandard2.1, Unity-targeted) | [`csharp/`](csharp/README.md) | Real, building, tested — 52+ passing tests including opt-in live ones. The priority developer-facing surface, since it's what most integrating game studios will actually use. |
| TypeScript (`bindings/ts`) | [`typescript/`](typescript/README.md) | Real and shipped (#701), browser-facing — drives a real WebAuthn ceremony. Also what `apps/hub` is actively migrating onto (epic #712), so it's exercised by a real production frontend, not only its own test suite. |

**See [`language-support.md`](language-support.md) for the full, canonical
table** — every language above with its detailed status, plus every
language that doesn't have an SDK yet (Go, Python, C++, Java/Kotlin,
Swift, GDScript, Lua, and more), so "is there an SDK for X?" has a real
answer instead of silence.

More languages get a subfolder here as they're added, not a new top-level
project.

## Find your door

| I am... | Start here |
|---|---|
| Building a game/app/service and need to pick an SDK | The table above — pick your language |
| Using Rust | [`rust/for-developers/getting-started.md`](rust/for-developers/getting-started.md) |
| Using C#/Unity | [`csharp/README.md`](csharp/README.md) |
| Using TypeScript/browser | [`typescript/README.md`](typescript/README.md) |
| Wondering if I should build on Avalon at all | [`rust/for-developers/WhyBuildOnAvalon.md`](rust/for-developers/WhyBuildOnAvalon.md) — language-agnostic pitch, just filed under the Rust guide since that's the only one with a full guide today |
| Looking for the shared design reference — capability grants, error taxonomy, why SDKs are shaped this way, regardless of language | [`architecture/sdk.md`](architecture/sdk.md) |
| Wondering what's actually on the other end of these calls | [`../backend-server/README.md`](../backend-server/README.md) |

## In this folder

- [`architecture/sdk.md`](architecture/sdk.md) — the normative,
  language-agnostic reference: design principles, the capability/grant
  model, error taxonomy, what's intentionally not exposed to any SDK.
- [`rust/`](rust/README.md) — the Rust SDK, reference implementation, with
  a full developer guide.
- [`csharp/`](csharp/README.md) — the C# SDK for Unity/game developers.
- [`typescript/`](typescript/README.md) — the TypeScript SDK for
  browser-facing integrations, and what `apps/hub` is migrating onto.
- [`language-support.md`](language-support.md) — the full language support
  table: every implemented language plus every language without an SDK yet.

## Related projects

- [`../backend-server/`](../backend-server/README.md) — what every SDK here talks to.
- [`../cli/`](../cli/README.md) — a separate Rust program built on the Rust
  SDK, but a dev/ops tool, not something you'd embed in a game.
