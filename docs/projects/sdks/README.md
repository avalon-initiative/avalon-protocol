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

| Language | Folder | Status (2026-09-21) |
|---|---|---|
| Rust (`crates/sdk`) | [`rust/`](rust/README.md) | Reference implementation. Real, live-tested; friends/presence/guilds/conversations work end to end against a live server; achievement issuance still returns `NotImplemented`. |
| C# (`bindings/csharp/AvalonSdk`, netstandard2.1, Unity-targeted) | [`csharp/`](csharp/README.md) | Real, building, tested — 52+ passing tests including opt-in live ones. The priority developer-facing surface, since it's what most integrating game studios will actually use. |

More languages get a subfolder here as they're added, not a new top-level
project.

## Find your door

| I am... | Start here |
|---|---|
| Building a game/app/service and need to pick an SDK | The table above — pick your language |
| Using Rust | [`rust/for-developers/getting-started.md`](rust/for-developers/getting-started.md) |
| Using C#/Unity | [`csharp/README.md`](csharp/README.md) |
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

## Related projects

- [`../backend-server/`](../backend-server/README.md) — what every SDK here talks to.
- [`../cli/`](../cli/README.md) — a separate Rust program built on the Rust
  SDK, but a dev/ops tool, not something you'd embed in a game.
