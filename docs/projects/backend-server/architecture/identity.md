# Identity

**An Avalon identity is self-owned and integrator-independent.** It is the one
thing that survives any single integrator, server, or database disappearing. An integrator
never owns it, never defines it, and never gets to rewrite its history. Integrators
establish their own scoped participation under it (see
[`./bindings.md`](./bindings.md)); the identity itself stays the same
across all of them.

Narrative: [`../stakeholders/Proposal.md` §7](../../../stakeholders/Proposal.md#7-persistent-identity)
and [§19](../../../stakeholders/Proposal.md#19-identity-vs-game-data).

## The model

```text
Avalon Identity
    ├── Profile (identity-controlled metadata)
    ├── Friends                       ./social-graph.md
    ├── Guild memberships             ./guilds.md
    ├── Achievements / attestations   ./achievements-and-attestations.md
    ├── Permission grants             ./privacy.md
    └── Integrator bindings                 ./bindings.md
          ├── Integrator A → characters (integrator-owned)
          └── Integrator B → characters (integrator-owned)
```

An identity is an opaque, stable handle (`IdentityId`, a UUID). It is never
derived from a display name, a username, a wallet address, or anything its
owner might want to change later. Everything human-facing hangs off it as
profile data.

## Self-described metadata is self-expression, not fact

The profile carries what an identity's owner chooses to say about themselves:
display name, avatar, bio, favorite genres (drawn from a fixed, small
vocabulary — `avalon_protocol::identity::Genre` — not free text, so it stays
useful for matching/filtering later), and pronouns (issue #155), plus a
banner image, a short status line, a small list of self-reported links, a
self-reported timezone, an accent color, and a free-text location (issue
#372). None of it is an authoritative integrator fact — `location` in particular is
self-described text only ("Pacific Northwest," say), never IP-derived or
geocoded; nothing in this protocol infers where a user physically is.

It also carries `main_guild` (no ticket): a self-chosen pointer to one of the
identity's own current guild memberships, so an integrator building a
guild-chat-style UI has one guild to default to instead of having to support
arbitrarily-many simultaneous memberships — see
[`./guilds.md`](./guilds.md#a-users-main-guild). Unlike the fields above,
setting it is checked against a real fact (current membership), not just
validated for shape; and unlike every other field here, `null` doesn't mean
"no guild" — a caller wanting a default in that case reads
`GET /me`'s `effective_main_guild`, computed at read time as the earliest
guild membership joined, never written back into `main_guild` itself.

Someone writing "I am an Avion" in their bio does not make Avion a
network-level race. An integrator can display that, interpret it, or ignore it. Facts
about what an identity *has done* come from issuer attestations with
provenance (see [`./provenance.md`](./provenance.md)), never from the profile.

The profile is deliberately small. It is not where game-specific data lives —
that boundary is the whole point of
[ADR #67](https://github.com/LunarVagabond/avalon-protocol/issues/67).

## What is promised durable

Per [ADR #75](https://github.com/LunarVagabond/avalon-protocol/issues/75),
anything Avalon promises to preserve must be reconstructable from protocol
history, and every change to it must emit a protocol event in the same unit of
work as the projection change. For identity state:

Unless a row says otherwise, every optional field below shares one
convention: `null` in the `profile.updated` payload means the field was
explicitly cleared, and an absent key means it was untouched.

| State | Promised durable? | Canonical record | Notes |
|---|---|---|---|
| identity exists, `created_at` | yes | `identity.created` | self-signed, see below |
| `display_name` | yes | `identity.created` (initial), `profile.updated` (changes) | emitted in the same transaction as the `profiles` row, via the outbox. Issue #510: `display_name` itself is the globally-unique, case-insensitive handle — no separate discriminator suffix exists any more (issue #128's old `name#1234` scheme, dropped) |
| `avatar_url` | yes | `profile.updated` | — |
| `bio` | yes | `profile.updated` | free text, capped at 500 characters (#155) |
| `favorite_genres` | yes | `profile.updated` | fixed, small controlled vocabulary (`Genre`), capped at 5 entries; unknown values rejected, not dropped; a present key always fully replaces the list, including to `[]` (#155) |
| `pronouns` | yes | `profile.updated` | free text, capped at 40 characters (#155) |
| `banner_url` | yes | `profile.updated` | same shape/validation as `avatar_url`, a separate image slot (#372) |
| `status` | yes | `profile.updated` | free text, capped at 100 characters (#372) |
| `links` | yes | `profile.updated` | up to 5 self-reported URLs, each capped at 200 characters and required to parse as an `http`/`https` URL; a present key always fully replaces the list, including to `[]` (#372) |
| `timezone` | yes | `profile.updated` | free text, capped at 64 characters; NOT validated against the real IANA time zone database (no such crate in this workspace today) — a documented gap (#372) |
| `theme_color` | yes | `profile.updated` | must match `^#[0-9a-fA-F]{6}$` (#372) |
| `location` | yes | `profile.updated` | free text, capped at 100 characters, self-described only — never IP-derived or geocoded (#372) |
| `main_guild` | yes | `profile.updated` | a pointer to one of this identity's own current guild memberships (no ticket — see below); must name a guild the identity is currently a member of, checked server-side against `guild_members`; also cleared automatically, in the same transaction, if the identity leaves the guild it points at |
| future title / labels | classify when added | `profile.updated` | the rule: promised-durable means it emits, or it isn't promised |
| WebAuthn passkey(s) | yes, as of #523 (Part 1 of #521's decision) | `identity.passkey_registered`/`.passkey_revoked` | `identity_keys` table (authoring node's own local source of truth) / `indexer_identity_passkeys` projection (what a mirror-only node reconstructs from replayed history alone); see below. Public credential material only, never anything secret. Passkeys registered before #523 landed have no such event — only newly-registered credentials are portable this way; a pre-existing passkey still works for local login on the node it was registered on, it just isn't verifiable from a different node's mirrored history |
| event-signing public key | yes, as of #525 (surfaced while implementing Part 2 of #521's decision) | `identity.signing_key_added`/`.signing_key_revoked` | `identity_signing_keys` table / `indexer_identity_signing_keys` projection, same authoring-vs-mirror split as passkeys above. **Corrected from an earlier version of this table**, which claimed this was already durable via `identity.created`'s issuer alone — that's a `GlobalId` string (`identity:<id>:self:created`), never the actual key bytes; the very first signing key genuinely had no durable event of its own until #525 |
| credentials (password hash) | **no**, pruned entirely | — | #73, done |
| sessions / tokens | **no**, still ephemeral server state — see below for the additive exception | — | the opaque `sessions`-table bearer token itself is unchanged; #525 adds a *separate*, short-lived, self-signed continuation credential that isn't stored anywhere at all (verified statelessly against the durable signing key above plus a one-time-use nonce row) |
| presence | **no** | — | [`./presence.md`](./presence.md) |
| visibility settings | no, unless later promoted | — | [`./privacy.md`](./privacy.md) |

A session token or any other shared secret is never part of an event payload —
there is no shared secret at all now that #73 has landed. The
`identity.created` event no longer carries a `username`.

## Authentication: two keys, two jobs (#73)

An identity is a self-custodied keypair — two of them, in fact, each doing a
different job, matching how real passkey-based wallets are actually built
(confirmed against how passkey Bitcoin wallets work: the passkey is a secure
*unlock*, a separate key is the actual *signer* — WebAuthn's challenge is
deliberately not a general-purpose signing oracle, so it can't do both jobs at
once):

- **A WebAuthn passkey** (`identity_keys` table) proves interactive presence —
  "the holder of this device authorized this request, right now." This is the
  entire login mechanism: register a passkey once, then a normal WebAuthn
  ceremony each time after. Multiple passkeys per identity are supported by
  the schema, and an authenticated identity can register additional ones
  after the fact (issue #200, issue #99's cheapest recovery mitigation): any
  registered passkey authenticates the identity, none is privileged over
  another, and each is independently nameable and revocable — mirroring the
  #135/#145 signing-key device list's UX, but against `identity_keys`, a
  different table with a different security property (see #200's own note
  below). As of #523, registering or revoking a passkey also emits a durable
  `identity.passkey_registered`/`.passkey_revoked` event carrying its public
  credential material — so a node that only ever mirrored this identity's
  ledger history can independently verify a fresh login for it, not just the
  node the passkey was originally registered on. This is a separate,
  additive fact about the *passkey* only; it does not change anything about
  how the signing key below is stored or recovered.
- **A raw Ed25519 key** (`identity_signing_keys` table) proves authorship of a
  specific durable event. It signs `identity.created` at registration —
  `issuer` on that event is `identity:<id>:self:created`, not
  `network:avalon-server:...` — so a hosted node can no longer fabricate an
  identity that never actually registered, the same guarantee that stops a
  node fabricating an integrator's attestation (see
  [`./security-model.md`](./security-model.md)). Losing this key alone is not
  catastrophic the way losing every passkey is: the identity still logs in,
  and can rotate to a new signing key from an authenticated session — that
  rotation flow isn't built yet either, but nothing about the schema
  precludes it.

Login is identity-id-first, not fully usernameless: `webauthn-rs`'s
convenience registration path hardcodes non-resident credentials, so true
discoverable ("tap your passkey, no identifier at all") login would need
*attested resident keys* — a heavier, attestation-verifying registration path
that wasn't built for this milestone. The meaningful property survives
regardless: no shared secret, a real challenge-response proof every time.

### Two authorization tiers: ambient session vs. a fresh signature (#696/#697/#698)

The two keys above answer "how does this identity log in" and "how does
this identity author a durable event." A third question — once an
identity already holds a session, how much can that ambient bearer token
alone actually do — is #696's decision: not every action an
authenticated session can request deserves the same trust. Most
first-party account actions (reads, chat, presence, profile edits,
ordinary guild membership churn) stay authorized by the session's bearer
token alone, the same as they always were. A smaller set of actions with
real blast radius or that are hard to reverse — guild ownership transfer,
role/permission-structure changes, approving a brand-new device into an
identity, revoking the last remaining passkey, granting an integrator
standing capabilities, and a few more — additionally require a *fresh*
signature from the identity's own locally-held Ed25519 signing key
(the same key above, reused rather than a new mechanism) at the moment of
the action, proving the request came from a device that actually holds
that key, not just whatever holds a copy of the bearer token. A stolen
session token alone is no longer sufficient for that second tier.

This applies uniformly across every first-party client — the Hub web app
and every SDK's `AccountSession` type (Rust, C#, TypeScript) mint that
signature automatically wherever it's required; a caller never
hand-constructs one. It does **not** create any new way for a
capability-gated integrator credential to escalate into account-level
power — that boundary (`IntegratorSession`/the integrator `Session` in
each SDK) is unaffected and stays exactly where it was. See "Today in the
repo" below for the complete endpoint-by-endpoint classification, the
exact canonical signing-message format, and which gaps this closed.

### Cross-device pairing for a WebAuthn-incapable client (#307)

WebAuthn login above assumes the client has some ceremony surface — a
browser, or a platform authenticator. A game engine with no embedded
browser, a console, or any other headless client doesn't, and never needs
to: `avalon-sdk`'s `authenticate()` already takes a bearer session token,
not a live WebAuthn exchange, so the only real gap is *how such a client
obtains that token in the first place*. Cross-device pairing
(`crates/server/src/device_pairing.rs`) solves it the way platform account
systems solve the identical problem: the incapable client requests a
pairing (`POST /auth/device/start`, unauthenticated) and gets back a short,
human-typeable `user_code` (shown to the user, e.g. as a QR code pointing
at `verification_uri`) plus an opaque `device_code` it alone holds. The
user completes a real WebAuthn login on a capable device — the Hub, in a
browser — and approves the pairing there (`POST /auth/device/approve`,
`apps/hub/src/views/PairDevice.vue`); the waiting client polls
(`POST /auth/device/poll`, bearer = `device_code`) until it receives an
ordinary session token, minted through the exact same
`auth::generate_session_token`/`sessions`-table mechanism
`handlers::session_finish` uses for a normal login — not a second,
differently-trusted token type.

The security boundary is deliberately not the `user_code`'s secrecy:
approval requires the *approver's own already-authenticated session* — the
same `authenticate()` check every other authenticated route in this crate
uses — so there is no path from "knows the user_code" alone to a minted
session. The `user_code` only disambiguates which pending pairing to act
on; it still carries real entropy (8 chars from an alphabet with
`0`/`O`/`1`/`I`/`L` removed), a ~10-minute expiry, and single-use poll
delivery (an approved pairing's token is returned exactly once; every
later poll of that `device_code` gets `expired`), but none of those are
the reason this is safe — the authenticated-approver requirement is.
Structurally this mirrors `crates/server/src/devices.rs`'s
request/approve/poll shape (#135/#122) and its rate-limiting/expiry
conventions, but solves a genuinely different problem — bootstrapping a
session for a client with *no* prior session at all, versus adding a
trusted signing device to an identity that's already authenticated
somewhere — so it lives in its own module and its own `device_pairings`
table rather than being bolted onto `devices.rs`. No password field or
password-shaped persistent secret is introduced anywhere in this flow.
`avalon pair-device` (`crates/cli`) drives the `start`/`poll` side as a
stand-in incapable client, for testing this without a real console/engine.
`avalon-sdk`'s `AvalonClient::login()`/`DeviceLogin::wait()`
(`crates/sdk/src/device_login.rs`, #398) is the real, required integration
surface for that same waiting side — #307 had left an SDK-side wrapper as
optional scope-creep, but hand-sequencing `start`/`poll` per integrator
risked every integrator getting backoff/expiry handling slightly wrong, so #398
promoted it to a first-class helper that resolves straight to a `Session`.



Recovery after every passkey is lost
([#99](https://github.com/LunarVagabond/avalon-protocol/issues/99), decided,
tracked as [#198](https://github.com/LunarVagabond/avalon-protocol/issues/198), an epic under #2): a layered answer, since the options
aren't mutually exclusive. Multi-device/multi-passkey registration (add a
second passkey at onboarding or any time after,
[#200](https://github.com/LunarVagabond/avalon-protocol/issues/200), done)
is the cheap, near-term mitigation for the common single-device-loss case —
it does nothing for someone who only ever registers one passkey and then
loses it, which is exactly why it's a mitigation, not the full answer.
Social recovery
via an M-of-N set of trusted guardians — drawn from the identity's own Avalon
friends, gated by a mandatory public time-delay so the real owner can veto a
malicious attempt — is the real answer for losing every device at once
([#201](https://github.com/LunarVagabond/avalon-protocol/issues/201), done;
see below); it composes naturally with the social graph already being
network-owned and avoids a centralized custodian. An opt-in custodial
fallback (email/SMS) stays explicitly off the table as a *default*: it
reintroduces exactly the shared-secret, centralized-trust surface #73 exists
to eliminate, and would only ever ship as a clearly-labeled, separately
opted-into weaker-security tier, never silently. Onboarding must still make
the total-loss consequence of relying on a single passkey loud and explicit
for an identity owner who hasn't configured guardians, not a buried settings toggle
discovered only after someone has already lost everything — that
requirement holds regardless of what else has shipped. Separately open:
migrating an existing username/password identity (none exist outside
development, so not applicable yet), and whether identities
can be transferred (Proposal §32).

### Hybrid transport ("use a phone or tablet") — issue #397

WebAuthn recognizes three authenticator categories: platform (Touch ID,
Windows Hello — built into the device doing the ceremony), cross-platform
hardware (a USB/NFC security key), and **hybrid transport** — the browser
shows a QR code, a nearby phone proves proximity over Bluetooth and unlocks
its own resident passkey, and the desktop/browser session is authenticated
through it without the passkey ever leaving the phone. This matters on a
desktop browser with no platform authenticator and no hardware key handy —
incognito/private mode commonly disables platform authenticators too — since
hybrid transport is otherwise the only way such a browser can use a passkey
at all.

This is a different problem from [#307](#cross-device-pairing-for-a-webauthn-incapable-client-307)
above: #307 is for a client with *no* WebAuthn/browser surface whatsoever
(a game engine, a console). Hybrid transport is a built-in feature of an
ordinary WebAuthn-capable browser and OS — Avalon doesn't implement it,
doesn't need to, and can't disable it short of explicitly restricting
authenticator attachment.

Verified by reading `crates/server/src/auth.rs`'s `build_webauthn` and
`crates/server/src/handlers.rs`'s `register_start`/`session_start`: neither
sets `webauthn-rs`'s `authenticator_attachment` (the registration option
that, if set to `Platform`, would suppress the hybrid/cross-platform choice
in the browser's UI), and the stored `Passkey`'s credential descriptor
carries no `transports` hint either (`webauthn-rs-core` hardcodes
`transports: None` when building it), so an authentication challenge's
`allowCredentials` entries never restrict which transport the browser may
use to satisfy them. Both are exactly the two levers that could suppress
hybrid transport, and neither is set — confirmed by inspecting the raw JSON
`/identities/register/start` and `/sessions/start` actually return
(`crates/server/tests/hybrid_transport.rs`, `--ignored`, live). That same
test also completes a full register-then-login round trip through a generic
(non-platform) virtual authenticator to confirm the server applies no
authenticator-type-specific logic anywhere in that path — nothing reads or
branches on `authenticatorAttachment`, an AAGUID, or a transport value.
`require_resident_key(false)` (this file's own non-discoverable-credential
note above) is orthogonal: it controls whether the browser must create a
*resident* credential, not which transport may satisfy the ceremony, so it
neither helps nor hinders hybrid transport here.

What this does *not* establish: an actual end-to-end hybrid ceremony —
scanning a QR code with a real phone over real Bluetooth — was not run.
That requires a real, non-headless browser and a real phone, which no
session doing this verification had access to; the configuration-level
check above is the strongest verification available without that hardware,
and is offered as exactly that, not as a substitute for it. A maintainer
with a phone and a normal (non-incognito) browser handy should still
confirm the "use a phone or tablet" option actually appears and completes
against a real `make start` server before treating this as fully closed.

Per [#51](https://github.com/LunarVagabond/avalon-protocol/issues/51)'s
design, this entire question is invisible to `avalon-sdk`: an integrator
never performs a WebAuthn ceremony through the SDK at all —
`AvalonClient::authenticate()` exchanges an already-issued bearer session
token for `GET /me`/`GET /me/grants`, nothing else (`crates/sdk/src/lib.rs`).
Which authenticator category produced that token has no representation
anywhere in that exchange, by construction, not by omission.

### Where the Ed25519 signing key lives in a browser

The Hub derives the identity's Ed25519 signing keypair client-side during
registration from a freshly generated BIP39 mnemonic phrase
(`apps/hub/src/crypto/signingKey.ts::deriveSigningKeyFromMnemonic` —
`sha256(BIP39 seed ‖ a versioned domain-separation label)`, not a BIP32 HD
derivation, since there's exactly one signing key per identity, not a
hierarchy) and stores the resulting secret key in plain `localStorage`,
keyed by identity id, unencrypted:

- It sits in plaintext at rest, same exposure as any other `localStorage`
  value (readable by any script running on the origin).
- The mnemonic itself is shown to the user exactly once, at creation
  (`CreateIdentity.vue`), and is never stored anywhere — any device holding
  it can re-derive the identical key offline, with no server round-trip
  (`Profile.vue`'s "recover your signing key" section, shown when the
  current device has none). The server only ever sees the resulting
  *public* key.

This mnemonic recovery is the fallback path. The primary path — a
device-registration/linked-device grant model, no phrase in the common
case — is [#135](https://github.com/LunarVagabond/avalon-protocol/issues/135):
each device gets its own signing keypair (`identity_signing_keys`, already
multiple rows per identity), and a grant only ever *authorizes* a new
device's public key — it never transfers a private key. An already-trusted
device (`crates/server/src/devices.rs`) approves a new one by signing the
grant with its own key; the server verifies that signature against the
approver's still-active key before registering the new one. Revocation
(`POST /me/devices/:id/revoke`) is unilateral — any authenticated session
for the identity can revoke any signing key, including its own, without
the revoked device's cooperation. Both `#134` and `#135` were decided
together in [#122](https://github.com/LunarVagabond/avalon-protocol/issues/122),
which is distinct from [#99](https://github.com/LunarVagabond/avalon-protocol/issues/99):
#99 covers a lost *passkey* (login credential), #122 covers the *signing
key* (what authenticates an identity's authored events) — recovering or
granting one never by itself authenticates a login. The WebAuthn passkey
has no equivalent client-storage decision here: it never leaves the
platform authenticator, already synced across devices by whatever passkey
provider the identity's owner uses. Separately, as of #523, the passkey's
*public* credential material (never anything secret, same as everything
above) is now durable/mirrored — a distinct fact from this section's
signing-key custody story, not a change to it.

### Session continuation across nodes (#525, Part 2 of #521's decision)

An opaque `sessions`-table bearer token is node-local by construction —
minted by whichever node ran the WebAuthn login, checked only against that
node's own `sessions` table. If that node goes offline, the token is dead
even though every other trusted node may have fully mirrored the
identity's ledger history and could otherwise serve it fine (issue #520).
Confirmed hands-on in the two-node LAN sandbox: a token minted on one node
is flatly rejected by another.

The fix doesn't touch the opaque token at all — it adds a second,
*additive* credential kind a client can present in the same
`Authorization: Bearer` slot: a **session-continuation token**
(`avalon_protocol::continuation::ContinuationToken`), a short-lived
(60 seconds by default), self-signed assertion `{identity_id,
signing_key_id, nonce, issued_at, expires_at, signature}`, minted entirely
client-side with the identity's own Ed25519 event-signing key — the same
key it already holds for authoring events (see "Where the Ed25519 signing
key lives in a browser" above). No server ever issues one; a node's only
job is verification.

Verification (`crates/server/src/continuation.rs`, wired into
`handlers::authenticate_token` so every existing session-gated route gets
it for free, no per-route changes) needs nothing but:

- the token's own claimed fields (checked for a sane, not-too-long expiry
  window and not already-expired, independent of what the client claims),
- the signing key's public half, read from
  `avalon_indexer::projections::identity_signing_keys` — durable as of
  this same ticket (see the durability table above) and, crucially, the
  *same* projection whether this node authored the key locally or only
  ever mirrored it,
- a one-time-use nonce, checked against `consumed_continuation_nonces`
  (a node-local anti-replay table, not itself durable/mirrored — a
  continuation token that somehow got replayed against a *different* node
  before its 60-second window closed would still succeed there; accepted
  as a narrow, short-window residual risk rather than building cross-node
  nonce coordination for it).

Never a login credential by itself (#122's separation, preserved): a
continuation token only extends an *already-established* session — nothing
routes it into `/identities/register/*` or `/sessions/*`, which authenticate
via a real WebAuthn ceremony, never via `authenticate_token`. Revoking the
signing key (`POST /me/devices/:id/revoke`) makes every future continuation
token minted with it fail immediately, on any node, the same
durable-revocation guarantee #523 gives passkeys.

**The client-side trigger (Hub half, implemented).** The identity's own
signing key only ever lives in `apps/hub`'s browser storage (see "Where
the Ed25519 signing key lives in a browser" above) — third-party games/
tools using the Rust SDK receive an already-authenticated session token,
never the raw key, so minting happens in `apps/hub/src/crypto/continuation.ts`,
not `crates/sdk`. `mintContinuationToken` is a byte-for-byte TypeScript
port of `avalon_protocol::continuation::signing_bytes`/`ContinuationToken::to_wire`
— live-verified: a token minted by the TS code was accepted by a real,
running `avalon-server`'s `GET /me`, and correctly rejected on a second
use of the same token (anti-replay).

The trigger itself lives centrally in `apps/hub/src/api/client.ts`'s one
`request()` function, not scattered across call sites: a 401 against
whatever token was passed is retried **exactly once**, with a freshly-minted
continuation token, but only when that token is exactly the
currently-persisted opaque session token (never some other bearer value a
caller passed directly, e.g. an identity token mid-registration) — the
specific case a 401 there can mean "this session's origin node doesn't
recognize this token," whether because the viewer explicitly switched
`server_url` (`NetworkStatus.vue`, issue #232) or the original node went
offline and a different one is now, for whatever operational reason,
answering at the same URL. The minted token is never persisted back into
the opaque-token storage slot (continuation tokens are single-use/
short-lived by design) — every later request against a node that still
doesn't recognize the opaque token mints its own fresh one again, the same
way. `stores/session.ts` caches `identityId`/`signingKeyId` alongside the
opaque token specifically so minting never depends on a prior successful
`GET /me` call against the (possibly-unreachable) node being reconnected
away from — a narrow, documented exception to #55's "profile data always
re-read from `GET /me`" invariant, since neither field is ever displayed
or treated as authoritative for anything but minting.

Scoped out: WebSocket reconnection (`openPresenceSocket`/chat sockets
carry their token in the connection URL, not through `request()`) and the
Rust SDK/C# binding surface for non-Hub consumers — #91's still-on-hold
node-discovery work is the more natural home for a generic SDK-level
failover story, if one is ever built.

### Social recovery via M-of-N guardians (#201)

This is the real answer to losing every registered passkey at once — #200's
multi-passkey registration only helps if a second device was registered
*before* the loss. An identity owner designates a set of guardians, drawn
only from their current friends (issue #15's network-level primitive — the
only pool this is allowed to draw from), plus a threshold M-of-N.

Configuring or changing that guardian set (`PUT /me/recovery/guardians`)
requires the identity's *current* session, same as every other session-gated
route in this crate. An attacker who has compromised only a not-yet-valid new
device can never reach it. That's what makes "changing the guardian set
requires the current set of valid credentials" true by construction, not by
a special-case check.

Recovery itself is a four-stage state machine, one `recovery_requests` row
per attempt:

1. **Request.** From a new device with no valid session, a caller names the
   identity to recover and completes a WebAuthn registration ceremony for
   that device (`POST /recovery/requests/start` then `/finish` — the same
   two-step shape `handlers::register_start`/`register_finish` and
   `passkeys::register_start`/`register_finish` already use). This is the
   one deliberate exception to "every route requires a session" in this
   crate, since the entire premise is that the caller has none for the
   identity in question.

   It's not an open door, though — several checks bound it: the identity id
   must be real; the identity must actually have guardians configured (an
   unconfigured identity can never satisfy any M, so there's nothing to spam
   toward); at most one *active* request may exist per identity at a time (a
   partial unique index on `recovery_requests`, not an application-level
   check-then-act); and a rolling 24-hour window caps how many requests may
   be initiated against a single identity regardless of outcome. The new
   device's passkey is captured but not yet a valid credential — it sits in
   `recovery_requests.pending_passkey_data` until the request actually
   finalizes.
2. **Approval.** Each guardian independently approves
   (`POST /recovery/requests/:id/approve`), gated on currently — not
   historically — being one of the identity's guardians. Once approvals
   reach the threshold that was in effect at request time
   (`threshold_at_request`, frozen so a guardian-set change mid-attempt
   can't retroactively change what the attempt needs), the request enters
   the delay phase and `delay_ends_at` is set.
3. **Mandatory public time-delay.** `AVALON_RECOVERY_DELAY_HOURS`
   (default 48 — long enough that an owner who only logs in occasionally
   plausibly notices, short enough that a genuine all-devices-lost recovery
   doesn't drag on for a week) must elapse with no veto. The delay's
   existence and countdown are public — `GET /identities/:id/recovery/status`
   requires no auth at all, a deliberate reading of the ticket's "mandatory
   *public* time-delay" language as a public marker on the identity, not
   merely something the owner happens to be told. `GET /me/recovery/status`
   is the session-authenticated mirror of the same data, so the owner sees
   a prominent notice through any surviving session without a separate
   notification channel being invented.
4. **Veto or finalize.** The original owner (any session for the identity
   itself) or any *current* guardian may cancel at any point before
   finalization (`POST /recovery/requests/:id/cancel`) — a former guardian
   who has since been removed cannot, matching the config-change invariant
   above: the owner's own recourse against a compromised guardian is to
   remove them, not to leave their veto/approval power intact.
   `POST /recovery/requests/:id/finalize` is deliberately public and
   idempotent: it grants nothing beyond what approvals and the elapsed
   delay already authorized, so no caller identity needs checking. It
   inserts the pending passkey as an ordinary new `identity_keys` row —
   exactly #200's own mechanism — and never touches or revokes anything the
   real owner might still hold.

The node operator has no path anywhere in this flow that bypasses guardian
approval or the delay — finalize only ever acts on what approvals and
elapsed time already durably recorded in Postgres, not on anything an
operator can unilaterally assert. Every phase transition is durable history
via the outbox: `identity.recovery_configured`, `.recovery_requested`,
`.recovery_approved`, `.recovery_cancelled`, `.recovered` — see
[`./protocol-events.md`](./protocol-events.md).

Scoped out of #201's first pass, deliberately: a background sweep that
auto-finalizes every eligible request the moment its delay elapses (today,
`finalize_request` is caller-triggered — the recovering device's
`RecoverIdentity.vue` calls it once it observes a ready `delay`-status
request past its `delay_ends_at`, #394 — which is correct but not
self-triggering); Rust SDK and C# binding surface for this flow; and Hub UI
polish beyond a functional guardian-management card, initiation flow,
approval list, and finalize action (an owner-visible in-progress banner
shows wherever `GET /me/recovery/status` is checked, but a dedicated
real-time alert is future work). None of these affect the state machine or
its invariants.

## What identity is not

- Not a universal integrator account. An integrator asks for scoped capabilities and gets
  only those.
- Not a universal character. See
  [`./bindings.md`](./bindings.md).
- Not a platform identity in the Steam/Xbox sense. No single operator owns it;
  see [`./nodes.md`](./nodes.md).
- **Not a real-world or government identity system, and not headed toward
  becoming one.** An Avalon identity identifies a keypair, never a person.
  Nothing in `identity.created`'s payload or anywhere else in identity
  creation asks for a legal name, date of birth, government ID, biometric,
  phone number, or email — there is no field for one, and no KYC step.
  This isn't a policy that could quietly change later; it's structural. Because
  identity is self-custodied (#73) with no central issuer, there is no party
  anywhere in the system — including Avalon's own maintainers or any node
  operator — that ever holds, or could be compelled to produce, a mapping
  from a keypair back to a real person, because that mapping is never
  created in the first place. An identity's owner who loses every passkey with no
  recovery configured loses the identity outright; nobody can "look it up"
  and reissue it. What Avalon identifies and makes portable is *online
  activity* — friendships, guild membership, achievements — never
  personhood. The heavy investment in decentralization throughout this
  protocol (no validator set, no platform lock-in, self-hostable nodes; see
  [`./nodes.md`](./nodes.md)) exists in large part to keep that true by
  construction rather than by promise.

## Portability means mirrored, not movable (issue #613, decided)

"Your identity isn't trapped in one game's world" is a claim about
*mirrored history* (this file's own model, [`./nodes.md`](./nodes.md)'s
"Mirrors, not federation" section), not about an end user ever needing to
export and re-import their own data to change which node they primarily
use. An identity's durable facts — friend/guild/achievement events — are
already visible from any node mirroring the network's history, the same
way #582/#583's node-to-node infrastructure already lets a Settlement
node be reached from anywhere in the mesh. There is currently no
user-facing export bundle, and none is planned: there is nothing to
export, because there is nothing a person is trapped inside in the first
place.

**The real limit, stated honestly**: an identity's *authentication*
ceremony (WebAuthn passkey registration, `identity_keys`) is local to the
node it was registered on, distinct from the durable facts above — and
this one is a real, load-bearing gap, not a footnote. A passkey
registered on node A cannot be presented in a WebAuthn ceremony against
node B at all (RP-ID scoping is part of the WebAuthn spec's own security
model, not something Avalon's protocol design can route around). #525's
session-continuation tokens solve the *already-logged-in* half of
reaching a different node, but were deliberately decided (#122) to never
count as a login credential by themselves — they prove key possession,
not human presence. Genuinely proving identity to a node you've never
registered on, for the first time, is decided as
[#620](https://github.com/LunarVagabond/avalon-protocol/issues/620): a
destination-bound, human-approved signed grant, extending #307's
cross-device pairing pattern. Tracked as epic
[#623](https://github.com/LunarVagabond/avalon-protocol/issues/623). See
also [`./nodes.md`](./nodes.md)'s "Identity and social actions are not
shard-locked" section — once #623 lands, "home node" stops being an
ongoing dependency for anything except this initial login step.

## Today in the repo

### Action-tier classification: ambient token vs. fresh signature (#697)

#696 decided that `AccountSession` actions split into two tiers: most stay
authorized by the ambient session bearer token alone; a smaller set of
high-blast-radius, hard-to-reverse actions additionally require a fresh
signature from the identity's own locally-held Ed25519 signing key at the
moment of the action, extending the same
`event_signing_public_key`/`event_signature` pattern `register_finish`
already verifies for `identity.created`. This section is that
classification, endpoint by endpoint, for every account-level (non-
integrator-credential-gated) route in `crates/server`. **It is a design
artifact, not yet enforced** — #698 is the ticket that makes the
signature-required column actually load-bearing server-side; until it
lands, every route below still only checks the ambient bearer token,
including the ones marked "signature-required."

Two endpoint families are out of scope and don't appear below:
node-to-node/operator routes (`/ledger/*`, `/nodes/*`, `/mirror/*`,
`/internal/*` — a different auth domain, see `docs/architecture/chain.md`),
and integrator-credential-gated routes (`/integrations/*`'s own key/schema/
achievement/milestone management, `/issuers/*` — authenticated by an
integrator's root/operational key, never by an identity's session, per
#26/#84's server-to-server model). `/integrations/{slug}/connect` and its
sibling grant-management routes below *are* in scope: those are called by
an identity's own account session to grant or revoke that identity's
consent, not by the integrator itself.

**Pre-session bootstrap — not applicable to either tier.** These routes run
before an ambient session exists, so there's no bearer token to layer a
signature requirement on top of; they're already gated by a real WebAuthn
ceremony and, for the two that mint durable identity state, an Ed25519
event signature at least as strong as the "fresh signature" tier below:
`POST /identities/register/start`, `POST /identities/register/finish`
(verifies the `identity.created` event signature before writing anything),
`POST /sessions/start`, `POST /sessions/finish`, `POST /auth/device/start`,
`POST /auth/device/poll`, `POST /auth/cross-node/start`, `POST
/auth/cross-node/poll`, `POST /auth/cross-node/submit`, `POST
/auth/cross-node/deny`, `GET /auth/cross-node/lookup`, `POST
/recovery/requests/start`, `POST /recovery/requests/finish`, `GET
/recovery/requests/{id}`, `GET /identities/{id}/recovery/status` (the last
four are public by design — see the social-recovery section above).

**Ambient-token tier** — reads, and writes that are low-blast-radius,
reversible, or high-frequency enough that requiring a fresh signature on
every call would make the product unusable:

| Endpoint | Rationale |
|---|---|
| `GET`/`PATCH /me` | Profile fields are self-description, not security state (see "Self-described metadata" above); reversible. |
| `GET /me/history`, `GET /me/achievements`, `GET /me/guild-announcements` | Reads. |
| `GET /identities/profiles`, `GET /identities/{id}/profile`, `GET /identities/{id}/locations` | Reads. |
| `PUT /me/presence`, `GET /presence`, `GET /ws/presence`, `GET /ws/messages` | High-frequency, low-stakes, self-correcting on the next update. |
| `GET`/`POST /friends/requests`, `POST /friends/requests/{id}/accept`, `DELETE /friends/requests/{id}`, `GET /friends`, `DELETE /friends/{identity_id}`, `GET /friends/handle/{handle}` | Reversible social-graph edits; blast radius is "an unwanted friend," not account takeover. |
| `GET /people/discover`, `GET /identities/search` | Reads. |
| `GET`/`POST /blocks`, `DELETE /blocks/{identity_id}` | Reversible, and blocking needs to be immediate/low-friction for it to be useful as a safety tool — gating it behind a signature ceremony would work against its purpose. |
| `GET`/`POST /conversations`, `GET`/`POST /conversations/{id}/messages` | Chat; existing moderation-delete precedent (see #697's own invariants) treats messages as high-frequency and reversible by deletion, not high-risk. |
| `POST /auth/device/deny` | Declining a pairing request; no state granted, nothing to reverse. |
| `GET /me/devices/grants`, `GET /me/devices/grants/{id}`, `GET /me/devices`, `PATCH /me/devices/{id}` (rename), `POST /me/devices/grants` (request) | Reads, a label rename, and *requesting* a grant (requesting confers no access by itself — see signature-required tier for the approval that does). |
| `POST /me/devices/{id}/revoke` | Revocation only ever narrows trust, the same reasoning `devices.rs`'s own module doc already applies to `identity.signing_key_revoked` vs. `identity.signing_key_added`; unilateral by design so a compromised/lost device can be cut off immediately without needing the signing key that device might itself hold. |
| `GET /me/passkeys`, `PATCH /me/passkeys/{id}` (rename) | Reads and a label rename. |
| `POST /me/passkeys/{id}/revoke` (not the identity's last passkey) | Narrows trust; see the last-passkey exception below. |
| `GET /me/recovery/guardians`, `GET /me/recovery/status`, `GET /me/recovery/guardian-requests`, `GET /me/recovery/guardian-of` | Reads (the *write* side, `PUT /me/recovery/guardians`, is conditionally tiered below — see the signature-required row). |
| `DELETE /me/recovery/guardian-of/{identity_id}` | Self-removal only narrows a guardian assignment; #443's clamp-threshold-down behavior keeps it safe unilaterally. |
| `POST /recovery/requests/{id}/approve`, `POST /recovery/requests/{id}/cancel` | Guardian actions on someone *else's* recovery; the M-of-N threshold and mandatory public delay are already the load-bearing safety property here, not per-call signing. |
| `POST /recovery/requests/{id}/finalize` | Finalization only executes what the threshold + delay already approved; nothing new is being authorized at this step. |
| `DELETE /integrations/{slug}/connect` (disconnect), `DELETE /integrations/{slug}/grants/{capability}` | Revocation only narrows what an integrator can do; reversible by reconnecting/re-granting. |
| `GET /me/connections`, `GET /me/grants` | Reads. |
| `POST /guilds`, `GET /guilds/discover`, `GET /guilds/{id}` | Creating a new guild or reading; no existing member's standing changes. |
| `PATCH /guilds/{id}` (name/description/etc.), `GET /guilds/{id}/roles` | Ordinary `manage_guild`-gated edits, not ownership/permission-structure changes. |
| `GET /guilds/{id}/permission-overrides`, `GET /guilds/{id}/integrator-breakdown`, `GET`/`PUT /guilds/{id}/favorite-integrators` | Reads and cosmetic/discovery settings (the *write* side of permission-overrides is in the signature-required tier below). |
| `POST /guilds/{id}/integrations/{integrator_id}` (associate) | Reversible association, not a permission grant to the integrator itself. |
| `GET /me/guild-invites`, `POST /guilds/{id}/invites`, `POST /guilds/{id}/invites/{invite_id}/accept`, `POST /guilds/{id}/invites/{invite_id}/decline`, `POST /guilds/{id}/join`, `GET`/`POST /guilds/{id}/join-requests`, `GET /guilds/{id}/join-requests/mine`, `POST /guilds/{id}/join-requests/{request_id}/approve`, `POST /guilds/{id}/join-requests/{request_id}/reject`, `DELETE /guilds/{id}/join-requests/{request_id}`, `POST /guilds/{id}/leave` | Ordinary membership churn, all reversible (leave and rejoin, reject and re-request). |
| `GET /guilds/{id}/members`, `GET /me/guilds` | Reads. |
| `DELETE /guilds/{id}/members/{identity_id}` (kick, not role change) | Reversible via re-invite; bounded by the kicker's own `manage_members` permission. |
| `GET`/`POST /guilds/{id}/channels`, `PATCH /guilds/{id}/channels/{cid}`, `POST /guilds/{id}/channels/{cid}/archive` | Structural but reversible (unarchive, re-edit), gated on existing `manage_channels` permission. |
| `GET`/`POST /guilds/{id}/channels/{cid}/messages`, `DELETE /guilds/{id}/channels/{cid}/messages/{mid}`, `GET /guilds/{id}/channels/{cid}/messages/archive` | Chat, same reasoning as `/conversations` above. |
| `GET`/`POST /guilds/{id}/events`, `PATCH`/`DELETE /guilds/{id}/events/{eid}`, `PUT /guilds/{id}/events/{eid}/rsvp`, `GET /guilds/{id}/events/{eid}/rsvps` | Reversible scheduling state. |

**Signature-required tier** — high-blast-radius and/or hard-to-reverse
actions where a stolen bearer token alone should not be sufficient:

| Endpoint | Rationale |
|---|---|
| `POST /auth/device/approve` | Mints a brand-new full session bearer token for an entirely different device off nothing but the approver's own ambient session (see `device_pairing.rs`'s module doc). An attacker holding only a stolen bearer token could otherwise use this to plant a second, independently-usable session for themselves — the clearest concrete gap #704 named. Enforced as of #698. |
| `POST /me/devices/grants/{id}/approve` | Already followed the target pattern before #698: approval is signed with the approving device's own Ed25519 key over `device_grant_approval_signing_bytes(...)`, not just ambient-token-gated (see `devices.rs`'s module doc) — the existing precedent #698's shared helper generalizes. |
| `POST /me/passkeys/{id}/revoke` **when it is the identity's last remaining passkey** | Flagged specifically because of the lock-out angle: if a passkey ceremony can't be required (there's none left to prove), a stolen-bearer-token attacker could otherwise use this single call to strip the real owner's only way back in. Previously gated by `?confirm=true` (`passkeys::guard_revoke_last_passkey`) — a client-side speed bump, not a credential check; #698 replaced that query param with the fresh-signature requirement below (`passkeys::needs_fresh_signature`). Revoking a non-last passkey stays unsigned/ambient. |
| `PUT /me/recovery/guardians` when it *removes* a guardian or *raises* the threshold | Naming/adding guardians stays ambient (#443's existing unilateral-opt-out design); but an attacker with only a stolen bearer token silently removing real guardians or raising the threshold past what remaining guardians can satisfy would neuter the owner's actual recovery path without needing to touch recovery itself. Lowering the threshold or adding guardians is comparatively low-risk (makes recovery easier, not harder) and can stay ambient. |
| `POST /integrations/{slug}/connect` | Explicitly named in #696's decision text ("granting an integrator broad capabilities"): this is the one call that hands a third party standing permission over the identity's data going forward. |
| `PUT /guilds/{id}/permission-overrides`, `DELETE /guilds/{id}/permission-overrides/{override_id}` | Changes what an entire role/resource can do guild-wide, not just one member's standing. |
| `POST /guilds/{id}/roles`, `PATCH`/`DELETE /guilds/{id}/roles/{idx}` | Same reasoning — role definitions are the guild's permission structure, not membership churn (`GET /guilds/{id}/roles` itself is a read, already listed above). |
| `POST /guilds/{id}/transfer-ownership` | #696's own named example; today gated only by `actor == guild.owner` over the ambient session (see `guilds::transfer_ownership`) — no re-proof of the owner's identity beyond the bearer token, exactly the gap #698 targets. |
| `PATCH /guilds/{id}/members/{identity_id}` (role change) | Can grant another member owner-adjacent permissions (`manage_roles`, `manage_members`) — same "silently escalate someone else's standing" shape as a permission-override change, just scoped to one member. |

Deliberately **not** blanket-classified as signature-required despite
mutating state: sending a chat message, leaving a guild, revoking a
non-last passkey/device, or disconnecting from an integrator — each is
either fully reversible or only ever narrows the caller's own exposure,
matching #697's own invariant against a blanket
"anything-that-mutates-state-is-high-risk" rule.

**Enforcement and the canonical signing-message format (#698).** Every row
in the signature-required tier above is load-bearing server-side as of
#698 — `crates/server/src/signature_gate.rs`'s `require_fresh_signature`
is the one shared enforcement point every flagged handler calls before its
mutation, reusing `auth::verify_event_signature` rather than a new signing
scheme (the same mechanism `register_finish` already uses for
`identity.created` and `devices::approve_device_grant` for
`device_grant_approval_signing_bytes`). A signature-required request
carries `signing_key_id` (a `Uuid` naming one of the caller's own
non-revoked `identity_signing_keys` rows) and `signature` (base64 Ed25519)
alongside its ordinary fields; the server independently rebuilds the exact
byte string that key must have signed and verifies against it — a client
never gets to hand the server pre-computed "this is valid" bytes.

The signed message is always

```
avalon:<action_tag>:v1:<field1>:<field2>:...
```

built by `signature_gate::canonical_message(action_tag, fields)` — a
short, explicit, versioned tag identifying the action, followed by that
action's own load-bearing fields in a fixed order (`Display`/`to_string()`
form, colon-joined), so a signature minted for one action or target can
never verify against a different one. Per-endpoint `action_tag`s and
fields, matching each handler's own implementation:

| Endpoint | `action_tag` | Fields |
|---|---|---|
| `POST /auth/device/approve` | `device_pairing.approve` | `identity_id`, `user_code` (not the server-internal `pairing_id` — the approving client never otherwise learns it, and `user_code` alone is already fresh, single-use, and TTL'd per pairing) |
| `POST /guilds/{id}/transfer-ownership` | `guild.transfer_ownership` | `guild_id`, current `owner`, `to` |
| `PUT /guilds/{id}/permission-overrides` | `guild.permission_override.set` | `guild_id`, `role_index`, `resource_kind`, `resource_id`, `permission`, `allow` |
| `DELETE /guilds/{id}/permission-overrides/{override_id}` | `guild.permission_override.delete` | `guild_id`, `override_id` |
| `POST /guilds/{id}/roles` | `guild.role.create` | `guild_id`, `name`, normalized `permissions` (comma-joined) |
| `PATCH /guilds/{id}/roles/{idx}` | `guild.role.update` | `guild_id`, `name_index` |
| `DELETE /guilds/{id}/roles/{idx}` | `guild.role.delete` | `guild_id`, `name_index` |
| `PATCH /guilds/{id}/members/{identity_id}` (escalating only) | `guild.member_role.update` | `guild_id`, target `identity_id`, `role_index` |
| `POST /me/passkeys/{id}/revoke` (last passkey only) | `passkey.revoke_last` | `passkey_id`, `identity_id` |
| `PUT /me/recovery/guardians` (removal/raise only) | `recovery.guardians.set` | `identity_id`, new guardian ids (sorted, comma-joined), `threshold` |
| `POST /integrations/{slug}/connect` | `integration.connect` | `slug`, `capabilities` (comma-joined, request order) |

Three of these (`DELETE /guilds/{id}/permission-overrides/{override_id}`,
`DELETE /guilds/{id}/roles/{idx}`, and `POST /me/passkeys/{id}/revoke`)
previously took no request body at all; they now take a small JSON body
carrying only `signing_key_id`/`signature` (an empty `{}` is a valid body
when no signature is required for that call).

A request in the signature-required tier that omits `signing_key_id`/
`signature` entirely is rejected one of two ways, deliberately distinct so
the client gets an actionable message rather than a generic auth failure
(#698's own invariant): `NO_REGISTERED_SIGNING_KEY` if the caller's
identity has no non-revoked `identity_signing_keys` row at all (nothing to
sign with — see "losing every device" below), or
`FRESH_SIGNATURE_REQUIRED` if it has one but didn't sign this specific
request. A `signing_key_id` that doesn't resolve to a non-revoked key
owned by the caller gets `SIGNING_KEY_NOT_FOUND`; a signature that fails
to verify against the reconstructed message gets `INVALID_FRESH_SIGNATURE`.

- `crates/protocol/src/identity.rs` — `Identity { id, created_at }` and
  `Profile { identity_id, display_name, avatar_url, bio, favorite_genres,
  pronouns, banner_url, status, links, timezone, theme_color, location,
  main_guild }`
  (`bio`/`favorite_genres`/`pronouns` added by #155; `banner_url`/`status`/
  `links`/`timezone`/`theme_color`/`location` added by #372; `main_guild`
  added with no ticket), plus the
  `Genre` enum `favorite_genres` draws its fixed vocabulary from. No
  reference to any character schema, and no reference to WebAuthn/Ed25519
  either — those stay server-side implementation detail, by design.
  `location` is self-described free text only, deliberately never IP-derived
  or geocoded — see the field's own doc comment before adding anything that
  would make it so.
- `crates/protocol/src/ids.rs` — `IdentityId(Uuid)`.
- `crates/server/src/handlers.rs` — `register_start`/`register_finish`,
  `session_start`/`session_finish`, `me`, `update_profile`, working end-to-end
  against Postgres. `register_finish` verifies both the WebAuthn ceremony and
  the Ed25519 event signature before writing anything, and enqueues
  `identity.created` into the outbox in the same transaction as the
  identity/profile/key rows (#71, done for this path). `update_profile`
  emits `profile.updated` (#86) whenever a promised-durable field changes,
  now including `bio`/`favorite_genres`/`pronouns` (#155). `list_profiles`
  (`GET /identities/profiles?ids=…`, issue #161) resolves *other*
  identities' public profile fields (`display_name`, `avatar_url`) in a
  batch — the gap every roster surface (friends, guild
  members) previously had to leave as a raw identity id. Deliberately does
  NOT include `bio`/`favorite_genres`/`pronouns`: this endpoint has no
  further visibility gating beyond session auth (any identity can
  batch-resolve arbitrary ids), so it stays at the same minimal exposure
  level it had before #155 rather than silently widening what any stranger
  can bulk-collect. Exposing those fields there, if ever wanted, is a
  scoping decision for its own ticket.
- `apps/hub/src/views/Profile.vue` (#277) — surfaces and edits
  `bio`/`favorite_genres`/`pronouns` on the user's own profile, closing
  the gap #155 left open: the server has supported all three fields since
  #155, but the Hub's own type layer (`apps/hub/src/api/types.ts`) never
  declared them and `Profile.vue` never rendered them, so they were only
  ever reachable by calling `PATCH /me` directly. Bio/pronouns reuse the
  same per-field `AvalonEditableField` save pattern `display_name`/
  `avatar_url` already use; `favorite_genres` is a fixed checkbox picker
  over the closed `Genre` vocabulary (capped at 5 client-side, matching
  `MAX_FAVORITE_GENRES`) with its own explicit Save, mirroring the
  recovery-guardians picker on the same page. Editable here on one's own
  profile only; readable on another identity's profile card via
  `get_identity_profile` below (#403), never editable there.
- `crates/server/src/handlers.rs`'s `get_identity_profile`
  (`GET /identities/{id}/profile`, issue #403 — decided) — a **separate,
  single-identity** endpoint from `list_profiles` above, deliberately not a
  widening of it: exposes the same fields `GET /me` already does
  (`bio`/`favorite_genres`/`pronouns`/`banner_url`/`status`/`links`/
  `timezone`/`theme_color`/`location`/`main_guild`/`effective_main_guild`)
  for exactly one identity per request, so `list_profiles`'s batch-lookup
  exposure stays exactly as narrow as it was. Omits `discoverable` — that
  field is the *viewed* identity's own search-visibility setting, not
  something the viewer needs. 404s via `AppError::IdentityNotFound` for an
  id that doesn't exist.
- `apps/hub/src/views/UserProfile.vue` (#393, widened by #403) — a
  read-only profile card for *another* identity, reachable by clicking a
  friend row or a guild member row. Now calls `get_identity_profile` above
  and renders the full self-description fields plus live presence. Still
  no shared-guilds list — no endpoint exposes another identity's guild
  memberships at all yet, out of #403's scope.
- `crates/server/src/auth.rs` — builds the `Webauthn` instance
  (`AVALON_WEBAUTHN_RP_ID`/`AVALON_WEBAUTHN_ORIGIN`), verifies Ed25519 event
  signatures, and still generates opaque session tokens (that part never
  needed to change). Carries its own in-process tests exercising a full
  register-then-authenticate ceremony against a virtual authenticator, no
  Postgres required.
- `crates/server/src/outbox.rs` — the outbox pattern: `enqueue` inside a
  transaction, a background worker that drains pending rows into
  `avalon-chain`, `status` for `avalon outbox-status`.
- `crates/server/db/migrations/0001_identity_and_auth/` — `identities`,
  `profiles`, `identity_keys` (passkeys), `identity_signing_keys` (Ed25519),
  `webauthn_ceremonies` (ephemeral ceremony state), `sessions`. No
  `credentials` table anymore.
- `crates/server/db/migrations/0003_outbox/` — `protocol_outbox`.
- `crates/server/db/migrations/0007_device_grants/` — `identity_signing_keys.revoked_at`
  and `device_grants` (#135); see the signing-key section above.
- `crates/server/db/migrations/0022_additional_passkeys/` — widens
  `webauthn_ceremonies.kind`'s check constraint to allow `'add_passkey'`
  alongside the existing `'registration'`/`'authentication'` (#200); no
  other schema change needed, since `identity_keys` already supported
  multiple rows per identity, `label` included, from migration 0001.
- `crates/server/tests/hybrid_transport.rs` (#397, `--ignored`, live) —
  confirms `/identities/register/start` and `/sessions/start` set neither
  `authenticatorAttachment` nor a credential `transports` restriction (the
  two levers that would suppress hybrid transport), and that a credential
  from a generic virtual authenticator registers and logs in exactly like
  any other. See "Hybrid transport" above for what this does and does not
  verify.
- `crates/server/src/passkeys.rs` (#200) — `POST /me/passkeys/register/start`,
  `POST /me/passkeys/register/finish`, `GET /me/passkeys`,
  `PATCH /me/passkeys/:id` (rename), `POST /me/passkeys/:id/revoke[?confirm=true]`.
  Session-gated, not identity-creation-gated: `register_start` excludes the
  caller's already-registered credential ids from the ceremony so the same
  physical key can't silently re-register itself, and both `register_start`/
  `register_finish` re-derive the identity from the session on every call
  rather than trusting the ceremony's own stored identity id. Revoking a
  passkey is a hard delete (no `revoked_at` column here, unlike
  `identity_signing_keys`) — nothing else references an `identity_keys` row
  by id, and no protocol event is emitted either way, since passkeys are
  operational state, not promised-durable (see the durability table above).
  Revoking the identity's last remaining passkey without `?confirm=true`
  returns 409; the count check and the delete happen inside one transaction
  with a `SELECT ... FOR UPDATE` so a concurrent revoke from another session
  can't race two unconfirmed revokes past the guard at once.
- `crates/server/src/devices.rs` (#135) — `POST /me/devices/grants`,
  `GET /me/devices/grants[?status=]`, `GET /me/devices/grants/:id`,
  `POST /me/devices/grants/:id/approve`, `GET /me/devices`,
  `PATCH /me/devices/:id` (rename, #145), `POST /me/devices/:id/revoke`.
  The first device's `identity_signing_keys` row is labeled at
  registration too (`register_finish`'s optional `device_label`, #145) —
  previously only devices added through a grant carried a label.
- `crates/server/db/migrations/0029_social_recovery/` — `recovery_guardian_settings`
  (per-identity threshold), `recovery_guardians` (the guardian set, a
  friend-only rule enforced at the handler layer against `friendships`, not
  a DB constraint), `recovery_requests` (one row per attempt, with a
  partial unique index capping one *active* — `pending_approvals`/`delay` —
  attempt per identity), and `recovery_approvals`; widens
  `webauthn_ceremonies.kind` to allow `'recovery_start'` (#201).
- `crates/server/src/recovery.rs` (#201) — the full guardian-configuration
  and recovery-request state machine described above:
  `PUT`/`GET /me/recovery/guardians`, `POST /recovery/requests/start`,
  `POST /recovery/requests/finish`, `POST /recovery/requests/:id/approve`,
  `POST /recovery/requests/:id/cancel`, `POST /recovery/requests/:id/finalize`,
  `GET /recovery/requests/:id`, `GET /identities/:id/recovery/status`,
  `GET /me/recovery/status`, `GET /me/recovery/guardian-requests`. Every
  security-load-bearing invariant (threshold enforcement, delay
  enforcement, veto authority, rate limiting) is factored into a pure,
  unit-tested function, same convention `passkeys::guard_revoke_last_passkey`
  established. `crates/server/tests/recovery.rs` (`--ignored`) covers the
  full flow against a live server: a 3-guardian 2-of-3 recovery, a lone
  guardian below threshold never finalizing, an owner veto, a removed
  guardian losing approve/cancel authority, and guardian-set changes
  requiring a session.
- `crates/cli/src/main.rs` — `avalon create-identity` drives a real WebAuthn
  registration via a virtual authenticator (`passkey-authenticator`'s
  `testable` feature) and prints the loss-of-everything warning #99 calls
  for; it also persists that virtual passkey locally
  (`_running/keys/<id>.passkey.json`) so `avalon login <identity_id>` can
  reload it into a fresh virtual authenticator and drive a real login
  ceremony later, printing a session token — a dev/test convenience, not a
  pattern for real deployment. `avalon outbox-status`.
- `crates/sdk/src/lib.rs` — `AvalonClient::authenticate()` exchanges an identity
  token for an integrator-scoped `Session`, unchanged by any of this — an integrator never
  creates identities or logs an identity in itself.
- `apps/hub/src/crypto/webauthn.ts` — the real browser WebAuthn ceremonies via
  `@simplewebauthn/browser`, verified field-for-field against
  `crates/server/src/handlers.rs`'s request/response shapes (both follow the
  same base64url/camelCase WebAuthn JSON convention, no adapter layer
  needed). `apps/hub/src/crypto/signingKey.ts` — Ed25519 signing via
  `@noble/curves`, keys derived from a BIP39 mnemonic (`@scure/bip39`) per
  the section above, storage unchanged. `apps/hub/src/api/identity.ts` ties
  it together with the `/identities/register/*` and `/sessions/*` API calls
  into `createIdentity()`/`login()`/`recoverSigningKey()`.
  `CreateIdentity.vue` shows the mnemonic once, right after the identity id,
  and asks for an optional device label (#145) passed through to
  `register_finish`. `apps/hub/src/api/passkeys.ts` (#200) orchestrates the
  authenticated add-a-passkey ceremony the same way `identity.ts`'s
  `createIdentity()` orchestrates the unauthenticated one. `Profile.vue` has
  the recovery form and the #135 grant-request form, shown only when the
  current device has no signing key stored for the logged-in identity; once
  it does, "Your devices" lists every registered device with rename and
  revoke, and explains how to add another one (#145). Separately, a
  "Passkeys" card (#200) always lists every registered passkey with rename
  and revoke, plus an "Add another passkey" button — visible regardless of
  signing-key state, since a passkey and a signing key are unrelated
  credentials (see the section above). Revoking the last remaining passkey
  surfaces the server's 409 as a plain `window.confirm` prompt before
  retrying with `?confirm=true`. `CreateIdentity.vue` also shows a
  non-dismissible total-loss warning (`@avalon/ui`'s `AvalonWarningBanner`,
  #199) right after the first passkey is created — losing that device with
  no second one registered means permanently losing the identity and
  everything durable it carries. The same warning resurfaces on the
  Passkeys card in `Profile.vue` for as long as `GET /me/passkeys` reports
  exactly one passkey (`apps/hub/src/utils/singlePasskeyWarning.ts`'s
  `shouldShowSinglePasskeyWarning`), not a one-time dismiss — it disappears
  the moment a second passkey is registered.
- `apps/hub/src/api/recovery.ts` (#201) orchestrates guardian configuration
  and the recovery-initiation ceremony the same way `api/passkeys.ts`
  orchestrates add-a-passkey — `startRecovery()` is the one function in the
  Hub's API layer that deliberately never sends a bearer token. `Profile.vue`
  gained a "Recovery guardians" card (a friend checklist plus an M-of-N
  threshold stepper, `packages/ui`'s `AvalonCard`/`AvalonButton` only — no
  new library component), an owner-visible in-progress banner backed by
  `GET /me/recovery/status` (polled the same cadence as the existing
  device-grant list), and a "Recovery requests to approve" card for when
  the logged-in identity is currently a guardian for someone else
  (`GET /me/recovery/guardian-requests`, approve/cancel inline).
  `RecoverIdentity.vue` (routed at `/recover-identity`, linked from
  `Login.vue`) is the unauthenticated initiation flow: enter the identity
  id, drive the real WebAuthn ceremony, then poll the public
  `GET /identities/:id/recovery/status` for status — no session anywhere
  on this page.
- **Guardian consent (#443)** — naming a guardian via `PUT
  /me/recovery/guardians` is still unilateral (opt-out, not opt-in: a named
  guardian is active immediately, same as before), but a guardian can now
  discover every identity relying on them (`GET
  /me/recovery/guardian-of`) and remove themselves from any one of those
  designations without the owner's cooperation (`DELETE
  /me/recovery/guardian-of/{identity_id}`). A self-removal that drops the
  owner below their configured threshold clamps the threshold down to the
  new guardian count rather than leaving an unsatisfiable M-of-N. Surfaced
  in the Hub as a "You're a recovery guardian for" card on `Profile.vue`,
  alongside the existing "Recovery requests to approve" card.
- `crates/server/db/migrations/0042_device_pairings/` — `device_pairings`
  (#307): `device_code`/`user_code`, `status`
  (`pending`/`approved`/`denied`/`expired`), nullable `identity_id`/
  `session_token` (set only on approval), `expires_at`, `last_polled_at`.
- `crates/server/src/device_pairing.rs` (#307) — `POST /auth/device/start`,
  `POST /auth/device/poll`, `POST /auth/device/approve`,
  `POST /auth/device/deny`; see the cross-device pairing section above.
  `crates/server/tests/device_pairing.rs` (`--ignored`) covers the full
  `start → approve → poll` round trip against a live server, plus wrong
  `user_code`, denial, single-use consumption, and poll-rate limiting.
- `crates/cli/src/dev_tools.rs` (#307) — `avalon pair-device` drives the
  `start`/`poll` side of the flow as a stand-in incapable client.
- `apps/hub/src/views/PairDevice.vue` (#307), routed at `/pair` (matching
  `verification_uri`'s `?user_code=` shape) — a `user_code` field plus
  approve/deny buttons, using the user's existing authenticated Hub
  session; `apps/hub/src/api/client.ts`'s `approvePairing`/`denyPairing`.

## Decisions and tickets

- [#67](https://github.com/LunarVagabond/avalon-protocol/issues/67) — ADR:
  identity is separate from game characters.
- [#75](https://github.com/LunarVagabond/avalon-protocol/issues/75) — ADR:
  durable history is canonical; the table above follows from it.
- [#73](https://github.com/LunarVagabond/avalon-protocol/issues/73) — identity
  as a self-custodied keypair. Done.
- [#71](https://github.com/LunarVagabond/avalon-protocol/issues/71) —
  identity creation and its ledger entry weren't atomic. Done for the
  identity path; the outbox pattern generalizes to every future emitter.
- [#99](https://github.com/LunarVagabond/avalon-protocol/issues/99) — decided:
  identity recovery when every passkey is lost (multi-device, done via
  #200; guardian social recovery, done via #201; custodial fallback
  opt-in-only, never a default, not built). Tracked as
  [#198](https://github.com/LunarVagabond/avalon-protocol/issues/198), an
  epic under #2.
- [#200](https://github.com/LunarVagabond/avalon-protocol/issues/200) —
  multi-device/multi-passkey registration, #99's cheap near-term mitigation.
  Done: `crates/server/src/passkeys.rs`, `apps/hub/src/api/passkeys.ts`.
- [#201](https://github.com/LunarVagabond/avalon-protocol/issues/201) —
  social recovery via an M-of-N set of trusted guardians, #99's real answer
  for losing every device at once. Done: `crates/server/src/recovery.rs`,
  `crates/server/tests/recovery.rs`, and (#394) the recovering device's own
  finalize step in `apps/hub/src/views/RecoverIdentity.vue` — the flow
  could be started and approved from the Hub but never actually completed
  until this. Scoped out this pass: a background auto-finalize sweep
  (finalize is currently caller-triggered, not self-triggering), Rust SDK /
  C# binding surface — see the section above for the full scoping
  rationale.
- [#199](https://github.com/LunarVagabond/avalon-protocol/issues/199) —
  onboarding/settings total-loss warning for a single-passkey identity, part
  of [#198](https://github.com/LunarVagabond/avalon-protocol/issues/198).
  Done: `apps/hub/src/views/CreateIdentity.vue`, `apps/hub/src/views/Profile.vue`,
  `packages/ui`'s `AvalonWarningBanner`.
- [#55](https://github.com/LunarVagabond/avalon-protocol/issues/55) — Hub
  identity creation/login UI: the real WebAuthn + Ed25519 flow, in-browser.
- [#122](https://github.com/LunarVagabond/avalon-protocol/issues/122) —
  release-blocking decision: Ed25519 signing-key custody. See the section
  above; [#134](https://github.com/LunarVagabond/avalon-protocol/issues/134)
  and [#135](https://github.com/LunarVagabond/avalon-protocol/issues/135)
  are the two halves it decided on.
- [#86](https://github.com/LunarVagabond/avalon-protocol/issues/86) — profile
  updates still emit no event; classify promised-durable identity state.
- [#307](https://github.com/LunarVagabond/avalon-protocol/issues/307) —
  cross-device pairing for a WebAuthn-incapable client. Done:
  `crates/server/src/device_pairing.rs`, `apps/hub/src/views/PairDevice.vue`,
  `avalon pair-device`. See the section above.
- [#397](https://github.com/LunarVagabond/avalon-protocol/issues/397) —
  verify and document native WebAuthn hybrid transport. Done at the
  configuration/test level: `crates/server/tests/hybrid_transport.rs`
  confirms nothing in Avalon's own `webauthn-rs` config or handlers
  restricts or special-cases it, and `crates/sdk/src/lib.rs::authenticate()`
  is confirmed unaffected by construction (#51). Not done, and needing a
  human with a real browser and phone: an actual end-to-end hybrid ceremony
  was never run. See the section above.
- [#395](https://github.com/LunarVagabond/avalon-protocol/issues/395) —
  credential-friction decision #397 feeds into.
- [#613](https://github.com/LunarVagabond/avalon-protocol/issues/613) —
  decided: end-user portability is mirrored history, not a movable export
  bundle — no export/import primitive is planned. See "Portability means
  mirrored, not movable" above.
- [#620](https://github.com/LunarVagabond/avalon-protocol/issues/620) —
  decided: proving identity to a node you've never registered a passkey
  on, via a signed, human-approved, cross-node assertion extending #307's
  approve/deny pattern, rather than a shared login domain. Tracked as
  epic [#623](https://github.com/LunarVagabond/avalon-protocol/issues/623).
  See "Portability means mirrored, not movable" above.
- [#622](https://github.com/LunarVagabond/avalon-protocol/issues/622) —
  open: minimum replication guarantee for identity-bearing shards, so a
  single operator's node disappearing can't strand the identities that
  live there. #620/#623's locator assumes this has a real answer, not
  just best-effort opt-in mirroring.
- [#2](https://github.com/LunarVagabond/avalon-protocol/issues/2) — Epic:
  Identity & Player Profile.
- [#696](https://github.com/LunarVagabond/avalon-protocol/issues/696) — Epic:
  unify the SDK schema across languages with tiered action signing (decided
  on [#695](https://github.com/LunarVagabond/avalon-protocol/issues/695)):
  [#697](https://github.com/LunarVagabond/avalon-protocol/issues/697) the
  ambient-token/signature-required classification (this doc's own "Today in
  the repo" section), [#698](https://github.com/LunarVagabond/avalon-protocol/issues/698)
  server enforcement, [#704](https://github.com/LunarVagabond/avalon-protocol/issues/704)
  two concrete gaps #697 found and #698 closed. See
  [sdk.md](../../rust-sdk/architecture/sdk.md)'s own "Decisions and tickets" for the companion
  per-language `AccountSession` tickets (#699/#700/#701/#707).
