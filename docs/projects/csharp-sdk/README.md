# csharp-sdk

## What this is, in plain language

The same phone line as [`rust-sdk`](../rust-sdk/README.md) — the thing a
game or app uses to log a player in, check friends, and issue achievements —
but for game developers working in C#, which is most of them, since this is
the SDK Unity projects use.

## What this is, technically (the meta)

`bindings/csharp/AvalonSdk` — the flagship *external* SDK for game
developers, targeting netstandard2.1 for Unity compatibility. The Rust SDK
([`../rust-sdk/`](../rust-sdk/README.md)) is the reference implementation;
this is the priority developer-facing surface, since it's what most
integrating game studios will actually use.

**Status (2026-09-20, last verified):** real, building, and tested —
`dotnet build` / `dotnet test` both work, 52+ passing tests including
opt-in live ones against a real server and database (see
`AvalonSdk.Tests/LiveTests.cs`'s own header comment for the exact
`DATABASE_URL` format it needs — an Npgsql keyword/value string, not this
repo's own `.env`-style Postgres URI). Not a skeleton — see
[`../rust-sdk/architecture/sdk.md`](../rust-sdk/architecture/sdk.md)'s own
"Today in the repo" section for the real, live-verified surface it covers
alongside the Rust SDK.

**Known scoping gap, not an oversight:** this SDK doesn't port every
account-session construction path the Rust SDK has (e.g. `Register`,
`AccountLogin`, `AddPasskeyAsync`, or the `AccountCredentials` type used to
log back into the same identity on the same device) — called out explicitly
in both `../rust-sdk/architecture/sdk.md` and `AccountSession.cs`'s own
header comment.

## Find your door

| I am... | Start here |
|---|---|
| A Unity/C# game developer wanting to integrate Avalon | Start with the Rust SDK's [`for-developers/getting-started.md`](../rust-sdk/for-developers/getting-started.md) for the concepts — this SDK mirrors most of that surface but doesn't have its own parallel guide yet (tracked, issue #51) |
| Looking for the underlying design reference | [`../rust-sdk/architecture/sdk.md`](../rust-sdk/architecture/sdk.md) — applies to both SDKs |

## Related projects

- [`../rust-sdk/`](../rust-sdk/README.md) — the reference implementation
  this SDK mirrors.
- [`../backend-server/`](../backend-server/README.md) — what this SDK talks to.
