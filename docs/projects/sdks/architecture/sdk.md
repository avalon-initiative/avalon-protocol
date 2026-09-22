# SDK

**Avalon exposes protocol capabilities, not infrastructure.** A developer
thinks in identity, guilds, achievements, presence, and integrator event verification —
never in Postgres instances, chain RPCs, indexer shards, or node addresses.
**Every capability-gated method checks its own required grant**; the SDK never
trusts the caller. Narrative:
[Proposal §17](../../../stakeholders/Proposal.md#17-developer-experience) and
[§24](../../../stakeholders/Proposal.md#24-phase-2--developer-sdk).

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
| offline/deferred participation | whether the call went out now or was journaled for later — see [synchronization](../../backend-server/architecture/synchronization.md) |

See [nodes](../../backend-server/architecture/nodes.md) for what discovery selects among,
[settlement](../../backend-server/architecture/settlement.md) for what "submission" hides, and
[synchronization](../../backend-server/architecture/synchronization.md) for what happens when there's no
node to reach at all.

## Verification surfaces three results, not one

From the [trust model](../../backend-server/architecture/trust-model.md): authentic, valid, and recognized are
separate answers. The SDK returns them separately so an integrator can:

- show all authentic-and-valid claims with provenance (a Hub-style view), and
- apply gameplay effects only to claims it recognizes under its own policy.

Collapsing them into one boolean would make the SDK the judge of meaning, which
is exactly the authority Avalon does not have.

## Capability checks per method

A `Session` is scoped to the capabilities the user actually granted the integrator
under an active [binding](../../backend-server/architecture/bindings.md). `achievements()` requires
`achievements.read`; `issue_achievement()` requires `achievements.issue`;
`friends()` requires `friends.read`; and so on. A method with no grant
fails with `CapabilityNotGranted` rather than silently returning less.

`Capability` (`crates/protocol/src/permissions.rs`, #98) is an enum with a
permanent-string mapping, not a bare `String`: `Capability::KNOWN` lists
every known variant, `as_str()`/`Display` give the wire string each one
(de)serializes as, and `Other(String)` preserves any capability string this
build doesn't know about yet rather than erroring — the wire string, not the
Rust variant name, is the permanent identifier. The starting capability list
is in [Proposal §13](../../../stakeholders/Proposal.md#13-permission-model).

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

## Two session types: capability-gated integrator vs. first-party account (#696)

Everything above (`Session`, capability checks, `Capability`) describes
the capability-gated *integrator* model — a game/app/service authenticating
with its own credential, scoped to whatever an identity explicitly
granted it. #696 added a second, entirely separate session type every
language SDK now implements: `AccountSession`, a first-party client for
an identity's own account — registration, login, recovery, passkeys,
devices, full guild administration, and every other action the identity
itself is allowed to take, not gated by any capability grant at all.

The two types share no conversion in either direction, in any SDK — an
integrator credential can never yield account-level power, by
construction, not just by convention. Within `AccountSession`, most
actions stay authorized by the session's ambient bearer token; a smaller,
named set additionally requires a fresh Ed25519 signature from the
identity's own locally-held signing key, minted automatically by the SDK
— see [identity.md](../../backend-server/architecture/identity.md)'s "Two authorization tiers" section
for the full model and rationale, and "Today in the repo" below for the
per-language `AccountSession` surface and the complete signature-required
endpoint list.

## Languages

Rust is the reference implementation (both `Session` — integrator — and
`AccountSession` — first-party). C# and TypeScript are also implemented and
fully supported. See [`../language-support.md`](../language-support.md)
for the full, canonical table — every implemented language with its
detailed status, and every language without an SDK yet, kept in one place
rather than duplicated across this doc and the project README.

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
  [synchronization](../../backend-server/architecture/synchronization.md)) — `SyncJournal` trait
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
  - `../../backend-server/architecture/identity.md`'s canonical
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
- `crates/sdk/build.rs` (issue #724, epic #722) — `sdk` now consumes
  generated wire-shape types for the whole `crates/sdk/src/account/*`
  domain (registration/login, profile, passkeys, devices/grants/cross-device
  pairing, recovery, conversations, friends/blocks/presence/discovery/
  history, and full guild administration): `typify` converts a hand-picked
  allowlist (`build.rs`'s `SCHEMA_NAMES`, ~85 entries) of
  `docs/generated/openapi.json`'s (#723) `components.schemas` into Rust
  types at build time, written to `OUT_DIR` and pulled in via
  `src/generated.rs`'s `include!`. `sdk` reads `openapi.json` as a plain
  checked-in file (not a Cargo dependency on `avalon-server`, which already
  depends on `avalon-sdk` — that edge can't run the other way).
  - Not migrated, called out rather than silently absorbed: the four
    WebAuthn-ceremony request/response types (`RegisterStartResponse`,
    `RegisterFinishRequest`, `SessionStartResponse`, `SessionFinishRequest`,
    plus `passkeys.rs`'s `AddPasskeyStartResponse`/`AddPasskeyFinishRequest`)
    stay hand-written — their `challenge`/`credential`/`webauthn_credential`
    fields are opaque `"type": "object"` blobs in the schema (`webauthn-rs`'s
    own types have no `ToSchema` impl), and typify would flatten them to
    `serde_json::Value`, discarding the real `passkey_types::webauthn::*`
    typing the ceremony code depends on. Pure signature-only wrapper bodies
    (just `signing_key_id`/`signature`, e.g. `RevokePasskeyRequest`,
    `ConnectRequest`) also stay hand-written, composed via the existing
    `super::SignatureFields` + `#[serde(flatten)]` pattern — every *other*
    signature-carrying request (role/override/member/ownership mutations,
    device-pairing approval, guardian changes, ...) is generated instead,
    with `signature`/`signing_key_id` destructured out of `SignatureFields`
    at each call site rather than flattened.
  - `UpdateProfileRequest`'s three-state `PATCH /me` semantics (omitted =
    untouched, `Some("")` = clear, `Some(v)` = set) turned out to migrate
    cleanly: every property is optional, and typify emits a single
    `Option<T>` per field with `#[serde(skip_serializing_if =
    "Option::is_none")]` by default — exactly the hand-written struct's own
    behavior, no nested `Option<Option<T>>` needed. The same three-state
    convention recurs in `UpdateGuildRequest`/`UpdateChannelRequest`, same
    treatment.
  - `Genre`, `PresenceStatus`, `GuildLink`, `RoleBadge` are replaced with the
    existing `avalon_protocol::{identity,social,guilds}::*` types via
    typify's `with_replacement`, not generated as duplicates.
  - typify hardcodes `"format": "date-time"` to `chrono`, not a dependency
    anywhere in this workspace (`time` is, everywhere) — `build.rs` strips
    that format hint before generation so those fields come through as plain
    `String`s, parsed/formatted with `time`'s own RFC3339 support
    (`account::parse_rfc3339`/`format_rfc3339`) at the call sites that need
    them, rather than adding a second date/time crate.
  - Migrating surfaced a handful of real, pre-existing drift between the
    hand-written SDK and the actual server responses, found rather than
    introduced by this migration, fixed where safe and otherwise called out
    at the affected struct: `social::DiscoveryCandidate` only ever carried
    `identity_id` server-side, but the old hand-written type declared
    `display_name` as non-optional — every real `discover_people()` call
    would have failed to deserialize; fixed to match the real (narrower)
    shape. `guild_admin::Guild`/`GuildEvent`/`UpdateGuildRequest`'s consumer
    (`GuildUpdate`) don't yet expose several fields the real schemas already
    carry (`member_count`/`integrators`/`game_breakdown_public`/
    `favorite_games`/`roster_visibility` on `Guild`; `details_visible`/
    `my_rsvp` on `GuildEvent`; `join_policy`/`links`/`roster_visibility`/
    `game_breakdown_public` on the update path) — left as documented gaps,
    not silently expanded, since adding new public surface wasn't this
    migration's scope.
  - `build.rs` also generates endpoint-path stub constants (the other half
    of #724's original design, not done in the first pass) — one `&str`
    template per `(tag, operationId)` pair, covering the *whole* published
    API (not just the `SCHEMA_NAMES` allowlist — unlike type generation, a
    path template carries no format-mapping risk, so there's no reason to
    hand-curate a second allowlist in lockstep with the first), under
    `crate::generated::paths::<tag>::<OPERATION_ID>`. Bare `operationId`
    collides across tags (`list_messages`/`send_message`/`register_start`/
    `register_finish` each name two different real endpoints under two
    different tags), so each tag gets its own module; confirmed
    `(tag, operationId)` is unique across the whole spec. Every
    hand-written `format!("/guilds/{guild_id}/roles")`-style path across
    `crates/sdk/src/account/*.rs` (~70 call sites) now references one of
    these constants instead. Path templates keep the server's own
    `{param}` placeholder names, which don't always match this SDK's local
    variable names (`/guilds/{id}/channels/{cid}` vs.
    `guild_id`/`channel_id`) — `format!()` requires a string *literal*, so
    a runtime `&str` constant can't be fed into it regardless of naming,
    hence `account::path(template, &[(name, value), ...])`
    (`crates/sdk/src/account/mod.rs`), a small runtime `{name}`
    substitution helper every call site uses instead.
  - #725 (`bindings/csharp`) and #726 (`bindings/ts`) — same migration for
    the other two SDKs, following this pattern — are follow-on work, not yet
    started.
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
    scoping decision, not an oversight — this file (this
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
  - **Unit tests** (`vitest`, under `bindings/ts/test/`, mirroring
    `src/`'s own directory structure rather than colocated next to the
    code they cover — a dedicated tree, not scattered through `src/`):
    the canonical-message shape byte-for-byte against the known format
    (`test/crypto/signing.test.ts`), `AccountSession.sign`'s empty-vs-signed
    behavior, `IntegratorSession`'s capability-gating (throws without a
    network call when ungranted), and a signed-call round trip
    (`test/accountSession/core.test.ts`) that stubs `fetch`, calls a
    signature-required method, and verifies the captured request body's
    signature against the session's known public key using
    `@noble/curves`'s own `ed25519.verify` — the same "verify server-side-
    equivalently" pattern the C# unit tests use with BouncyCastle.
  - **Live tests** (`bindings/ts/test/account.live.test.ts`, opt-in via
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
  - **Epic #712 stage 1**: three `AccountSession` capabilities `apps/hub`
    needs that #701 didn't build, added so the later Hub-migration stage
    (#712's own later work, `apps/hub` untouched by this stage) has
    something functionally complete to migrate onto:
    - `bindings/ts/src/accountSession/realtime.ts` — `subscribePresence`
      (`GET /ws/presence`, issue #136) and `subscribeChannelMessages`/
      `subscribeConversationMessages` (`GET /ws/messages`, issue #438),
      ported from `packages/api-client/src/client.ts`'s
      `openPresenceSocket`/`openChannelMessageSocket`/
      `openConversationMessageSocket`, same shapes (a plain browser
      `WebSocket`, additive `subscribe(ids)` queued until `open` for
      presence, the `node_info`-then-`subscribe_channel`/
      `subscribe_conversation` handshake for chat). The chat handshake
      includes issue #610's signed DHT interest claim, minted via the new
      `bindings/ts/src/crypto/interestClaim.ts` (mirroring
      `packages/api-client/src/crypto/interestClaim.ts` byte-for-byte)
      whenever this session holds a local signing key and the server
      offers a `base_url` — `AccountSession` already holds
      identityId/signingKeyId/secretKey in memory, so no storage-adapter
      lookup is needed the way the Hub reference needs one.
      `subscribeChannelMessages` also takes an optional `onDeleted`
      callback firing with a message id on a moderation delete, matching
      the reference's `openChannelMessageSocket` shape exactly;
      conversations have no equivalent (no moderation-delete endpoint).
    - `bindings/ts/src/crypto/continuation.ts` — session-continuation
      token minting (issue #525), mirroring
      `packages/api-client/src/crypto/continuation.ts`'s wire format
      exactly (`avalon:continuation:v1:<identity_id>:<signing_key_id>:
      <nonce>:<issued_at_secs>:<expires_at_secs>`, wire-encoded as
      `AVCT1.<base64url-no-pad-json>`). Wired into `bindings/ts/src/
      http.ts`'s `request()` via a new optional `onUnauthorized` hook on
      `RequestOptions` — `AccountSession`'s own internal `get`/`post`/
      `patch`/`put`/`del`/etc. helpers pass a hook that mints a
      continuation token from the session's own in-memory signing key on
      a 401 and retries exactly once, never persisting the result back
      into the session's own token (single-use by design). `Integrator
      Session`'s call sites never pass this hook — an integrator
      credential has no local signing key to reconnect from, so its 401s
      propagate unchanged, same as before this stage.
    - `bindings/ts/src/ledger.ts` — a free-standing `getLatestSth(server
      Url)` (not a session method — `GET /ledger/sth/latest` is public,
      unauthenticated) plus the `SignedTreeHeadResponse` wire type in
      `bindings/ts/src/types.ts`, matching `crates/server/src/
      settlement.rs::SignedTreeHeadResponse` field-for-field. Hub's own
      `apps/hub/src/network/verifyNetwork.ts` (issue #232) keeps its STH-
      based network-trust verification logic — this only supplies the
      fetch and the wire shape.
    - `bindings/ts/src/crypto/mnemonic.ts` + `AvalonClient.registerWithMnemonic`/
      `AccountSession.attachSigningKey` — found scoping the *next* stage
      (the actual Hub migration), not in the original stage-1 pass:
      `AvalonClient.register()` generated a purely random signing key with
      no recovery phrase at all, which would have silently dropped a real,
      already-shipping feature (`packages/api-client`'s `createIdentity`,
      issue #134's BIP39-mnemonic disaster-recovery fallback) had Hub
      migrated onto it as-is. `deriveSigningKeyFromMnemonic`/
      `generateMnemonicSigningKey`/`isValidMnemonic` reimplement
      `packages/api-client/src/crypto/signingKey.ts`'s derivation exactly
      (same `DERIVATION_LABEL`, same BIP39-seed-then-SHA256 scheme) — a
      phrase recovered through this SDK derives the identical key a
      Hub user already has. `registerWithMnemonic` is `register()`'s
      mnemonic-backed sibling, returning `{ session, mnemonic }`;
      `AccountSession.attachSigningKey(secretKey)` resolves an
      already-derived key's server-side `signing_key_id` (`GET
      /me/devices`, matched by public key) and attaches it to an
      *already-built* session in place — the "this browser has no signing
      key stored for me, the user just typed their recovery phrase" path,
      distinct from constructing a whole new session. Storage of the
      phrase/key stays Hub's own concern, same as every other credential-
      persistence decision in this SDK. Unit-tested
      (`crypto/mnemonic.test.ts`, `accountSession/core.test.ts`); not
      live-tested, same already-documented gap `register()`/`login()`
      have (no WebAuthn-ceremony-driving authenticator available in
      Node/vitest) — `registerWithMnemonic` shares that ceremony step, so
      inherits the same limitation rather than introducing a new one.
    - `AvalonClient.loginWithIdentityId(identityId)` — found migrating
      `apps/hub`'s actual `Login.vue`/`CreateIdentity.vue` (epic #712's
      later stage), not by the earlier function-by-function audit: `login()`
      requires `AccountCredentials` (pre-existing local key material from
      an earlier `register()`/`login()` on this same browser), but Hub's
      real login flow supports any device with a registered passkey for an
      identity logging in — including one with *no* local signing key at
      all yet (awaiting a device-grant approval from another device).
      `loginWithIdentityId` drives the same WebAuthn ceremony with no key
      material, resolving to a session with `signingKeyId() === undefined`
      — call `AccountSession.attachSigningKey` afterward if this browser
      separately holds a key (mnemonic-derived or otherwise) to attach.
      Same live-testing gap as `register()`/`login()`, for the same reason.
    - **Unit tests**: `crypto/continuation.test.ts` covers the wire format
      byte-for-byte (mirroring `packages/api-client/src/crypto/
      continuation.test.ts`'s own test style: prefix, field shapes, exact
      60-second TTL, the exact signed-bytes format, tamper detection,
      nonce freshness). `http.test.ts` covers the `onUnauthorized` retry
      mechanics against a stubbed `fetch` (retries once, never persists,
      propagates when the hook returns `null` or is absent, never fires
      for a non-401). `accountSession/core.test.ts` adds the same round
      trip through a real `AccountSession` (mints from its own in-memory
      key, retries, `token()` unchanged afterward; propagates as-is with
      no local key). `accountSession/realtime.test.ts` covers what's
      testable without a real server against a stubbed `WebSocket`:
      subscribe-before-open queuing, the `node_info` handshake firing
      `subscribe_channel`/`subscribe_conversation` at most once, claim
      minting/omission depending on local key and server-offered
      `base_url`, and camelCase message mapping — a real server pushing a
      live update is left to the live suite below, not faked here.
    - **Live tests** (`account.live.test.ts`, run 2026-09-21 against this
      sandbox's real `avalon-server`/Postgres via `npm run test:live`):
      a presence-subscribe round trip (one identity subscribes, a second
      identity's `PUT /me/presence` update arrives pushed over the
      socket, confirmed against the same `indexer_friendships` projection
      the server's own friend-visibility check reads, not just
      `friendships`); a channel-message subscribe round trip (a sent
      message arrives pushed over the socket, then a moderation delete
      arrives via `onDeleted`); and a `getLatestSth()` round trip against
      the real `GET /ledger/sth/latest`. All pass.
      A live continuation-reconnect test (retrying against a *second*
      node the original session token isn't valid on) was attempted
      against `avalon-peer` — reachable and network-adjacent, genuinely
      running its own separate Postgres, the actual topology this test
      needs. The initial `PATH` gap (the Rust toolchain isn't on a
      non-interactive `ssh host 'command'` session's `PATH` there) was
      fixed and `avalon-peer` brought up to this same commit
      successfully, but `crates/server/src/continuation.rs::verify`
      resolves a continuation token's signing key by the exact
      `indexer_identity_signing_keys.signing_key_id` — a UUID that has to
      match byte-for-byte across two genuinely independent Postgres
      instances for the test to mean anything (not just the same
      `identity_id`/public key). Hand-seeding that consistently across
      both databases for one test was judged not worth it relative to
      what it would add: the minting logic already has full byte-format
      unit coverage (`crypto/continuation.test.ts`), a mocked-fetch round
      trip through a real `AccountSession` proves the retry wiring itself
      works end-to-end (`core.test.ts`), and the wire format is an exact
      port of Hub's own already-shipping, production-proven
      `packages/api-client` implementation. What's specifically
      unverified live is only the cross-node network hop itself.
  - **Epic #712, final SDK-extension pass**: an audit comparing every
    exported function in `packages/api-client/src/client.ts`/`identity.ts`/
    `session.ts` against `bindings/ts` after the stage-1 pass above (and the
    same day's websocket-subscription/continuation-reconnect/
    network-trust-fetch/BIP39-mnemonic work) found fourteen remaining gaps,
    all closed here — `bindings/ts` now has full feature parity with
    `packages/api-client`'s public surface, and the actual `apps/hub`
    migration onto it (untouched by this pass) can begin as #712's next
    stage:
    - Three `AccountSession` additions: `getMyAchievements()` (`GET
      /me/achievements?limit=200`, `bindings/ts/src/accountSession/
      achievements.ts`, a new domain file) — the player's own read of their
      attestation history (active and revoked alike), distinct from
      `IntegratorSession.achievements()`, which is capability-gated and
      issuance-focused, and deliberately without a `recognition` field per
      ADR #76; `getGameBreakdown(guildId)` (`GET
      /guilds/{id}/integrator-breakdown`) and `getMessageArchive(guildId,
      channelId, before?, limit?)` (`GET /guilds/{id}/channels/{channelId}/
      messages/archive`), both added to the existing
      `bindings/ts/src/accountSession/guildAdmin.ts` as siblings of
      `channelMessages()`.
    - Eleven free-standing, unauthenticated functions, matching
      `ledger.ts`'s existing `getLatestSth(serverUrl)` convention (no
      session, `serverUrl` as the first parameter), split across four new
      files: `integratorDirectory.ts` (`listAchievementDefinitions`,
      `listMilestoneDefinitions`, `getIntegrator`, `listIntegrators`,
      `getIntegratorRegistry`, `listIssuerKeys` — all under
      `/integrations`); `identityData.ts` (`getIdentityIntegratorData`,
      `GET /identities/{id}/integrator-data`); `recovery.ts` (the five
      recovery-initiation calls — `startRecoveryRequest`/
      `finishRecoveryRequest`/`getRecoveryRequest`/
      `finalizeRecoveryRequest`/`getIdentityRecoveryStatus` — deliberately
      free-standing rather than `AccountSession` methods, since the caller
      has no session yet for the identity being recovered, reusing
      `accountSession/recovery.ts`'s own `RecoveryRequest`/`requestFromWire`
      rather than duplicating that conversion); and `crossNodeLogin.ts`
      (epic #623's `lookupCrossNodeLogin`/`denyCrossNodeLogin`/
      `submitCrossNodeLoginGrant`, all taking an explicit `baseUrl` target
      rather than the session's own configured server, matching
      `packages/api-client`'s own three cross-node functions). The new
      `bindings/ts/src/crypto/crossNodeLogin.ts` mints a
      `CrossNodeLoginGrant` locally (mirroring
      `packages/api-client/src/crypto/crossNodeLogin.ts`'s
      `avalon:cross-node-login:v1:<identity_id>:<signing_key_id>:
      <destination_base_url>:<requesting_context>:<nonce>:<issued_at_secs>:
      <expires_at_secs>` signing-bytes format and hex-encoded signature
      byte-for-byte, the same `DEFAULT_TTL_SECONDS = 60`, same convention
      `crypto/continuation.ts`/`crypto/interestClaim.ts` already use).
      `submitCrossNodeLoginGrant` takes raw
      `identityId`/`signingKeyId`/`signingKeySecret` rather than an
      `AccountSession`, matching how `apps/hub`'s own `CrossNodeLogin.vue`
      call site already needs to shape this call: the approving browser's
      ambient session, if any, has nothing to do with the destination node.
    - **Unit tests**: a wire-shape/conversion test each for
      `getGameBreakdown`/`getMessageArchive`
      (`accountSession/guildAdmin.test.ts`, new) and `getMyAchievements`
      (`accountSession/achievements.test.ts`, new, including an
      empty-history case). `crypto/crossNodeLogin.test.ts` mirrors
      `crypto/continuation.test.ts`'s own style: exact field shapes, the
      60-second TTL, the exact signed-bytes format (hex, not base64),
      tamper detection, nonce freshness.
    - **Live tests** (run 2026-09-21 against this sandbox's real
      `avalon-server`/Postgres, seeded the same way
      `account.live.test.ts` does — direct SQL inserts rather than a
      WebAuthn ceremony): `getMyAchievements()` on a freshly seeded
      identity returns `[]`; `getIntegrator`/`listIntegrators` round-trip
      against a real integrator registered directly via `POST
      /integrations`; `getMessageArchive` round-trips (empty, since a
      freshly sent message hasn't aged into the archive tier yet — the
      endpoint itself is confirmed reachable and correctly shaped). The
      five recovery-initiation functions and the three cross-node-login
      functions were not live-verified: recovery needs guardians
      configured first, and cross-node login needs a destination node
      genuinely distinct from the one the grant is minted against — both
      left to unit coverage plus the byte-for-byte format match against
      their already-live-proven `packages/api-client` counterparts.
- **Epic #712, `apps/hub` migration off `packages/api-client` onto
  `bindings/ts` — done.** `bindings/ts` now has a real first-party
  consumer: `apps/hub`'s entire data-fetching surface runs on
  `@avalon/sdk`, and `"@avalon/api-client"` is gone from
  `apps/hub/package.json` — `grep -rl "@avalon/api-client" apps/hub/src`
  returns nothing. Migrated batch by batch (auth-adjacent views;
  Profile/passkeys/devices/recovery; friends/blocks/discovery/presence;
  guilds/chat/events; conversations; integrations/connections;
  achievements/notifications/activity; network trust; `NetworkStatus.vue`
  plus an out-of-plan `UserProfile.vue` gap found late), each batch
  live-verified against a real `avalon-server`/Postgres via a real
  browser (Playwright + a CDP virtual WebAuthn authenticator), not just
  unit-tested. Found and fixed a recurring real `bindings/ts` gap along
  the way, not just Hub-side renames: nearly every list-returning
  `AccountSession`/free-standing method crashed via `.map()` on a
  non-array wire response instead of passing it through for the caller's
  own `Array.isArray` check (now fixed across
  `passkeys.ts`/`devices.ts`/`recovery.ts`/`social.ts`/
  `guildAdmin.ts`/`conversations.ts`/`integrations.ts`/`identityData.ts`/
  `integratorDirectory.ts`/`achievements.ts`), plus one real missing-field
  gap (`PublicIdentityProfile`/`identityProfile()` was missing
  `mainGuild`/`effectiveMainGuild`, which the real server always
  returns). `apps/mobile-hub` stays on `packages/api-client`, untouched
  — explicitly out of this epic's scope.
- `bindings/ts/scripts/generate-types.mjs` (issue #726, epic #722 — full
  migration, mirroring #724's Rust migration) — `bindings/ts` consumes
  generated wire-shape types from `docs/generated/openapi.json` (#723) via
  `openapi-typescript`, a devDependency-only tool (zero runtime footprint —
  the generated file itself has no imports). Unlike the Rust SDK,
  `bindings/ts` has **no build step at all** (`package.json`'s `main`/
  `types` point straight at `./src/index.ts`; `apps/hub` depends on it via
  `"@avalon/sdk": "file:../../bindings/ts"`, consuming raw source), so
  there's no compile-time hook the way `crates/sdk/build.rs` gets — the
  generated `bindings/ts/src/generated.ts` (whole-spec, not a curated
  allowlist like Rust's `SCHEMA_NAMES`, since unused TS interfaces cost
  nothing at runtime) is instead a **checked-in artifact**, same
  convention `docs/generated/openapi.json` itself already uses: `make
  ts-sdk-types` regenerates it, `make ts-sdk-types-check` (wired into `make
  check`) fails CI if it's stale.
  - The real spec has real `operationId` collisions across tags
    (`list_messages`/`send_message` for chat vs. guild channels,
    `register_start`/`register_finish` for identity vs. passkeys — the
    same four collisions #724's own `build.rs` found and resolved via
    per-tag module namespacing) — `openapi-typescript`'s `operations`
    namespace is flat with no such namespacing, so generating from the
    unmodified spec produces a `generated.ts` that doesn't even
    type-check. `generate-types.mjs` calls `openapi-typescript`'s
    programmatic API (not its CLI, which only takes a file path) and
    prefixes every colliding `operationId` with its own first tag before
    generation — this only touches the (not yet used) `operations`/`paths`
    naming, never `components.schemas`, which has no collisions since
    every schema name is already globally unique.
  - Every hand-written wire-shape type across `types.ts`, `client.ts`, and
    all of `accountSession/{core,passkeys,devices,deviceLogin,integrations,
    recovery,conversations,social,achievements,guildAdmin}.ts` (48 alone in
    `guildAdmin.ts`) now aliases a generated `components['schemas'][...]`
    type in place of its old hand-written interface; the wire/domain split
    and each `fromWire`-style mapping function's own shape are otherwise
    untouched, same discipline #724 used. `accountSession/realtime.ts`
    stays entirely hand-written and correctly so — WebSocket push payloads
    have no OpenAPI coverage at all (utoipa doesn't model raw socket
    upgrades). Opaque WebAuthn-ceremony blob fields (`RegisterStartResponse`/
    `SessionStartResponse`/`RecoveryStartResult`'s `challenge`) also stay
    hand-written, the same reason the Rust SDK keeps its own equivalents
    hand-written.
  - This migration found the same `DiscoveryCandidate` bug independently
    rediscovered from #724's own Rust migration: the real server only ever
    sends `identity_id` on discovery results; the old hand-written type
    additionally declared `displayName`/`avatarUrl`/`mutualFriends`/
    `mutualGuilds`, fields that were always `undefined` in practice (`apps/
    hub`'s own `src/api/discovery.ts` already worked around this
    defensively, re-fetching display names separately via `profiles()`).
    `DiscoveryCandidate` is now just `{ identityId: string }`.
  - This migration also found — and fixed at the source — a real
    already-merged schema bug: `crates/server/src/conversations.rs`'s
    `MessageResponse` (`conversation_id`) and `crates/server/src/
    guild_messages.rs`'s `MessageResponse` (`channel_id`) both registered
    as the bare name `MessageResponse` in `crates/server/src/openapi.rs`'s
    schema aggregator; utoipa silently let the second-registered one win,
    so the published schema for `/conversations/{id}/messages` actually
    described the *guild channel* shape. This was already live on `main`
    via #724's own (also-affected) Rust migration. Fixed by giving the
    conversations struct its own registered name —
    `#[schema(as = ConversationMessageResponse)]` — regenerating
    `docs/generated/openapi.json`, and propagating the rename through both
    SDKs (`crates/sdk/src/account/conversations.rs`,
    `bindings/ts/src/accountSession/conversations.ts`), each with a new
    permanent live regression test
    (`account_session_conversation_message_round_trip` in Rust,
    the parallel `AccountSession conversation message live round trip
    (issue #726)` suite in TS) rather than a one-off probe.
  - Several generated fields are optional (`T | null | undefined`) where
    the hand-written wire types declared them non-optional (`T | null`) —
    every affected `fromWire`-style mapping function now coalesces with
    `?? null` (or `?? []`/`?? false` where appropriate); a few generated
    fields are typed as plain `string` where the domain type expects a
    narrower string-literal union (`RsvpStatus`), handled with an explicit
    cast at the mapping boundary. No behavior change in either case: the
    real server has always sent these fields/values on every real
    response.
  - Endpoint-path stubs (mirroring #724's
    `crate::generated::paths::<tag>::<NAME>`) are a deliberate scope
    exclusion, not a gap: TS has no `format!()`-style compile-time-literal
    restriction forcing string paths through a constant, template-literal
    call sites already interpolate variables correctly today, and
    generating a parallel constants module would add indirection without
    fixing a real problem the way it did for Rust.
  - `apps/hub`'s full test suite (455 tests) and production build were
    verified unaffected; the zero-Vue/Pinia-dependency invariant holds
    (`openapi-typescript` is a devDependency only). Full Rust workspace
    test suite (`cargo test --workspace`) and `make openapi-check` both
    verified clean after the schema fix.
- Schema/SDK versioning (issue #735, epic #722) — `docs/generated/
  openapi.json`'s `info.version` (a static `"0.1.0"` since #723) now has
  real enforcement behind it: `make openapi-version-check` (new, part of
  `make check`) diffs the checked-in schema against the same file at
  `origin/main` (falling back to local `main`) with `info.version` itself
  stripped from both sides — if the rest of the document differs but the
  version field doesn't, it fails and points at the literal in
  `crates/server/src/openapi.rs` to bump (`scripts/
  check-openapi-version.sh`). `make openapi-check` (#723, staleness) and
  this check (shape-change-without-a-bump) are deliberately separate
  targets — one catches "you forgot to regenerate," the other catches
  "you regenerated but didn't bump."
  - Both codegen pipelines now embed that same version so a build can
    report which schema it targets: `crates/sdk/build.rs` emits
    `pub const OPENAPI_SCHEMA_VERSION: &str` straight from the same
    `doc["info"]["version"]` it already reads for type generation
    (re-exported from `crates/sdk/src/lib.rs`, through the
    otherwise-`pub(crate)` `generated` module), and `bindings/ts/scripts/
    generate-types.mjs` appends an `export const OPENAPI_SCHEMA_VERSION`
    to the end of `generated.ts` (re-exported from `bindings/ts/src/
    index.ts`). Both are generated directly from the schema file, not
    hand-copied, so they can't drift from it independently — each SDK has
    a small permanent test (`crates/sdk/tests/schema_version.rs`,
    `bindings/ts/test/schemaVersion.test.ts`) asserting the constant
    matches `docs/generated/openapi.json`'s own `info.version` read fresh
    off disk, to catch a future change to the generation step itself
    breaking that link.
  - `bindings/csharp` (#725) doesn't have this yet — its own typify-
    equivalent codegen migration hasn't landed, so there's nothing to
    embed the constant into. Revisit once #725 lands, per #735's own
    acceptance criteria.
  - No server-side compatibility *enforcement* (an SDK declaring its
    schema version in a request header, checked or logged server-side) —
    #735's own design section calls this out as a separate, bigger
    compatibility-policy question than this ticket's plumbing, not
    silently decided here.

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
- [#696](https://github.com/LunarVagabond/avalon-protocol/issues/696) — Epic:
  unify the SDK schema across languages with tiered action signing (decided
  on [#695](https://github.com/LunarVagabond/avalon-protocol/issues/695)):
  [#699](https://github.com/LunarVagabond/avalon-protocol/issues/699) Rust
  `AccountSession`, [#700](https://github.com/LunarVagabond/avalon-protocol/issues/700)
  C# `AccountSession`, [#701](https://github.com/LunarVagabond/avalon-protocol/issues/701)
  new TypeScript SDK,
  [#707](https://github.com/LunarVagabond/avalon-protocol/issues/707)
  `AccountSession` device-pairing login (found after #699 shipped, folded
  into #700/#701 from the start). See [identity.md](../../backend-server/architecture/identity.md)'s own
  "Decisions and tickets" for the companion tier-classification/
  enforcement tickets (#697/#698/#704).
- [#712](https://github.com/LunarVagabond/avalon-protocol/issues/712) —
  Epic: migrate `apps/hub` off `packages/api-client` onto `bindings/ts`.
  Stage 1 (this stage): extend `AccountSession` with presence/chat
  WebSocket subscriptions, issue #525's continuation-token reconnect, and
  `getLatestSth()`, so the surface is functionally complete before Hub
  itself is touched in a later stage.
