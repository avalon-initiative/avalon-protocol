# Getting Started (Rust SDK)

Ten minutes: add the crate, build a client, authenticate, read a profile.
This page is about *doing*; for the concepts behind what you're doing (what
a session token proves, what a capability grant is, what an attestation's
"authentic"/"valid"/"recognized" split means), see
[`../projects/backend-server/architecture/sdk.md`](../projects/rust-sdk/architecture/sdk.md) and
[`../projects/backend-server/architecture/trust-model.md`](../projects/backend-server/architecture/trust-model.md).

## 1. Add the crate

```toml
[dependencies]
avalon-sdk = { git = "https://github.com/LunarVagabond/avalon-protocol" }
```

(Not yet published to crates.io — a git dependency, or a path dependency if
you're working inside this workspace, until it is.)

## 2. You need a server to talk to

For local development, clone this repo and run `make start` (see
[`local-development.md`](local-development.md)) — it needs `.env` with a
`DATABASE_URL` pointing at a real Postgres, per the repo root
`README.md`/`CLAUDE.md`.

## 3. Build a client

```rust
use avalon_sdk::{AvalonClient, AvalonConfig};

let client = AvalonClient::new(AvalonConfig {
    server_url: "http://127.0.0.1:8080".to_string(),
    // Your integrator's own registered credential key id — identifies
    // which integrator's capability grants `authenticate()` looks up.
    // An empty string is fine for read-only calls that need no grant at
    // all (like the profile read below).
    integrator_credential_key_id: String::new(),
    // Only needed for achievement issuance — see achievements.md.
    integrator_slug: None,
    signing_key: None,
    // Sensible retry/timeout defaults — see errors-and-retries.md.
    retry: Default::default(),
});
```

## 4. Authenticate

`authenticate()` takes a **session token** — something your game/app/service
never mints itself. A player gets one by logging into Avalon through the Hub
(WebAuthn passkey) or a `avalon-cli login`/`pair-device` flow during local
dev (see [`local-development.md`](local-development.md)); your integration
receives it however your own UI hands it off (a deep link, a paste-in field,
a redirect callback — this repo doesn't prescribe the transport).

```rust
let session = client.authenticate(&session_token).await?;
```

This does two things: `GET /me` for the identity's own profile, and
`GET /me/grants` for this integrator's own active capability grants — see
[`capabilities.md`](capabilities.md) for what a grant is and how a player
gives one.

## 5. Read the profile

```rust
let profile = session.profile();
println!("Hello, {}", profile.display_name);
println!("Identity id: {}", session.identity().id.0);
```

No capability grant needed for this much — `GET /me` only requires a valid
session.

## C# / Unity equivalent

`bindings/csharp/AvalonSdk` (targets netstandard2.1, so it works unmodified
in Unity/IL2CPP) mirrors the same four steps — same method names translated
to C# idiom, same two-call `authenticate` (`GET /me` + `GET /me/grants`),
same typed exceptions in place of `SdkError`:

```csharp
using Avalon.Sdk;

var client = new AvalonClient(new AvalonConfig(
    serverUrl: "http://127.0.0.1:8080",
    // Your integrator's own registered credential key id — empty string is
    // fine for read-only calls that need no grant at all.
    integratorCredentialKeyId: ""));

Session session;
try
{
    session = await client.AuthenticateAsync(sessionToken);
}
catch (AuthenticationFailedException)
{
    // The token itself was rejected — distinct from CapabilityNotGrantedException,
    // which means the token is fine but a specific method's capability isn't granted.
    throw;
}

Console.WriteLine($"Hello, {session.Profile.DisplayName}");
Console.WriteLine($"Identity id: {session.Identity.Id}");
```

`AvalonClient`'s `HttpClient` is constructor-injected (`new AvalonClient(config, myHttpClient)`)
so a Unity project can supply its own handler; a fresh one is used by default.
See [`achievements.md`](achievements.md) for `GetAchievementsAsync`/
`IssueAchievementAsync`, which additionally need `AvalonConfig.IntegratorSlug`/
`SigningKey` — the C# `IssueAchievementAsync` signs with a pure-managed
`BouncyCastle.Cryptography` Ed25519 implementation rather than a native
library, keeping the core package Unity/IL2CPP-safe with no engine
dependency.

## Where to go next

- [`capabilities.md`](capabilities.md) — what else you can ask for, and what
  happens when you haven't been granted it.
- [`achievements.md`](achievements.md) — define, issue, read, verify, revoke.
- [`guilds-and-friends.md`](guilds-and-friends.md) — rosters and presence.
- [`errors-and-retries.md`](errors-and-retries.md) — the `SdkError` taxonomy
  and what's safe to retry.
- A complete, runnable version of everything above is
  `crates/sdk/examples/authenticate.rs` — `cargo run -p avalon-sdk --example
  authenticate`.
