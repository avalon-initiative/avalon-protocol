# SDK

**Avalon exposes protocol capabilities, not infrastructure.** A developer
thinks in identity, guilds, achievements, presence, and integrator event verification —
never in Postgres instances, chain RPCs, indexer shards, or node addresses.
**Every capability-gated method checks its own required grant**; the SDK never
trusts the caller. Narrative:
[Proposal §17](../stakeholders/Proposal.md#17-developer-experience) and
[§24](../stakeholders/Proposal.md#24-phase-2--developer-sdk).

## Target shape

This is direction, not the current API:

```rust
let avalon = Avalon::connect().await?;

let identity = avalon.identity(user_id).await?;

let friends = identity.friends().await?;
let guilds = identity.guilds().await?;
let achievements = identity.achievements().await?;

avalon.presence().publish(...).await?;

avalon
    .achievement("dragon_slayer")
    .issue(user_id)
    .await?;
```

Not this:

```rust
let avalon = Avalon::connect("postgres://...").await?;
```

and not "which Postgres, which Redis, which chain RPC, which indexer, which
region, which node". A developer should be able to say "I want identity,
guilds, achievements, cross-integrator event verification, and presence" and
consume exactly those.

## What the SDK abstracts

| Concern | Hidden from the integrator |
|---|---|
| node discovery and selection | latency, proximity, capabilities, health |
| authentication | identity session exchange, integrator credential |
| protocol version and capability negotiation | which node roles are reachable |
| retries, failover, routing | a node disappearing (scenario K) |
| realtime connections | presence transport |
| settlement submission | batching, commitments, whichever backend |
| verification | signature checks, key resolution at issuance time, history walks |
| indexing topology | which projection served the read |
| infrastructure changes | a backend swap never reaches integrator code |
| offline/deferred participation | whether the call went out now or was journaled for later — see [synchronization](./synchronization.md) |

See [nodes](./nodes.md) for what discovery selects among,
[settlement](./settlement.md) for what "submission" hides, and
[synchronization](./synchronization.md) for what happens when there's no
node to reach at all.

## Verification surfaces three results, not one

From the [trust model](./trust-model.md): authentic, valid, and recognized are
separate answers. The SDK returns them separately so an integrator can:

- show all authentic-and-valid claims with provenance (a Hub-style view), and
- apply gameplay effects only to claims it recognizes under its own policy.

Collapsing them into one boolean would make the SDK the judge of meaning, which
is exactly the authority Avalon does not have.

## Capability checks per method

A `Session` is scoped to the capabilities the user actually granted the integrator
under an active [binding](./bindings.md). `achievements()` requires
`achievements.read`; `issue_achievement()` requires `achievements.issue`;
`friends()` requires `friends.read`; and so on. A method with no grant
fails with `CapabilityNotGranted` rather than silently returning less.

`Capability` (`crates/protocol/src/permissions.rs`, #98) is an enum with a
permanent-string mapping, not a bare `String`: `Capability::KNOWN` lists
every known variant, `as_str()`/`Display` give the wire string each one
(de)serializes as, and `Other(String)` preserves any capability string this
build doesn't know about yet rather than erroring — the wire string, not the
Rust variant name, is the permanent identifier. The starting capability list
is in [Proposal §13](../stakeholders/Proposal.md#13-permission-model).

## Error handling and retries (issue #47)

`SdkError` is a small, stable, protocol-level taxonomy — `Unauthorized`,
`CapabilityNotGranted`, `NotFound`, `Conflict`, `Rejected`, `Unavailable`,
plus `Protocol` for a genuinely unexpected response — never a raw
`reqwest::Error`/`StatusCode` a game would have to know HTTP to interpret.
The bucket a non-success response maps into is chosen by HTTP status (a
small, stable, always-present signal every one of `AppError`'s variants
funnels through); the message text carried inside that bucket is
`AppError::code` (a stable, machine-readable identifier, one per server
error variant) whenever the response has one, never the free-text `error`
message, which stays free to reword without being a breaking change.

**Retries.** Connection errors, timeouts, and 502/503/504 — the transient
class a node hiccup produces — are retried with exponential backoff and
full jitter, *but only for a call that opts in as idempotent*: every read
opts in automatically; a write opts in only when it carries something
that makes a retry provably safe — an `Idempotency-Key` header the server
honors (achievement issuance, the concrete case wired up so far — see
below), or an endpoint-specific dedup key a write already had for other
reasons (`ConversationHandle::send`'s `client_entry_id`, #111's own
submission-engine dedup), or the write's own HTTP method already
guarantees it (`PUT /me/presence`). A write with none of those gets
exactly one attempt — retrying it blind risks applying it twice.
`AvalonConfig::retry` (`RetryConfig { max_retries, base_delay,
request_timeout }`) tunes this per integration; `RetryConfig::default()`
is 3 retries, 200ms base, 10s per-request timeout.

**Idempotency-Key coverage is intentionally partial in this first pass.**
Only `Session::issue_achievement` carries one — the ticket's own named
example, and the write where a duplicate is worst (a second, spurious
attestation). Every other unkeyed write (`dm`, `ConversationHandle::send`
with no `client_entry_id`, `AvalonClient::login`,
`Session::publish_schema_version`/`publish_instance`) stays single-attempt
rather than being retried unsafely; extending real key coverage to those
is a documented follow-up, not silently assumed done.

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
  integrator_credential_key_id, integrator_slug, signing_key })` and
  `authenticate(identity_token)` wired to a real `avalon-server` (`GET /me`,
  `GET /me/grants`). `Session::require(capability)` is the per-method check.
  `integrator_slug`/`signing_key` are `Option`s: `None` for a read-only
  integration, required by `issue_achievement`.
  `Session::grant_for_testing(capability)` (`#[doc(hidden)]`, gated behind
  the `test-util` feature) remains for tests that want a granted `Session`
  without driving the full consent flow.
- `crates/sdk/src/achievements.rs` (#34) — `Session::achievements()`
  (`achievements.read`, `GET /me/achievements` — new, #34, listing the
  identity's own full attestation history across every issuer) and
  `Session::issue_achievement(key)` (`achievements.issue`, `POST
  /integrations/{slug}/achievements/{key}/issue`) are both wired to a real server;
  neither returns `NotImplemented` any more. `VerifiedAttestation` carries
  `authenticity`/`validity`/`history` as the server computed them —
  deliberately no `recognition` field, matching `GET /attestations/{id}`'s
  (#33) own posture; an integrator wanting a recognition verdict filters through
  its own policy. Issuing signs locally: `AvalonConfig::signing_key` never
  leaves this process, only a detached signature does, and the same
  challenge-response proof every other issuer-credentialed endpoint in this
  repo uses (`POST /integrations/{slug}/challenge`) authenticates the HTTP call
  itself. Milestones (`issue_milestone`/`milestones()`, the App/Service
  equivalent) aren't wired up yet — same shape, not this ticket's scope.
  Found and fixed a real pre-existing bug while live-verifying this: `GET
  /me/grants` only ever read the deprecated `x-avalon-game-key-id` header,
  silently ignoring the generalized `x-avalon-integrator-key-id` name the
  SDK actually sends (per #293) — every grant this SDK ever fetched was
  therefore invisible to `Session::require`, not just this ticket's new
  methods (this was issue #322's real root cause too, closed alongside this
  fix).
- `crates/sdk/src/social.rs` (#17) — `Session::friends()` (`friends.read`),
  `presence()` and `presence_of(&[IdentityId])` (`presence.read`), wired to
  `GET /friends` and `GET /presence?ids=…` (#15/#16). `friends()` embeds each
  friend's `Presence` only when `presence.read` is also granted, via one
  batched `presence_of` call. `Friend.display_name` is always `None` today —
  no endpoint resolves another identity's profile yet. `Session::update_presence(status)`
  wraps `PUT /me/presence` (a user publishing their own status); it
  deliberately isn't the integrator-authority `AvalonClient::publish_presence`
  this issue originally described, since that needs an integrator-credential/binding
  system (#26/#28/#83) that doesn't exist — see the module doc comment for
  the full reasoning. `presence_of` applies no visibility filtering (#87).
  `Session::subscribe_presence(&[IdentityId])` (`presence.read`, #136 —
  decided in #119) is the live push counterpart, additive to `presence()`/
  `presence_of()`: connects to `GET /ws/presence` (auth via `?token=`, not
  a header — a websocket handshake can't send one), sends one `subscribe`
  message, and returns a `tokio::sync::mpsc::UnboundedReceiver<Presence>`
  fed by a background task for the connection's lifetime. `friends()`
  itself is unchanged — still a one-shot `presence_of` batch, not
  auto-subscribed.
- `crates/sdk/src/guilds.rs` (#23) — `Session::guilds()` (`guilds.read`,
  wired to `GET /me/guilds`, with one follow-up `GET /guilds/{id}` per
  membership to fill in the full `Guild` that endpoint doesn't itself
  return) and `Session::guild(id)`, a `GuildHandle` closing over a guild id
  with `roster()` (`guilds.read`, `GET /guilds/{id}/members`) and
  `channels()` (`guilds.chat`, `GET /guilds/{id}/channels`), plus
  `GuildHandle::channel(cid)`, a `ChannelHandle` with `messages(before,
  limit)` and `send(body)` (both `guilds.chat`, wired to `GET`/`POST
  /guilds/{id}/channels/{cid}/messages`). No `guilds.*` blanket check —
  reads use `guilds.read`, chat uses `guilds.chat`. `roster()` embeds each
  member's `Presence` only when `presence.read` is also granted, same
  batched-`presence_of` pattern `friends()` uses, and returns
  `GuildRosterMember` (an SDK-side view type wrapping the protocol
  `GuildMember`) rather than the raw protocol type, for the same reason
  `friends()` returns `Friend` instead of `Friendship` — there's nowhere on
  the protocol type to put the merged `Presence`. `roster()`/`channels()`/
  `messages()` apply no visibility scoping (#87, same gap `presence_of`
  already has). Creating guilds, inviting, kicking, changing roles, and
  managing channels are deliberately not on the SDK — user-authority-only
  actions taken through the Hub.
- `crates/sdk/src/conversations.rs` (#104) — `Session::conversations()`
  (`messages.read`, `GET /conversations`) and `Session::conversation(id)`, an
  ungated `ConversationHandle` constructor mirroring `Session::guild(id)`,
  with `messages(before, limit)` (`messages.read`, `GET
  /conversations/{id}/messages`) and `send(body)` (`messages.send`, `POST
  /conversations/{id}/messages`). `Session::dm(other_identity_id)`
  (`messages.send`, `POST /conversations`) creates-or-gets the 1:1
  conversation with another identity and returns a `ConversationHandle` —
  gated on `messages.send` rather than `messages.read` so a send-only grant
  can still open a conversation; a read-only grant reaches the same
  conversations through `conversations()` plus `conversation(id)` instead.
  No `messages.*` blanket check, same posture `guilds.rs` takes for
  `guilds.read`/`guilds.chat`. A rejected read or send — whether the caller
  was never a participant or is a blocked one (#97) — surfaces as the same
  `SdkError::NotConversationParticipant`, mapped from the server's identical
  `403` for both cases without inspecting the response body, so the SDK
  never has more to leak than the server does.
- `crates/sdk/src/device_login.rs` (#398) — `AvalonClient::login()` wraps
  #307's cross-device pairing (`POST /auth/device/start`) for a client with
  no WebAuthn surface of its own (a game engine, a console), returning a
  `DeviceLogin` carrying `user_code`/`verification_uri` — everything needed
  to render a code or QR, with no QR library forced on the caller.
  `DeviceLogin::wait()` drives `POST /auth/device/poll` to completion,
  sleeping the server's own `poll_interval` between polls and doubling it
  (capped at 60s) on `slow_down` rather than treating it as plain `pending`.
  Resolves to a real `Session` on `approved` by feeding the minted token
  through `AvalonClient::authenticate` — the same path a normal WebAuthn
  login already uses — or a typed `SdkError::DeviceLoginDenied`/
  `DeviceLoginExpired` on `denied`/`expired`.
- `crates/sdk/tests/device_login.rs` — live tests (`make test-live`)
  covering `wait()` resolving to a session matching the approving
  identity's profile once `POST /auth/device/approve` is called against
  the seeded `user_code`, and `wait()` surfacing `DeviceLoginDenied` once
  `POST /auth/device/deny` is called instead.
- `crates/sdk/tests/authenticate.rs` — live test (`make test-live`) covering a
  successful authenticate, an invalid token, and a capability being rejected.
- `crates/sdk/tests/social.rs` — live tests (`make test-live`) covering
  `friends()` returning a friendship created through the HTTP API,
  `presence.read` gating whether presence is embedded in `friends()`,
  `update_presence`/`presence`/`presence_of` round-tripping through a real
  server, and `subscribe_presence` receiving a real pushed update (plus its
  own capability check) over a real websocket connection.
- `crates/sdk/tests/guilds.rs` — live tests (`make test-live`) covering
  `guilds()` listing a membership created through `POST /guilds`,
  `roster()` returning the owner as a member with no presence embedded
  without `presence.read`, `channel(cid).send()` then `.messages()`
  round-tripping a message through the default `general` channel every
  guild is seeded with, and `guilds.chat` being required independently of
  `guilds.read`.
- `crates/sdk/src/sync_journal.rs` (#110, see
  [synchronization](./synchronization.md)) — `SyncJournal` trait
  (`append`/`pending`/`all`/`entry`/`mark_submitted`/`mark_rejected`/
  `mark_failed`, plus #113's `status`/`status_of` default methods) plus
  `FileJournal`, a dependency-light reference implementation: an
  append-only, `fsync`-per-write JSON-lines file, replayed on `open()` to
  recover pending state after a crash. `EntryId` is a client-generated
  `Uuid`, stable and never server-assigned. `mark_submitted`/`mark_rejected`
  are both idempotent and mutually exclusive (never overwrite each other);
  `append` never deduplicates identical payloads — all unit-tested in the
  same file, no `make test-live`/Postgres dependency. `AvalonClient`/
  `Session` don't call it yet — that's #111 (deferred submission engine),
  which drains and submits what an integrator journals, and
  `SubmissionEngine::subscribe` (#113) for terminal-transition
  notifications.
- `crates/sdk/tests/conversations.rs` — live tests (`make test-live`)
  covering `dm()`/`send()`/`messages()` round-tripping across two real
  sessions (alice starts and sends, bob discovers the conversation via
  `conversations()` and replies through `conversation(id)`), a missing
  capability grant being rejected before any request, and a non-participant
  reading or sending into someone else's conversation being rejected with
  `NotConversationParticipant`.
- `crates/schema-derive` (`avalon-schema-derive`, #386) —
  `#[derive(AvalonSchema)]`, a proc-macro generating the `.proto` message
  text and `default_visibility`/`field_visibility` maps an Integrator
  Space schema publication (#255/#381/#384) needs, from an ordinary Rust
  struct — an integrator writes `#[derive(AvalonSchema, Serialize)]` once
  and never hand-writes protobuf syntax or a request body by hand. Split
  into a thin `lib.rs` entry point plus a pure `codegen.rs` operating on
  `syn::DeriveInput`, unit-tested directly (`cargo test -p
  avalon-schema-derive`, 8 tests) without a second compiling crate or
  `trybuild`. Supports `String`/`bool`/the integer and float scalar
  types, and `Option<T>`/`Vec<T>` of any of those (`optional`/`repeated`
  in the generated proto); anything else is a compile error naming the
  field, not a silently wrong schema. Field numbers are sequential in
  declaration order, starting at 1 — safe because a schema version is
  immutable once published (#255), so there's no in-place field-number
  evolution to design around. `#[avalon(default_visibility = "private")]`
  (struct-level) and `#[avalon(visibility = "private")]` (field-level,
  either direction) map to #381's visibility model; a field without the
  attribute is simply omitted from `field_visibility` rather than listed
  redundantly, so there's nothing that can drift from the struct's actual
  fields.
- `crates/sdk/src/schema.rs` (#386) — the `AvalonSchema` trait (re-exported
  from `avalon-schema-derive`) plus `Session::publish_schema_version::<T>()`
  (`POST /integrations/{slug}/schemas`) and
  `Session::publish_instance::<T>(version, &instance)` (`POST
  /integrations/{slug}/schemas/{version}/data`), both authenticated via the
  same integrator challenge-response ceremony
  `achievements.rs::submit_achievement_issuance` uses — proof the caller's
  key is making this call right now — but, unlike achievement issuance,
  with no second content-specific signature, matching those two endpoints'
  own server-side `authenticate_owning_integrator` guard (no
  `permission_grants` capability check either; `publish_instance` only
  requires the subject have an active binding to the calling integrator).
  `publish_instance` always targets the session's own identity as
  `subject`, the same "issue to the session's own identity" simplification
  `issue_achievement` already takes. Live-verified end to end: deriving
  `AvalonSchema` on a struct with a scalar, an `Option`, a `Vec`, and one
  `#[avalon(visibility = "private")]` field; publishing the schema;
  publishing an instance; and reading it back through the public `GET
  /identities/{id}/integrator-data` — proving both that plain
  `serde_json::to_value` of the struct (snake_case, no `rename_all` needed)
  parses against the generated proto's own snake_case field names, and
  that the private field is actually redacted from an unauthenticated
  read (`crates/sdk/tests/schema.rs`).
- `crates/sdk/src/http.rs` (#47) — `send`/`map_error_response`/
  `retry_write`, the shared machinery behind the "Error handling and
  retries" section above; every HTTP call site in this crate (achievements,
  conversations, device_login, guilds, registry, schema, social) goes
  through it now instead of a raw `reqwest` call with ad hoc status
  matching. `crates/server/src/error.rs::AppError::code` gives every
  variant a stable, mechanically-generated (one per Rust variant name)
  machine-readable identifier alongside the existing free-text `error`
  message. `crates/server/src/idempotency.rs`
  (`db/migrations/0054_idempotency_keys`) is the server-side cache
  `achievements::issue_attestation` checks/writes when an
  `Idempotency-Key` header is present — see this section's own note on
  why only that one write has real key coverage so far. Unit-tested with
  a local `wiremock` server (`crates/sdk/src/http.rs`'s own test module —
  status-to-variant mapping, 503-then-success retry, a non-idempotent
  write staying single-attempt, a keyed write reusing the same key across
  retries, and a timeout) and live-verified end to end
  (`crates/sdk/tests/achievements.rs`: a repeated `Idempotency-Key`
  replays the same attestation id rather than minting a second one).
- `AvalonConfig { server_url }` is the opposite of the `connect()` target; that
  gap is [#91](https://github.com/LunarVagabond/avalon-protocol/issues/91).
- `bindings/csharp/AvalonSdk/` — a real, building C# port of the friends/
  presence, guilds, conversations, and sync-journal surface (issue #396):
  `Social.cs` (`FriendsAsync`/`PresenceAsync`/`PresenceOfAsync`/
  `UpdatePresenceAsync`/`SubscribePresenceAsync`, the last over
  `ClientWebSocket` and a `System.Threading.Channels.ChannelReader<Presence>`),
  `Guilds.cs` (`GuildsAsync`, `Guild(id)` → `GuildHandle` with `RosterAsync`/
  `ChannelsAsync`/`EventsAsync`, `ChannelHandle` with `MessagesAsync`/
  `SendAsync`), `Conversations.cs` (`ConversationsAsync`, `Conversation(id)` →
  `ConversationHandle`, `DmAsync`), and `SyncJournal.cs` (an `ISyncJournal`
  interface plus `FileJournal`, the same append-only `fsync`-per-write
  JSON-lines reference implementation and crash-recovery behavior as the Rust
  `FileJournal`, unit-tested against the same scenarios). `Session.cs`/
  `AvalonClient.cs` grew the HTTP/token plumbing (`AuthenticateAsync` now
  calls a real `GET /me` + `GET /me/grants`) the rest of the port needs — the
  achievements stub from #51 is left as-is, out of #396's scope. Same
  capability-check-before-any-request and no-visibility-scoping-yet (#87)
  posture as the Rust SDK throughout. `AvalonSdk.Tests/` covers it with
  `HttpMessageHandler`-stubbed unit tests plus opt-in live tests
  (`AVALON_SERVER_URL`/`DATABASE_URL`) mirroring `crates/sdk/tests/social.rs`,
  `guilds.rs`, and `conversations.rs`. `make csharp-build`/`make csharp-test`
  pass.

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
- [#102](https://github.com/LunarVagabond/avalon-protocol/issues/102) —
  conversation domain model + server endpoints;
  [#104](https://github.com/LunarVagabond/avalon-protocol/issues/104) — this
  SDK surface.
