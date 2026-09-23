# C# SDK

The `csharp/AvalonSdk` project in the `avalon-sdks` repo (NuGet package id
`Avalon.Sdk`, root namespace `Avalon.Sdk`) — the flagship *external*
[Avalon SDK](../README.md) for game developers. Physically lives in
`avalon-sdks` (`languages/csharp/`) as of issue #775 (epic #771), moved out of this
repo's `bindings/csharp` alongside the Rust SDK's own earlier move —
neither this repo's `Makefile` nor its CI touch it anymore; `dotnet
build`/`dotnet test` run from `avalon-sdks` directly. Targets
`netstandard2.1` deliberately, not `net8+`: Unity's Mono/IL2CPP runtimes
are the primary target, and netstandard2.1 is Unity's minimum supported
C# API compatibility level as of writing.

See [`../architecture/sdk.md`](../architecture/sdk.md) for the design that
applies to every language's SDK — this page is the C#-specific "how," not
the "why."

**Status:** real, building, and tested — `dotnet build` / `dotnet test`
both work, 126+ passing tests including opt-in live ones against a real
server and database (see `AvalonSdk.Tests/LiveTests.cs`'s own header
comment for the exact `DATABASE_URL` format it needs — an Npgsql
keyword/value string, not this repo's own `.env`-style Postgres URI). Not
a skeleton — see [`../architecture/sdk.md`](../architecture/sdk.md) for
the real, live-verified surface it covers alongside the Rust SDK.

## Dependencies, and why

`netstandard2.1` has no built-in JSON, channel, or WebSocket-client types,
so the SDK pulls in the smallest set that covers social/guilds/
conversations/`sync_journal` without a full web framework:

- **`System.Text.Json`** — not a hand-rolled DTO parser, so there's one
  JSON layer for the whole SDK.
- **`System.Threading.Channels`**
- **`BouncyCastle.Cryptography`** — `netstandard2.1` has no built-in
  Ed25519, and this is a pure-managed implementation (no native binary),
  which matters for Unity/IL2CPP targets that can't easily bundle a
  per-platform native library the way something like
  `NSec.Cryptography`/libsodium would need. Used by `Achievements.cs` for
  signing issuance.

## Shape of the API

- **`AvalonClient`** — construct with an `AvalonConfig` (server URL,
  integrator credential key id, optional integrator slug and Ed25519
  signing key seed), then `AuthenticateAsync(identityToken)` to get a
  `Session`.
- **`Session`** — the authenticated, capability-checked entry point for
  everything else. Every method fast-fails client-side with
  `CapabilityNotGrantedException` if the integrator wasn't granted the
  capability it needs, mirroring `SdkError::CapabilityNotGranted` — the
  server enforces the same check independently; this is a fast
  fail, not the actual security boundary.
- **`AccountSession`** (and its partial-class split —
  `AccountSession.Social.cs`, `.GuildAdmin.cs`, `.Conversations.cs`,
  `.Devices.cs`, `.Passkeys.cs`, `.Recovery.cs`, `.Integrations.cs`,
  `.DeviceLogin.cs`) — the identity-owner-authenticated surface: friend
  requests, blocks, guild administration (roles, permission overrides,
  channels, events, invites, join requests, ownership transfer), device
  and passkey management, guardian recovery, integrator connections.
- **`Guilds.cs`**, **`Social.cs`**, **`Conversations.cs`**,
  **`Achievements.cs`**, **`SyncJournal.cs`**, **`CrossNodeLogin.cs`** —
  the remaining domain surfaces, one file per area, mirroring the Rust
  SDK's own module split.

Every exception type mirrors a specific `SdkError` variant from the Rust
SDK by design (see each exception's own doc comment in `Session.cs`) —
`AuthenticationFailedException` (`SdkError::Unauthorized`),
`CapabilityNotGrantedException` (`SdkError::CapabilityNotGranted`), and
`AvalonRequestException` for the remaining transport-level cases
(`SdkError::Unavailable`/`Protocol`) — so error handling reads the same way
regardless of which language's SDK you're using.

## Getting started

```csharp
var config = new AvalonConfig(
    serverUrl: "https://your-avalon-node.example",
    integratorCredentialKeyId: "your-integrator-key-id",
    integratorSlug: "your-integrator-slug",   // only needed to issue achievements
    signingKey: yourEd25519SigningKeySeed);   // only needed to issue achievements

var client = new AvalonClient(config);
Session session = await client.AuthenticateAsync(identityToken);

var profile = await session.IdentityProfileAsync(someIdentityId);
```

## Known scoping gap, not an oversight

This SDK doesn't port every account-session construction path the Rust SDK
has (e.g. `Register`, `AccountLogin`, `AddPasskeyAsync`, or the
`AccountCredentials` type used to log back into the same identity on the
same device) — called out explicitly in both
[`../architecture/sdk.md`](../architecture/sdk.md) and `AccountSession.cs`'s
own header comment. Passkey/WebAuthn registration flows are expected to
stay Hub/browser-side; a Unity game logging a player in hands the SDK an
already-issued identity token rather than driving passkey registration
itself.

## No parallel developer guide yet

Unlike the [Rust SDK](../rust/README.md), this one doesn't have its own
step-by-step guide set (getting started / capabilities / achievements /
guilds-and-friends / errors-and-retries) yet. Until then, the Rust guides
under
[`../rust/for-developers/`](../rust/for-developers/README.md) are the best
walkthrough of the *concepts* (capability grants, achievement lifecycle,
error taxonomy); the method names and types above are this SDK's concrete
mapping of those same concepts.

## Related

- [`../README.md`](../README.md) — the SDK project overview, and the other languages.
- [`../rust/README.md`](../rust/README.md) — the reference implementation this SDK mirrors.
- [`../../backend-server/README.md`](../../backend-server/README.md) — what this SDK talks to.
