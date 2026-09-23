# SDK

**Avalon exposes protocol capabilities, not infrastructure.** A developer
thinks in identity, guilds, achievements, presence, and integrator event
verification — never in Postgres instances, chain RPCs, indexer shards, or
node addresses. **Every capability-gated method checks its own required
grant**; the SDK never trusts the caller. Narrative:
[Proposal: SDKs](../../../stakeholders/Proposal.md#sdks).

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
consume exactly those. See [Known limitations](#known-limitations) below for
how far the current SDKs are from this target today.

## What the SDK abstracts

| Concern | Hidden from the integrator |
|---|---|
| node discovery and selection | latency, proximity, capabilities, health |
| authentication | identity session exchange, integrator credential |
| protocol version and capability negotiation | which node roles are reachable |
| retries, failover, routing | a node disappearing |
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

`Capability` (`crates/protocol/src/permissions.rs`) is an enum with a
permanent-string mapping, not a bare `String`: `Capability::KNOWN` lists
every known variant, `as_str()`/`Display` give the wire string each one
(de)serializes as, and `Other(String)` preserves any capability string this
build doesn't know about yet rather than erroring — the wire string, not the
Rust variant name, is the permanent identifier. The starting capability list
is in [Proposal: Permission Model](../../../stakeholders/Proposal.md#permission-model).

## Error handling and retries

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

**The stable code is always reachable.** Every SDK exposes the server's
`code` so a caller can tell apart failures that share one status (for
example a not-reversible versus an already-reversed rollback, both `409`):
Rust carries it as the variant's message, C# as
`AvalonRequestException.Code`, TypeScript as `AvalonSdkError.code`. Only the
code is exposed, never the server's reworded prose.

**Retries.** Connection errors, timeouts, and 502/503/504 — the transient
class a node hiccup produces — are retried with exponential backoff and
full jitter, *but only for a call that opts in as idempotent*: every read
opts in automatically; a write opts in only when it carries something
that makes a retry provably safe — an `Idempotency-Key` header the server
honors (achievement issuance, the concrete case wired up so far — see
below), or an endpoint-specific dedup key a write already had for other
reasons (`ConversationHandle::send`'s `client_entry_id`), or the write's own
HTTP method already guarantees it (`PUT /me/presence`). A write with none of
those gets exactly one attempt — retrying it blind risks applying it twice.
`AvalonConfig::retry` (`RetryConfig { max_retries, base_delay,
request_timeout }`) tunes this per integration; `RetryConfig::default()`
is 3 retries, 200ms base, 10s per-request timeout.

**Idempotency-Key coverage is intentionally partial today.** Only
`Session::issue_achievement` carries one — the write where a duplicate is
worst (a second, spurious attestation). Every other unkeyed write (`dm`,
`ConversationHandle::send` with no `client_entry_id`, `AvalonClient::login`,
`Session::publish_schema_version`/`publish_instance`) stays single-attempt
rather than being retried unsafely; extending real key coverage to those is
a known gap, not silently assumed done.

## Two session types: capability-gated integrator vs. first-party account

Everything above (`Session`, capability checks, `Capability`) describes
the capability-gated *integrator* model — a game/app/service authenticating
with its own credential, scoped to whatever an identity explicitly
granted it. Every language SDK also implements a second, entirely separate
session type: `AccountSession`, a first-party client for an identity's own
account — registration, login, recovery, passkeys, devices, full guild
administration, and every other action the identity itself is allowed to
take, not gated by any capability grant at all.

The two types share no conversion in either direction, in any SDK — an
integrator credential can never yield account-level power, by
construction, not just by convention. Within `AccountSession`, most
actions stay authorized by the session's ambient bearer token; a smaller,
named set additionally requires a fresh Ed25519 signature from the
identity's own locally-held signing key, minted automatically by the SDK
— see [identity.md](../../backend-server/architecture/identity.md)'s "Two authorization tiers" section
for the full model and rationale.

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

The Rust SDK physically lives in the `avalon-sdks` repo (`languages/rust/`) rather
than `crates/sdk` in this repo; `crates/cli` reaches it via a real git
dependency, not a workspace path. It has **no dependency on any other
crate in this workspace** — not `avalon-protocol`, not `avalon-chain`, not
`avalon-server` — so it is structurally the same kind of thing the C# and
TypeScript SDKs already were: schema-generated wire types plus a
hand-written client layer, with its own copies of the domain types
(`rust/src/types/` in `avalon-sdks`) and its own implementations of every
signing-byte construction it needs (`achievements.rs`, `sth.rs`,
`cross_node_login.rs`).

That independence is a deliberate trade with a real cost. Before it, the
Rust SDK called `avalon_protocol::achievements::*` and
`avalon_protocol::sth::*` directly, so client and server were
*compiler-guaranteed* to sign the same bytes — drift was not expressible.
Now it is, exactly as it always has been for C# and TypeScript. What
replaces the compiler is the conformance suite: `conformance/vectors/`
holds the canonical signing-byte and signature vectors,
`crates/protocol/tests/conformance.rs` asserts the server's
implementations against them, and each SDK's own runner asserts its
implementation against the same files. A format change that isn't mirrored
everywhere fails a test on whichever side moved. **Anything in an SDK that
signs bytes must be covered by a vector** — that is the price of this
layout, not an optional extra.

### Rust SDK surface

`AvalonClient::new(AvalonConfig { server_url, integrator_credential_key_id,
integrator_slug, signing_key })` and `authenticate(identity_token)` are
wired to a real `avalon-server` (`GET /me`, `GET /me/grants`).
`Session::require(capability)` is the per-method check.
`integrator_slug`/`signing_key` are `Option`s: `None` for a read-only
integration, required by `issue_achievement`. A `test-util`-gated
`Session::grant_for_testing(capability)` exists for tests that want a
granted `Session` without driving the full consent flow.

- **Achievements** — `Session::achievements()` (`achievements.read`) lists
  the identity's own full attestation history across every issuer;
  `Session::issue_achievement(key)` (`achievements.issue`) signs an
  attestation locally and submits it, carrying an `Idempotency-Key`. Both
  are live, not stubbed. `VerifiedAttestation` carries
  `authenticity`/`validity`/`history` as the server computed them —
  deliberately no `recognition` field; an integrator wanting a recognition
  verdict filters through its own policy. Milestones (the App/Service
  equivalent of achievements), definition CRUD, and bulk issuance/revocation
  are also covered.
- **Social** — `Session::friends()` (`friends.read`), `presence()`/
  `presence_of(&[IdentityId])` (`presence.read`), and
  `update_presence(status)` (a user publishing their own status, no
  capability required). `friends()` embeds each friend's `Presence` only
  when `presence.read` is also granted, via one batched `presence_of` call.
  `Friend.display_name` is always `None` today — no endpoint resolves
  another identity's profile yet. `presence_of` is scoped server-side by
  each subject's own visibility setting.
  `Session::subscribe_presence(&[IdentityId])` opens a live-push websocket
  channel, additive to the point-in-time reads.
- **Guilds** — `Session::guilds()` (`guilds.read`) and `Session::guild(id)`,
  a `GuildHandle` with `roster()` (`guilds.read`) and `channels()`
  (`guilds.chat`), plus `GuildHandle::channel(cid)`, a `ChannelHandle` with
  `messages(before, limit)` and `send(body)` (both `guilds.chat`).
  `roster()` embeds presence the same way `friends()` does. Guild
  `channels()`/`messages()` apply no per-channel visibility scoping yet —
  see Known limitations. Creating guilds, inviting, kicking, changing
  roles, and managing channels are deliberately not on this session type —
  identity-authority-only actions taken through the Hub, not something an
  integrator can do on a player's behalf.
- **Conversations** — `Session::conversations()` (`messages.read`) and
  `Session::conversation(id)`, an ungated `ConversationHandle` with
  `messages(before, limit)` (`messages.read`) and `send(body)`
  (`messages.send`). `Session::dm(other_identity_id)` (`messages.send`)
  creates-or-gets a 1:1 conversation. A rejected read or send — whether the
  caller was never a participant or is a blocked one — surfaces as the same
  `SdkError::NotConversationParticipant` either way, so the SDK never
  reveals more than the server does.
- **Device and cross-node login** — `AvalonClient::login()` wraps
  cross-device pairing for a client with no WebAuthn surface of its own (a
  game engine, a console), returning a `DeviceLogin` with a
  `user_code`/`verification_uri` and a `wait()` that polls to completion.
  `AvalonClient::cross_node_login()` is the same shape for logging in from
  a different node than the one an existing session lives on, plus a
  same-device fast path (`submit_cross_node_login_grant`) that signs and
  submits a grant directly when the caller already controls the identity's
  key material.
- **Sync journal** — a `SyncJournal` trait
  (`append`/`pending`/`all`/`entry`/`mark_submitted`/`mark_rejected`/
  `mark_failed`, plus `status`/`status_of`) and `FileJournal`, a
  dependency-light reference implementation: an append-only,
  `fsync`-per-write JSON-lines file, replayed on `open()` to recover
  pending state after a crash. `AvalonClient`/`Session` don't drain it
  automatically yet — that's a deferred submission engine, a documented
  next step rather than something silently assumed built.
- **Integrator Space (schemas)** — `#[derive(AvalonSchema)]` generates the
  `.proto` message text and visibility maps a schema publication needs from
  an ordinary Rust struct. `Session::publish_schema_version::<T>()` and
  `Session::publish_instance::<T>(version, &instance)` are both
  authenticated via the same challenge-response ceremony achievement
  issuance uses, with no second content-specific signature.
  `publish_instance` always targets the session's own identity.
- **Integrator/issuer registration, recovery, registry** — integrator
  registration and issuer-key management
  (`AvalonClient::register_integrator`/`list_integrators`/`get_integrator`/
  `list_issuer_keys`/`add_issuer_key`/`revoke_issuer_key`/
  `integrator_whoami`), social recovery's request-initiation flow
  (`AvalonClient::start_recovery_request`, driving a real WebAuthn
  ceremony, plus the public read/finalize endpoints), and registry/
  recognition reads and writes (`get_integrator_registry`,
  `list_recognitions`/`list_recognized_by`,
  `Session::publish_recognition`/`revoke_recognition`) round out the
  surface, all live against a real server.
- **HTTP layer** — a shared `send`/`map_error_response`/`retry_write`
  module is the machinery behind the retry/idempotency behavior described
  above; every domain module goes through it rather than a raw `reqwest`
  call with ad hoc status matching.

**`AccountSession`** is a second, entirely separate session type
alongside `Session`: a first-party client for an identity's own account
(registration, login, recovery, passkeys, devices, full guild
administration, friends/blocks/presence/discovery, conversations, and
integrator connect/consent — the full first-party surface the Hub
exposes). No conversion exists between `Session` and `AccountSession` in
either direction, and no `AccountSession` constructor accepts an
integrator credential anywhere in its signature.

It's obtained via several entry points on `AvalonClient`, none of them
`authenticate()`: `register(display_name)` drives a real WebAuthn
registration ceremony against a virtual (software-only) authenticator and
generates a fresh Ed25519 event-signing key locally, then immediately logs
the new identity in; `account_login(&AccountCredentials)` logs back into an
identity `register` already created; `resume_account_session(token)` /
`resume_account_session_with_signing_key(token, seed)` wrap an
already-minted bearer token, mirroring how the Hub resumes a persisted
session; `start_account_device_login()` is an `AccountSession`-returning
counterpart to the integrator-side device login, for a client with no
WebAuthn ceremony surface of its own that still needs to originate a
first-party login rather than resume a token minted elsewhere.

Every action the server flags as signature-required signs itself
automatically with `AccountSession::sign` — the caller never
hand-constructs `signing_key_id`/`signature`. A session built via
`resume_account_session` (token only, no local key) sends those requests
unsigned; the server's own signature-requirement check surfaces the real
problem rather than the SDK silently failing.

The Rust SDK consumes generated wire-shape types for most of the
`AccountSession` domain, converted at build time from
`docs/generated/openapi.json` via `typify` — a hand-picked allowlist of
schema names becomes Rust types written to `OUT_DIR` and included into the
crate. WebAuthn-ceremony request/response types stay hand-written (their
`challenge`/`credential` fields are opaque blobs in the schema), as do
pure signature-only wrapper bodies. Endpoint-path stub constants are also
generated, covering the whole published API, so hand-written
`format!("/guilds/{guild_id}/roles")`-style paths reference a shared
constant instead of a literal string at every call site.

### C# SDK surface

`avalon-sdks`' `languages/csharp/AvalonSdk` (moved from this repo's `bindings/csharp`
by #775) is a real, building C# port of the same surface: auth/session,
friends/presence, guilds, conversations,
sync-journal, achievements (including definition CRUD and bulk issuance),
integrator-space schema/mapping/instance-data, integrator registration and
key management, social recovery's request-initiation flow, and
registry/recognition reads and writes. `AccountSession` (split across
`AccountSession.Passkeys.cs`/`.Devices.cs`/`.Recovery.cs`/`.Social.cs`/
`.Conversations.cs`/`.GuildAdmin.cs`/`.Integrations.cs`/`.DeviceLogin.cs`)
mirrors the Rust `AccountSession` surface field-for-field with
C#-idiomatic naming. Signing uses a pure-managed `BouncyCastle.Cryptography`
Ed25519 implementation rather than a native library, keeping the SDK
Unity/IL2CPP-safe. Typed exceptions
(`AuthenticationFailedException`/`CapabilityNotGrantedException`/
`AvalonRequestException`/`MissingIssuerCredentialsException`/
`AvalonWebSocketException`/`NotConversationParticipantException`) mirror
the Rust `SdkError` variants this SDK has coverage for.

**Deliberate scoping gap:** this SDK does not drive a WebAuthn ceremony of
its own — nothing in its dependency set does WebAuthn, and its real
audience (a Unity game binding a bearer token/signing key another surface
already produced) makes a ceremony-driving entry point low value relative
to its dependency cost. So `AccountSession` is constructed only via
`AvalonClient.ResumeAccountSessionAsync(token)` /
`ResumeAccountSessionWithSigningKeyAsync(token, signingKeySeed)`, plus
`StartAccountDeviceLoginAsync()` for a client with no WebAuthn surface that
still needs to originate a login. `Register`/`AccountLogin`/
`AddPasskeyAsync` are not ported, and `AccountCredentials` has no C#
equivalent.

Generated wire-shape types come from `docs/generated/openapi.json` via
NSwag's C#-POCO-only generator, written to a checked-in
`Avalon.Sdk.Generated.*` file regenerated by a standalone console tool
rather than at compile time (this SDK has no build step Unity consumers go
through). `AvalonSdk.Tests/` covers the SDK with stubbed-HTTP unit tests
plus opt-in live tests against a real server and Postgres.

### TypeScript SDK surface

`avalon-sdks`' `languages/typescript/` (moved from this repo's `bindings/ts`) is a
self-contained TypeScript SDK implementing both `AccountSession` and
`IntegratorSession` from scratch, ES modules, `vitest` for tests.
Published to GitHub Packages as `@avalon-initiative/protocol-sdk`;
`apps/hub`'s entire data-fetching surface runs on the published package
in production, not a local path.

Unlike the C# port (which scopes WebAuthn ceremony-driving out entirely)
and the Rust port (which drives a virtual/software authenticator, since it
has no browser to run in), this SDK is browser-facing and drives a **real**
WebAuthn ceremony via `@simplewebauthn/browser` —
`AvalonClient.register(displayName)` and `AvalonClient.login(credentials)`
are both implemented for real. `AvalonClient.resumeAccountSession(token)` /
`.resumeAccountSessionWithSigningKey(token, seed)` cover the
already-minted-token path, and `.startAccountDeviceLogin()` is the same
device-originated-login pattern the other SDKs have.

`AccountSession` mirrors the Rust/C# surface field-for-field, split one
file per domain under `typescript/src/accountSession/`, attached to the
class prototype and merged into the `AccountSession` interface via
TypeScript declaration merging (since TS classes can't be split across
files the way a C# `partial class` can). Every signature-required action
signs itself automatically via `AccountSession.sign(actionTag, fields)`,
using `@noble/curves`'s ed25519 over the same canonical bytes the server
reconstructs. `IntegratorSession` mirrors the Rust `Session`/C# `Session`
capability-gated model, including achievements read/issue driving the same
challenge-response-plus-embedded-signature exchange the other SDKs use.

This SDK is also where several capabilities exist **only** in TypeScript
today (see [Known limitations](#known-limitations)):

- **Realtime websocket subscriptions** — `subscribePresence`
  (`GET /ws/presence`) and `subscribeChannelMessages`/
  `subscribeConversationMessages` (`GET /ws/messages`), including the
  signed interest-claim handshake a chat subscribe sends after the
  server's `node_info` hello.
- **Session-continuation reconnect** — a signed, short-lived continuation
  token minted from the session's own in-memory signing key on a 401,
  retried exactly once, never persisted back into the session's own token.
- **BIP39 mnemonic-derived signing keys** — `registerWithMnemonic`
  (`register()`'s mnemonic-backed sibling, returning `{ session, mnemonic }`)
  and `AccountSession.attachSigningKey(secretKey)` (resolving an
  already-derived key's server-side signing-key id and attaching it to an
  already-built session), matching the Hub's own already-shipping
  disaster-recovery fallback derivation exactly.
- **`AvalonClient.loginWithIdentityId(identityId)`** — logging in with any
  device with a registered passkey, including one with no local signing
  key at all yet, resolving to a session with no signing key attached
  until one is separately supplied.
- **Free-standing, unauthenticated functions** covering the public
  integrator directory, identity integrator-data reads, recovery
  initiation, cross-node login, and an integrator's self-management
  surface (registration, issuer keys, achievement/milestone definitions,
  schema/mapping/instance-data publication, recognition), each proven by
  the integrator's own key via the same challenge-response scheme rather
  than an identity session token.

Generated wire-shape types come from `docs/generated/openapi.json` via
`openapi-typescript`, a devDependency-only tool with zero runtime
footprint. This SDK has no build step at all, so the generated file is a
checked-in artifact like the C# equivalent, not a compile-time output.
WebSocket push-payload shapes and opaque WebAuthn-ceremony blob fields stay
hand-written, the same carve-outs the Rust and C# codegen make.

## Cross-SDK conformance suite

Wire-shape codegen alone can't catch parity gaps in client-side "smart"
behavior — a real signing algorithm, a real derivation, a real handshake —
since none of that is expressible as a schema. The conformance suite
targets that failure mode directly.

Shared, canonical test vectors live at `conformance/vectors/` (repo root,
not nested under any one SDK) — one JSON file per behavior, each naming
which SDKs actually implement it today (`supportedIn`) and, for the rest,
why not (`notSupported.<language>`). See `conformance/vectors/SCHEMA.md`
for the full format. Each SDK has a thin runner consuming the same
vectors, wired into its existing default test job.

What's covered today: `cross-node-login.json` (`CrossNodeLoginGrant`
signing) and `attestation-signing.json` (attestation issuance, bulk
issuance, and revocation signing bytes) are implemented and verified
byte-for-byte identical across all three SDKs. `signed-tree-head.json` is
Rust only. Session-continuation tokens, the WebSocket interest-claim
subscribe handshake, and BIP39 mnemonic-derived signing keys exist in
TypeScript only — the Rust and C# runners assert this gap explicitly (a
named, passing skip test citing the vector file's own `notSupported`
entry) rather than inventing a fake implementation to pass their own
suite.

Standing convention: add a vector under `conformance/vectors/` whenever a
new smart-client behavior lands in *any* SDK, in the same change — list
the implementing SDK(s) in `supportedIn` immediately, even if the other
two don't implement it yet.

## SDK coverage check

`scripts/check-sdk-coverage.py` (in the `avalon-sdks` repo, run against its vendored `docs/generated/openapi.json`) diffs the
SDK-facing route table against each SDK's real HTTP call sites and fails,
naming the exact route(s), when one or more SDKs never call a route the
server exposes. It matches on HTTP method plus a normalized path template,
the one thing common to all three SDKs regardless of what each one calls
the wrapping method. A small, explicitly-cited allowlist covers the
WebAuthn ceremony endpoints C# deliberately doesn't port. All three SDKs
currently report full route coverage against the published API.

## Known limitations

- **Zero-URL `connect()` discovery is real in all three SDKs, but
  unranked.** `AvalonClient::connect(target, config)` (Rust),
  `AvalonClient.ConnectAsync(target, config)` (C#) and
  `AvalonClient.connect(target)` (TypeScript) resolve a `TargetNetwork` (an
  exact `network_id` or a deployment tier) to a live server with no URL
  supplied up front: candidates come only from `docs/trusted-networks.json`'s
  own `server_url`/`seed_nodes` fields for matching entries, each fetched and
  verified via the same `GET /ledger/sth/latest` STH check `verify_network()`
  uses for an already-known URL — so a candidate that answers but isn't
  cryptographically the target network is rejected, not silently accepted.
  First candidate that verifies wins; every rejected candidate is retained so
  a caller can see why. Explicit-URL construction remains fully supported for
  self-hosted/local-dev connections — this is additive. No SDK yet consumes
  the server's `GET /nodes/discover` peer-set expansion or ranks candidates by
  latency/health/role. Capability negotiation for
  an already-known URL is real, across all three official SDKs:
  `GET /nodes/status` reports a node's own `roles`
  (settlement/indexer/realtime/gateway), exposed as
  `AvalonClient.status()`/`getNodeStatus()` (TypeScript),
  `AvalonClient::node_status()` (Rust), and `GetNodeStatusAsync()` (C#).
- **Visibility scoping is partial.** Presence reads and guild rosters are
  scoped server-side by the subject's own visibility settings. Guild
  `channels()`/`messages()` are not — any member with `guilds.chat` sees
  every channel regardless of any per-channel visibility a guild might
  eventually want.
- **Idempotency-key coverage is partial.** Only achievement issuance
  carries a real `Idempotency-Key`; every other unkeyed write (direct
  messages, guild/conversation sends without a dedup key, schema/instance
  publication) stays single-attempt rather than being retried unsafely.
- **Per-language feature gaps.** The C# SDK doesn't drive a WebAuthn
  ceremony at all (registration/login/add-passkey flows are expected to
  stay Hub/browser-side). BIP39 mnemonic-derived signing-key recovery,
  the session-continuation reconnect primitive, and the WebSocket
  interest-claim subscribe handshake exist in the TypeScript SDK only;
  Rust's WebSocket subscribe methods send a bare `subscribe` message with
  no equivalent handshake, and neither Rust nor C# has a session-level
  reconnect-on-401 mechanism.
- **No server-side SDK-version compatibility enforcement.** Each SDK
  embeds the OpenAPI schema version it was generated against, but nothing
  server-side currently checks or logs a caller's declared schema version
  — a separate, bigger compatibility-policy question than plumbing the
  version through.
