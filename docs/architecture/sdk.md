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
- `crates/sdk/src/cross_node_login.rs` (epic #623, issue #637) —
  `AvalonClient::cross_node_login()` wraps #634's cross-node login
  (`POST /auth/cross-node/start`), almost identical in shape to
  `device_login`'s own start/poll dance: from a caller's perspective both
  flows look the same. The one real difference: the response carries no
  `verification_uri` the way device pairing's does — there's no Hub route
  for cross-node approval yet (#639, not built), just a `user_code` and
  `requesting_context` to show however the integrator's own UI displays a
  pairing code. `CrossNodeLogin::wait()` drives `POST /auth/cross-node/poll`
  to completion with the identical backoff shape `DeviceLogin::wait` uses,
  resolving to a real `Session` on `approved` or a typed
  `SdkError::CrossNodeLoginDenied`/`CrossNodeLoginExpired` on
  `denied`/`expired`. Also exposes the same-device fast path epic #623's own
  scope note describes: `AvalonClient::submit_cross_node_login_grant`
  mints, signs (with a caller-supplied `ed25519_dalek::SigningKey`), and
  submits a `CrossNodeLoginGrant` directly, skipping the start/poll dance
  entirely — real for a caller that directly controls some identity's key
  material (e.g. a service/bot identity), not the common case for this
  SDK's usual integrator-backend callers, who never hold a *player's* own
  key.
- `crates/sdk/tests/cross_node_login.rs` — live tests (`make test-live`)
  covering `wait()` resolving to a session once a real signed grant is
  submitted against the seeded `user_code` (simulating the Hub approval
  #639 will eventually provide), `wait()` surfacing
  `CrossNodeLoginDenied` once `POST /auth/cross-node/deny` is called
  instead, the same-device fast path resolving directly to a session with
  no polling at all, and a grant signed by the wrong key being rejected.
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
  conversations, cross_node_login, device_login, guilds, registry, schema,
  social) goes
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
- `bindings/csharp/AvalonSdk/` — a real, building C# port of the auth/session,
  friends/presence, guilds, conversations, sync-journal, and achievements
  surface (issues #51/#396): `AvalonClient.cs`/`Session.cs`
  (`AuthenticateAsync` calls a real `GET /me` + `GET /me/grants` and
  populates `Session.Identity`/`Session.Profile`, mirroring the Rust SDK's
  `Session::identity()`/`profile()`), `Social.cs`
  (`FriendsAsync`/`PresenceAsync`/`PresenceOfAsync`/`UpdatePresenceAsync`/
  `SubscribePresenceAsync`, the last over `ClientWebSocket` and a
  `System.Threading.Channels.ChannelReader<Presence>`), `Guilds.cs`
  (`GuildsAsync`, `Guild(id)` → `GuildHandle` with
  `RosterAsync`/`ChannelsAsync`/`EventsAsync`, `ChannelHandle` with
  `MessagesAsync`/`SendAsync`), `Conversations.cs` (`ConversationsAsync`,
  `Conversation(id)` → `ConversationHandle`, `DmAsync`), `SyncJournal.cs` (an
  `ISyncJournal` interface plus `FileJournal`, the same append-only
  `fsync`-per-write JSON-lines reference implementation and crash-recovery
  behavior as the Rust `FileJournal`, unit-tested against the same
  scenarios), and `Achievements.cs` (`GetAchievementsAsync`/
  `IssueAchievementAsync`, the same challenge-response-plus-embedded-
  signature ceremony as `crates/sdk/src/achievements.rs`'s single-claim
  path, signed with a pure-managed `BouncyCastle.Cryptography` Ed25519
  implementation rather than a native library — the smallest change that
  keeps this Unity/IL2CPP-safe — and the same `Authenticity`/`Validity`/
  `history` split, never a combined "trusted" boolean, per ADR #76). Bulk
  issuance (#495) and revocation (#498) are Rust-side-only so far, no C#
  equivalent yet. Typed exceptions
  (`AuthenticationFailedException`/`CapabilityNotGrantedException`/
  `AvalonRequestException`/`MissingIssuerCredentialsException`/
  `AvalonWebSocketException`/`NotConversationParticipantException`) mirror
  the Rust `SdkError` variants this SDK has real coverage for.
  `AvalonClient`'s `HttpClient` is constructor-injected (a fresh instance by
  default), so a Unity project or a test can supply its own handler. Same
  capability-check-before-any-request and no-visibility-scoping-yet (#87)
  posture as the Rust SDK throughout. `AvalonSdk.Tests/` covers it with
  `HttpMessageHandler`-stubbed unit tests plus opt-in live tests
  (`AVALON_SERVER_URL`/`DATABASE_URL`, the latter an Npgsql-style
  keyword/value string rather than `.env`'s own `postgres://` URI) mirroring
  `crates/sdk/tests/social.rs`, `guilds.rs`, `conversations.rs`, and
  `achievements.rs` — live-verified end to end against a real `avalon-server`
  and Postgres: issuing an achievement and reading it back with its
  authenticity/validity populated, and the no-signing-key-configured case
  rejecting client-side without an HTTP call. `make csharp-build`/`make
  csharp-test` pass.
- `bindings/csharp/AvalonSdk/CrossNodeLogin.cs` (epic #623, issue #638) —
  ports the Rust SDK's `cross_node_login` shape: `AvalonClient.CrossNodeLoginAsync`/
  `CrossNodeLogin.WaitAsync` (identical start-then-poll dance and backoff to
  the Rust side), and the same-device fast path
  `AvalonClient.SubmitCrossNodeLoginGrantAsync`, which signs with a
  caller-supplied 32-byte Ed25519 identity-key seed — the first place this
  SDK signs with a *player's* own key rather than an integrator's issuer
  key (see `AvalonSdk.csproj`'s updated `BouncyCastle.Cryptography` comment).
  `CrossNodeLoginDeniedException`/`CrossNodeLoginExpiredException` mirror
  `SdkError::CrossNodeLoginDenied`/`CrossNodeLoginExpired`.
  `AvalonSdk.Tests/CrossNodeLoginTests.cs` covers the orchestration
  (start/poll/backoff/approved/denied/expired, and the same-device path)
  against a stubbed `HttpMessageHandler`; `LiveTests.cs` adds three
  end-to-end cases against a real server and Postgres — same-device submit
  resolving directly to a session, a wrong-key grant rejected, and the
  cross-device `WaitAsync` resolving once a real signed grant is submitted
  against its `UserCode` (a "simulated Hub," since #639 isn't built yet) —
  seeding the identity's real Ed25519 keypair directly into
  `indexer_identity_signing_keys` rather than a WebAuthn ceremony, since
  that's the one table cross-node-login verification actually reads.
- `crates/sdk/src/account/` (issue #699, on top of #696's decision and
  #697/#698's action-tier classification/enforcement) — `AccountSession`, a
  second, entirely separate session type alongside the integrator `Session`
  documented above: a first-party client for an identity's own account
  (registration, login, recovery, passkeys, devices, full guild
  administration, friends/blocks/presence/discovery, conversations, and
  integrator connect/consent — the full surface `packages/api-client`
  exposes to Hub). No `From`/`Into` exists between `Session` and
  `AccountSession` in either direction, and no `AccountSession`
  constructor accepts an integrator credential anywhere in its
  signature — #696's hard invariant holds at the type level.
  - Obtained via three entry points on `AvalonClient`, none of them
    `authenticate()`: `register(display_name)` drives a real WebAuthn
    registration ceremony against a virtual (software-only) authenticator
    (`account::webauthn`, the same `passkey-authenticator`/`passkey-client`
    `testable`-feature approach `crates/cli/src/dev_tools.rs::create_identity`
    already proved — duplicated rather than factored into a shared crate,
    a deliberate scoping call for #699 given how small the ceremony-driving
    code is), generates a fresh Ed25519 event-signing key locally, then
    immediately logs the new identity in (`register/finish` itself returns
    only the new `identity_id`, not a bearer token) so the returned session
    is immediately usable; `account_login(&AccountCredentials)` logs back
    into an identity `register` already created, replaying the same
    virtual-authenticator passkey; `resume_account_session(token)` /
    `resume_account_session_with_signing_key(token, seed)` wrap an
    already-minted bearer token, mirroring how Hub resumes a persisted
    session (the `_with_signing_key` variant also resolves this device's
    `identity_signing_keys.id` via `GET /me/devices`, matched by public
    key, so signature-required methods keep signing automatically after a
    resume); `start_account_device_login()` (issue #707, filed after #699
    shipped without it) is a fourth entry point — an `AccountSession`-
    returning counterpart to `crate::device_login`'s existing
    `AvalonClient::login()`/`Session` wrapper around #307's cross-device
    pairing, for a client with no WebAuthn ceremony surface of its own (a
    game engine, a console) that still needs to originate a first-party
    login rather than resume a token minted elsewhere. Returns an
    `AccountDeviceLogin` carrying `user_code`/`verification_uri`;
    `.wait()` polls to completion and resolves to a real `AccountSession`
    via `resume_account_session` once approved from another,
    already-registered device — so, like any `resume_account_session`
    result, it holds no local signing key until this device separately
    requests and gets its own approved via `request_device_grant`.
  - Every action #697 flags as signature-required signs itself
    automatically with `AccountSession::sign` — the caller never
    hand-constructs `signing_key_id`/`signature`. For the conditionally-
    signed endpoints (last-passkey revoke, guardian removal/threshold-
    raise, escalating member-role change), this crate signs
    unconditionally rather than replicating the server's own condition
    client-side — an unused-but-valid signature is harmless, the same
    simplification the Hub frontend already made wiring up #698. A session
    built via `resume_account_session` (token only, no local key) sends
    those requests unsigned; the server's own
    `NO_REGISTERED_SIGNING_KEY`/`FRESH_SIGNATURE_REQUIRED` split
    (`crates/server/src/signature_gate.rs`) surfaces the real problem.
  - Split into one file per domain, mirroring `guilds.rs`/`social.rs`'s
    existing "impl blocks grouped by concern" convention, just for
    `AccountSession` instead of `Session`: `passkeys.rs`, `devices.rs`
    (signing-key devices, device grants, and cross-device pairing
    approval — #704's `POST /auth/device/approve` gap included),
    `recovery.rs`, `social.rs` (friends/blocks/presence/discovery/handle
    resolution/history), `conversations.rs`, `guild_admin.rs` (the large
    one: creation, roles, permission overrides, ownership transfer,
    membership/invites/join-requests, channels/chat, events/RSVP,
    favorite-integrators), and `integrations.rs` (connect/disconnect/
    grants/`GET /me/connections`). `mod.rs` holds the type itself, the
    shared signing/HTTP-call helpers every submodule's methods use, and
    the three `AvalonClient` entry-point methods.
  - `docs/architecture/identity.md`'s canonical
    `avalon:<action_tag>:v1:<field>:<field>:...` format and per-endpoint
    `action_tag`/field table (the #697/#698 section) is reused byte-for-
    byte — `account::canonical_message` is unit-tested directly against
    that exact shape, and `crates/sdk/tests/account_session.rs` (live,
    `--ignored`) proves the auto-minted signature actually verifies
    server-side end to end (register -> a signature-required guild-role
    creation).
  - Known gaps, scoped out of #699 deliberately: `ProfileUpdate` doesn't
    yet expose `main_guild`/`discoverable`/`presence_visibility` (three of
    `PATCH /me`'s less commonly touched fields); the unauthenticated
    recovery-initiation calls (`POST /recovery/requests/start`/`finish`,
    `GET /recovery/requests/{id}`, `GET /identities/{id}/recovery/status`
    — deliberately callable with *no* session, since the whole premise is
    the caller has none for the identity being recovered) aren't wrapped
    yet, since they don't belong on `AccountSession` at all and weren't
    this ticket's focus; `AccountSession` has no `subscribe_presence`/chat
    websocket methods the way `Session` does (issue #136/#438's live-push
    surface), read-only point-in-time polling only.
- `bindings/csharp/AvalonSdk/AccountSession.cs` (+
  `AccountSession.Passkeys.cs`/`.Devices.cs`/`.Recovery.cs`/`.Social.cs`/
  `.Conversations.cs`/`.GuildAdmin.cs`/`.Integrations.cs`, issue #700, on
  top of #699 settling the Rust shape) — a real, building C# port of
  `AccountSession` alongside the existing integrator `Session`: profile,
  passkeys, devices/grants/cross-device pairing, social recovery,
  friends/blocks/presence/discovery, conversations, full guild
  administration, and integrator connect/consent, mirroring
  `crates/sdk/src/account/`'s surface field-for-field and
  method-for-method with C#-idiomatic naming (PascalCase methods, `Async`
  suffix). No cast operator, shared base class, or `AccountSession`
  constructor/factory that accepts an integrator credential anywhere in
  its signature exists between `AccountSession` and `Session` — #696's
  invariant holds at the type level, same as the Rust side.
  - **Deliberate scoping call**: `crates/sdk/src/account/webauthn.rs`
    drives a real WebAuthn ceremony against a virtual/software
    authenticator (`passkey-authenticator`/`passkey-client`'s `testable`
    feature) for `Register`/`AccountLogin`, and `passkeys.rs::add_passkey`
    does the same for registering an *additional* passkey. Nothing in this
    SDK's .NET dependency set does WebAuthn ceremony work at all (no
    equivalent package is wired in, and `LiveTests.cs` itself seeds
    identities/sessions/signing keys directly via SQL rather than driving
    one), and this SDK's real audience — a Unity game binding a bearer
    token/signing key another surface (Hub, a platform's own auth) already
    produced, not something driving its own WebAuthn ceremony inside a
    game client — makes a ceremony-driving entry point low-value relative
    to its dependency cost. So the C# port implements only
    `AvalonClient.ResumeAccountSessionAsync(token)` /
    `ResumeAccountSessionWithSigningKeyAsync(token, signingKeySeed)` as the
    ways to construct an `AccountSession`, and does not port
    `Register`/`AccountLogin`/`AddPasskeyAsync`; `AccountCredentials` (the
    Rust type those calls hand back to log into the same identity again on
    the same device) has no C# equivalent for the same reason. This is a
    scoping decision, not an oversight — `docs/architecture/sdk.md` (this
    section) and `AccountSession.cs`'s own header comment both call it out
    explicitly. Filling the gap that scoping call would otherwise leave —
    a client with no WebAuthn surface still needing to *originate* a
    first-party login, not just resume a token minted elsewhere — issue
    #707's `AccountSession.DeviceLogin.cs` adds
    `AvalonClient.StartAccountDeviceLoginAsync()` -> `AccountDeviceLogin`
    (`UserCode`/`VerificationUri`) -> `WaitAsync()`, mirroring
    `crates/sdk/src/account/device_login.rs` (itself mirroring
    `crates/sdk/src/device_login.rs`'s existing pattern for the integrator
    `Session`, which this SDK has never ported at all — this is a fresh
    port straight from the Rust `AccountSession` side). Resolves to a real
    `AccountSession` via `ResumeAccountSessionAsync` once approved from
    another, already-registered device, so — like any
    `ResumeAccountSessionAsync` result — it holds no local signing key
    until this device separately requests and gets its own approved.
  - Every action #697 flags as signature-required signs itself
    automatically with a private `Sign(actionTag, fields)` helper — the
    caller never hand-constructs `signing_key_id`/`signature`. The
    conditionally-signed endpoints (last-passkey revoke, guardian
    removal/threshold-raise, escalating member-role change) sign
    unconditionally, same "unused-but-valid signature is harmless"
    simplification the Rust SDK and Hub frontend already make. A session
    built via `ResumeAccountSessionAsync` (token only, no local key) sends
    those requests with explicit JSON `null` `signing_key_id`/`signature`
    fields rather than omitting them, matching what
    `crates/server/src/signature_gate.rs::require_fresh_signature` expects
    to see either way. `AccountSession.CanonicalMessage` builds the exact
    `avalon:<action_tag>:v1:<field>:<field>:...` byte string
    `signature_gate::canonical_message` reconstructs server-side, unit-
    tested directly against that shape; signing itself uses the same
    pure-managed `BouncyCastle.Cryptography` Ed25519 (`Ed25519Signer`) the
    rest of this SDK already depends on.
  - Types are split one file per matching `crates/sdk/src/account/*.rs`
    submodule via C# `partial class AccountSession`, same convention
    `CrossNodeLogin.cs` already established for `partial class
    AvalonClient`. Domain types that would otherwise collide with the
    integrator `Session`'s own same-named types (`Guild`, `GuildMember`,
    `GuildChannel`, `GuildMessage`, `GuildEvent`, `Conversation`,
    `ConversationMessage`) are prefixed `Account` (`AccountGuild`,
    `AccountGuildMember`, `AccountConversation`, etc.); `Presence`/
    `PresenceStatus` are reused directly from `Social.cs` since both
    surfaces share the exact same wire shape.
  - `AvalonSdk.Tests/AccountSessionTests.cs` covers the canonical-message
    shape byte-for-byte plus signed-call round trips through
    `StubHttpMessageHandler` (extended with request-body capture for this
    ticket) proving the signature actually verifies against the session's
    known public key, and that an unsigned session sends explicit JSON
    nulls rather than omitting the fields. `AvalonSdk.Tests/LiveTests.cs`
    adds the registration-equivalent (SQL-seeded identity + signing key) ->
    `ResumeAccountSessionWithSigningKeyAsync` -> `guild.role.create`
    round trip `crates/sdk/tests/account_session.rs` proves in Rust, plus
    the companion case (`ResumeAccountSessionAsync` with no local key ->
    the same signature-required call rejected server-side) — both
    live-verified against a real `avalon-server` and Postgres.

- `bindings/ts` (issue #701, on top of #696/#697/#698/#699/#700) — a new,
  self-contained TypeScript SDK implementing both `AccountSession` and
  `IntegratorSession` from scratch, ES modules, `vitest` for tests
  (matching `packages/api-client`'s existing conventions). Not a workspace
  member: intentionally outside the root `package.json`'s `workspaces`
  array and `npm install`ed separately from inside `bindings/ts` itself,
  since the design is for this package to eventually move into its own
  `avalon-sdks` org repo with no internal deps beyond the shared wire
  schema — no dependency (npm workspace or otherwise) on
  `packages/api-client`, `apps/hub`, or `apps/mobile-hub` anywhere in its
  source. `apps/hub` itself is untouched by this ticket; migrating Hub off
  `packages/api-client` onto this SDK is separate, later work.
  - `AccountSession` mirrors #699/#700's surface field-for-field: profile,
    passkeys, devices/grants/cross-device pairing approval, social
    recovery, friends/blocks/presence/discovery, conversations, full guild
    administration, and integrator connect/consent. Domain methods are
    split one file per matching `crates/sdk/src/account/*.rs` submodule
    under `bindings/ts/src/accountSession/` (`passkeys.ts`, `devices.ts`,
    `recovery.ts`, `social.ts`, `conversations.ts`, `guildAdmin.ts`,
    `integrations.ts`, `deviceLogin.ts`), matching the Rust/C# "one file
    per domain" convention — since TS classes can't be split across files
    the way a C# `partial class` can, each domain file instead attaches
    its methods to `AccountSession`'s prototype and merges its own method
    signatures into the `AccountSession` interface via TypeScript
    declaration merging (`declare module './core.js' { interface
    AccountSession { ... } }`), so the result still type-checks as one
    class with the full surface while staying split by domain on disk.
  - Unlike the C# port (which scoped WebAuthn ceremony-driving out
    entirely) and the Rust port (which drives a virtual/software
    authenticator, since it has no browser to run in), this SDK is
    browser-facing and drives a **real** WebAuthn ceremony via
    `@simplewebauthn/browser` — `AvalonClient.register(displayName)` and
    `AvalonClient.login(credentials)` are both implemented for real, not
    scoped out, reimplementing
    `packages/api-client/src/crypto/webauthn.ts`'s ceremony-driving
    pattern and `signingKey.ts`'s Ed25519/canonical-signing-bytes
    approach (via `@noble/curves`/`@noble/hashes`) inside `bindings/ts`
    itself rather than importing that package. `AvalonClient
    .resumeAccountSession(token)` /
    `.resumeAccountSessionWithSigningKey(token, seed)` cover the
    already-minted-token path, and `.startAccountDeviceLogin()` ->
    `AccountDeviceLogin.wait()` (issue #707's pattern) is included from
    day one rather than bolted on later, unlike Rust/C#, which both
    found this gap only after `AccountSession` had already shipped.
  - Every action #697 flags as signature-required signs itself
    automatically via `AccountSession.sign(actionTag, fields)`, using
    `@noble/curves`'s ed25519 over the same `avalon:<action_tag>:v1:...`
    canonical bytes `signature_gate::canonical_message` builds
    server-side — callers never hand-construct
    `signing_key_id`/`signature`. The conditionally-signed endpoints
    (last-passkey revoke, guardian removal/threshold-raise, escalating
    member-role change) sign unconditionally, same simplification every
    other SDK in this repo already makes. A session with no local key
    (`resumeAccountSession` without a seed, or one resolved via
    `startAccountDeviceLogin`) sends those requests with explicit JSON
    `null` `signing_key_id`/`signature` fields.
  - `IntegratorSession` (`bindings/ts/src/integratorSession.ts`) mirrors
    the Rust `Session`/C# `Session` capability-gated model: constructed
    via `AvalonClient.authenticate(...)` (`GET /me` + `GET /me/grants`,
    identified via an `integratorCredentialKeyId`), every method calling
    a private `require(capability)` check before making a request — the
    same fast-fail-client-side-first convention, never the actual
    security boundary (the server enforces the same thing independently).
    Covers friends/presence, guild membership/roster/chat, conversations,
    and achievements read/issue (the latter driving the same
    challenge-response-plus-embedded-signature exchange
    `crates/sdk/src/achievements.rs` does, using the integrator's own
    configured slug/signing key — `MissingIssuerCredentialsError` without
    any HTTP call if neither is configured, matching
    `SdkError::MissingIssuerCredentials`).
  - **No implicit conversion between `AccountSession` and
    `IntegratorSession`** anywhere in this package — no shared base
    class, no cast, no constructor/factory on either that accepts the
    other's credential shape — #696's hard invariant holds at the type
    level here too, same as Rust/C#.
  - Errors are typed subclasses of `AvalonSdkError`
    (`UnauthorizedError`/`CapabilityNotGrantedError`/`NotFoundError`/
    `ConflictError`/`RejectedError`/`UnavailableError`/`ProtocolError`/
    `NotConversationParticipantError`/`MissingIssuerCredentialsError`/
    `DeviceLoginDeniedError`/`DeviceLoginExpiredError`/
    `NoLocalSigningKeyError`), mapped from HTTP status + the server's own
    `{ error, code }` body, mirroring `SdkError`'s variants.
  - **Unit tests** (`vitest`, colocated `*.test.ts` files): the
    canonical-message shape byte-for-byte against the known format
    (`crypto/signing.test.ts`), `AccountSession.sign`'s empty-vs-signed
    behavior, `IntegratorSession`'s capability-gating (throws without a
    network call when ungranted), and a signed-call round trip
    (`accountSession/core.test.ts`) that stubs `fetch`, calls a
    signature-required method, and verifies the captured request body's
    signature against the session's known public key using
    `@noble/curves`'s own `ed25519.verify` — the same "verify server-side-
    equivalently" pattern the C# unit tests use with BouncyCastle.
  - **Live tests** (`bindings/ts/src/account.live.test.ts`, opt-in via
    `AVALON_SERVER_URL`/`AVALON_LIVE_DATABASE_URL`, run with `npm run
    test:live` from `bindings/ts`): SQL-seeded identity + signing key (via
    the `pg` npm client, reading the same `postgres://` URI `DATABASE_URL`
    already uses — no Npgsql-style conversion needed, unlike the C# suite)
    -> `resumeAccountSessionWithSigningKey` -> a signature-required guild
    action (`createGuild` then `createRole`) succeeds and verifies
    server-side; the companion case (no local key -> the same call
    rejected server-side); and a `startAccountDeviceLogin` ->
    `wait()` round trip approved (and, separately, denied) from a second
    SQL-seeded identity/session/signing key, signing the
    `device_pairing.approve` bytes directly over HTTP — same shapes
    `crates/sdk/tests/account_session.rs`/`account_device_login.rs` and
    their C# equivalents cover. All four passed against this sandbox's
    real `avalon-server` and Postgres as of 2026-09-21.
  - **Known scoping note**: `register()`/`login()`'s real WebAuthn
    ceremony is implemented for real (unlike C#'s port, which skipped it
    entirely) but is not live-tested end-to-end — there's no virtual/
    software WebAuthn authenticator readily available to drive from a
    Node/vitest process (the Rust SDK's own virtual-authenticator
    approach is a native-process trick this browser-facing SDK doesn't
    have an equivalent for), so that path has unit/type-level coverage
    only, same category of gap the Rust SDK's own `webauthn.rs` doesn't
    have (it *can* drive a virtual authenticator) but C#'s
    `Register`/`AccountLogin` do (skipped outright there).

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
