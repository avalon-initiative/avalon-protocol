# SDK

**Avalon exposes protocol capabilities, not infrastructure.** A game developer
thinks in identity, guilds, achievements, presence, and game event verification —
never in Postgres instances, chain RPCs, indexer shards, or node addresses.
**Every capability-gated method checks its own required grant**; the SDK never
trusts the caller. Narrative:
[Proposal §17](../stakeholders/Proposal.md#17-developer-experience) and
[§24](../stakeholders/Proposal.md#24-phase-2--developer-sdk).

## Target shape

This is direction, not the current API:

```rust
let avalon = Avalon::connect().await?;

let identity = avalon.identity(player_id).await?;

let friends = identity.friends().await?;
let guilds = identity.guilds().await?;
let achievements = identity.achievements().await?;

avalon.presence().publish(...).await?;

avalon
    .achievement("dragon_slayer")
    .issue(player_id)
    .await?;
```

Not this:

```rust
let avalon = Avalon::connect("postgres://...").await?;
```

and not "which Postgres, which Redis, which chain RPC, which indexer, which
region, which node". A developer should be able to say "I want player identity,
guilds, achievements, cross-game game event verification, and presence" and
consume exactly those.

## What the SDK abstracts

| Concern | Hidden from the game |
|---|---|
| node discovery and selection | latency, proximity, capabilities, health |
| authentication | player session exchange, game credential |
| protocol version and capability negotiation | which node roles are reachable |
| retries, failover, routing | a node disappearing (scenario K) |
| realtime connections | presence transport |
| settlement submission | batching, commitments, whichever backend |
| verification | signature checks, key resolution at issuance time, history walks |
| indexing topology | which projection served the read |
| infrastructure changes | a backend swap never reaches game code |
| offline/deferred participation | whether the call went out now or was journaled for later — see [synchronization](./synchronization.md) |

See [nodes](./nodes.md) for what discovery selects among,
[settlement](./settlement.md) for what "submission" hides, and
[synchronization](./synchronization.md) for what happens when there's no
node to reach at all.

## Verification surfaces three results, not one

From the [trust model](./trust-model.md): authentic, valid, and recognized are
separate answers. The SDK returns them separately so a game can:

- show all authentic-and-valid claims with provenance (a Hub-style view), and
- apply gameplay effects only to claims it recognizes under its own policy.

Collapsing them into one boolean would make the SDK the judge of meaning, which
is exactly the authority Avalon does not have.

## Capability checks per method

A `Session` is scoped to the capabilities the player actually granted the game
under an active [binding](./game-bindings.md). `achievements()` requires
`achievements.read`; `issue_achievement()` requires `achievements.issue`;
`friends()` will require `friends.read`; and so on. A method with no grant
fails with `CapabilityNotGranted` rather than silently returning less. The
starting capability list is in
[Proposal §13](../stakeholders/Proposal.md#13-permission-model).

## Languages

- **Rust** (`crates/sdk`) — the reference implementation.
- **C#** (`bindings/csharp`) — the flagship developer-facing SDK, targeting
  netstandard2.1 for Unity. Mirrors the Rust surface.
- **TypeScript** — next, when a web or Node integration needs it.
- **C++** and others — only as actual integrations demand. Don't build every
  SDK up front.

Non-Rust SDKs and third-party network implementations need only the wire
protocol and the domain model in `crates/protocol`; they never pull in
`avalon-chain` or `avalon-server`.

## Today in the repo

- `crates/sdk/src/lib.rs` — `AvalonClient::new(AvalonConfig { server_url,
  game_credential_key_id })` and `authenticate(player_token)` wired to a real
  `avalon-server` (`GET /me`). `Session::require(capability)` is the per-method
  check; `achievements()` and `issue_achievement()` check it, then return
  `NotImplemented`. `granted` is always empty until grants exist —
  `Session::grant_for_testing(capability)` (`#[doc(hidden)]`) is a temporary
  escape hatch so integration tests can exercise capability-gated methods
  against a real server before the grant system (#26–#28) exists; delete it
  once `authenticate()` can populate `granted` for real.
- `crates/sdk/src/social.rs` (#17) — `Session::friends()` (`friends.read`),
  `presence()` and `presence_of(&[IdentityId])` (`presence.read`), wired to
  `GET /friends` and `GET /presence?ids=…` (#15/#16). `friends()` embeds each
  friend's `Presence` only when `presence.read` is also granted, via one
  batched `presence_of` call. `Friend.display_name` is always `None` today —
  no endpoint resolves another identity's profile yet. `Session::update_presence(status)`
  wraps `PUT /me/presence` (a player publishing their own status); it
  deliberately isn't the game-authority `AvalonClient::publish_presence`
  this issue originally described, since that needs a game-credential/binding
  system (#26/#28/#83) that doesn't exist — see the module doc comment for
  the full reasoning. `presence_of` applies no visibility filtering (#87).
  Delivery is still poll-only from the game's side too; a push transport is
  tracked separately in
  [#119](https://github.com/LunarVagabond/avalon-protocol/issues/119).
- `crates/sdk/tests/authenticate.rs` — live test (`make test-live`) covering a
  successful authenticate, an invalid token, and a capability being rejected.
- `crates/sdk/tests/social.rs` — live tests (`make test-live`) covering
  `friends()` returning a friendship created through the HTTP API,
  `presence.read` gating whether presence is embedded in `friends()`, and
  `update_presence`/`presence`/`presence_of` round-tripping through a real
  server.
- `AvalonConfig { server_url }` is the opposite of the `connect()` target; that
  gap is [#91](https://github.com/LunarVagabond/avalon-protocol/issues/91).
- `bindings/csharp/AvalonSdk/` — `AvalonClient.cs`, `Session.cs` skeleton; no
  verified build.

## Decisions and tickets

- [#45](https://github.com/LunarVagabond/avalon-protocol/issues/45) — Epic: Rust
  SDK & CLI: [#46](https://github.com/LunarVagabond/avalon-protocol/issues/46)
  client/session against the real API,
  [#47](https://github.com/LunarVagabond/avalon-protocol/issues/47) errors,
  retries, typed errors,
  [#48](https://github.com/LunarVagabond/avalon-protocol/issues/48) CLI commands,
  [#49](https://github.com/LunarVagabond/avalon-protocol/issues/49) docs/examples,
  [#91](https://github.com/LunarVagabond/avalon-protocol/issues/91) node discovery
  + capability negotiation (on-hold).
- [#50](https://github.com/LunarVagabond/avalon-protocol/issues/50) — Epic: C# SDK:
  [#51](https://github.com/LunarVagabond/avalon-protocol/issues/51),
  [#52](https://github.com/LunarVagabond/avalon-protocol/issues/52),
  [#53](https://github.com/LunarVagabond/avalon-protocol/issues/53).
- [#76](https://github.com/LunarVagabond/avalon-protocol/issues/76) — ADR: trust
  model (three verification results).
- [#17](https://github.com/LunarVagabond/avalon-protocol/issues/17),
  [#23](https://github.com/LunarVagabond/avalon-protocol/issues/23),
  [#34](https://github.com/LunarVagabond/avalon-protocol/issues/34) — SDK methods
  for friends/presence, guilds, achievements.
