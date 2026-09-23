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
display name (the globally-unique, case-insensitive handle itself — there is no
separate discriminator suffix), avatar, bio (free text, capped at 500 characters),
favorite genres (drawn from a fixed, small vocabulary — `avalon_protocol::identity::Genre`
— not free text, capped at 5 entries), pronouns (free text, capped at 40 characters),
a banner image (same shape/validation as avatar), a short status line (capped at 100
characters), a self-reported list of up to 5 links (each an `http`/`https` URL, capped
at 200 characters), a self-reported timezone (free text, capped at 64 characters — not
validated against the real IANA time zone database, a known gap), an accent color
(`^#[0-9a-fA-F]{6}$`), and a free-text location. None of it is an authoritative
integrator fact — `location` in particular is self-described text only ("Pacific
Northwest," say), never IP-derived or geocoded; nothing in this protocol infers where a
user physically is.

It also carries `main_guild`: a self-chosen pointer to one of the identity's own
current guild memberships, so an integrator building a guild-chat-style UI has one
guild to default to instead of having to support arbitrarily-many simultaneous
memberships — see [`./guilds.md`](./guilds.md#a-users-main-guild). Setting it is
checked against a real fact (current membership), not just validated for shape; and
unlike every other field here, `null` doesn't mean "no guild" — a caller wanting a
default in that case reads `GET /me`'s `effective_main_guild`, computed at read time
as the earliest guild membership joined, never written back into `main_guild` itself.

Someone writing "I am an Avion" in their bio does not make Avion a
network-level race. An integrator can display that, interpret it, or ignore it. Facts
about what an identity *has done* come from issuer attestations with
provenance (see [`./provenance.md`](./provenance.md)), never from the profile.

The profile is deliberately small. It is not where game-specific data lives — that
boundary between identity and game characters is foundational to the protocol.

## What is promised durable

Anything Avalon promises to preserve must be reconstructable from protocol history,
and every change to it must emit a protocol event in the same unit of work as the
projection change.

Unless a row says otherwise, every optional field below shares one convention: `null`
in the `profile.updated` payload means the field was explicitly cleared, and an absent
key means it was untouched.

| State | Promised durable? | Canonical record | Notes |
|---|---|---|---|
| identity exists, `created_at` | yes | `identity.created` | self-signed, see below |
| `display_name` | yes | `identity.created` (initial), `profile.updated` (changes) | emitted in the same transaction as the `profiles` row, via the outbox |
| `avatar_url` | yes | `profile.updated` | — |
| `bio` | yes | `profile.updated` | free text, capped at 500 characters |
| `favorite_genres` | yes | `profile.updated` | fixed, small controlled vocabulary (`Genre`), capped at 5 entries; unknown values rejected, not dropped; a present key always fully replaces the list, including to `[]` |
| `pronouns` | yes | `profile.updated` | free text, capped at 40 characters |
| `banner_url` | yes | `profile.updated` | same shape/validation as `avatar_url`, a separate image slot |
| `status` | yes | `profile.updated` | free text, capped at 100 characters |
| `links` | yes | `profile.updated` | up to 5 self-reported URLs, each capped at 200 characters and required to parse as an `http`/`https` URL; a present key always fully replaces the list, including to `[]` |
| `timezone` | yes | `profile.updated` | free text, capped at 64 characters; not validated against the real IANA time zone database (a documented gap) |
| `theme_color` | yes | `profile.updated` | must match `^#[0-9a-fA-F]{6}$` |
| `location` | yes | `profile.updated` | free text, capped at 100 characters, self-described only — never IP-derived or geocoded |
| `main_guild` | yes | `profile.updated` | a pointer to one of this identity's own current guild memberships; must name a guild the identity is currently a member of, checked server-side against `guild_members`; also cleared automatically, in the same transaction, if the identity leaves the guild it points at |
| future title / labels | classify when added | `profile.updated` | the rule: promised-durable means it emits, or it isn't promised |
| WebAuthn passkey(s) | yes | `identity.passkey_registered`/`.passkey_revoked` | `identity_keys` table (authoring node's own local source of truth) / `indexer_identity_passkeys` projection (what a mirror-only node reconstructs from replayed history alone); public credential material only, never anything secret. A passkey registered before this event existed has no such event — it still works for local login on the node it was registered on, it just isn't verifiable from a different node's mirrored history |
| event-signing public key | yes | `identity.signing_key_added`/`.signing_key_revoked` | `identity_signing_keys` table / `indexer_identity_signing_keys` projection, same authoring-vs-mirror split as passkeys above. This is a separate durable fact from `identity.created`'s issuer field, which is a `GlobalId` string, never the actual key bytes |
| credentials (password hash) | **no**, pruned entirely | — | there is no password authentication anywhere in this protocol |
| sessions / tokens | **no**, still ephemeral server state — see below for the additive exception | — | the opaque `sessions`-table bearer token itself is node-local; a separate, short-lived, self-signed continuation credential exists that isn't stored anywhere at all (verified statelessly against the durable signing key above plus a one-time-use nonce row) |
| presence | **no** | — | [`./presence.md`](./presence.md) |
| visibility settings | no, unless later promoted | — | [`./privacy.md`](./privacy.md) |

A session token or any other shared secret is never part of an event payload — there
is no shared secret at all. The `identity.created` event carries no `username`.

## Authentication: two keys, two jobs

An identity is a self-custodied keypair — two of them, in fact, each doing a
different job, matching how real passkey-based wallets are actually built
(the passkey is a secure *unlock*, a separate key is the actual *signer* —
WebAuthn's challenge is deliberately not a general-purpose signing oracle, so it
can't do both jobs at once):

- **A WebAuthn passkey** (`identity_keys` table) proves interactive presence —
  "the holder of this device authorized this request, right now." This is the entire
  login mechanism: register a passkey once, then a normal WebAuthn ceremony each time
  after. Multiple passkeys per identity are supported by the schema, and an
  authenticated identity can register additional ones after the fact: any registered
  passkey authenticates the identity, none is privileged over another, and each is
  independently nameable and revocable. Registering or revoking a passkey emits a
  durable `identity.passkey_registered`/`.passkey_revoked` event carrying its public
  credential material — so a node that only ever mirrored this identity's ledger
  history can independently verify a fresh login for it, not just the node the passkey
  was originally registered on.
- **A raw Ed25519 key** (`identity_signing_keys` table) proves authorship of a
  specific durable event. It signs `identity.created` at registration —
  `issuer` on that event is `identity:<id>:self:created`, not
  `network:avalon-server:...` — so a hosted node can never fabricate an
  identity that never actually registered, the same guarantee that stops a
  node fabricating an integrator's attestation (see
  [`./security-model.md`](./security-model.md)). Losing this key alone is not
  catastrophic the way losing every passkey is: the identity still logs in,
  and can rotate to a new signing key from an authenticated session.

Login is identity-id-first, not fully usernameless: the WebAuthn library's
convenience registration path hardcodes non-resident credentials, so true
discoverable ("tap your passkey, no identifier at all") login would need
*attested resident keys* — a heavier, attestation-verifying registration path
not currently built. The meaningful property holds regardless: no shared secret, a
real challenge-response proof every time.

### Two authorization tiers: ambient session vs. a fresh signature

The two keys above answer "how does this identity log in" and "how does this identity
author a durable event." A third tier exists for the question "once an identity
already holds a session, how much can that ambient bearer token alone actually do" —
not every action an authenticated session can request deserves the same trust. Most
first-party account actions (reads, chat, presence, profile edits, ordinary guild
membership churn) are authorized by the session's bearer token alone. A smaller set of
actions with real blast radius or that are hard to reverse — guild ownership transfer,
role/permission-structure changes, approving a brand-new device into an identity,
revoking the last remaining passkey, granting an integrator standing capabilities, and
a few more — additionally require a *fresh* signature from the identity's own
locally-held Ed25519 signing key (the same key above, reused rather than a new
mechanism) at the moment of the action, proving the request came from a device that
actually holds that key, not just whatever holds a copy of the bearer token. A stolen
session token alone is not sufficient for that second tier.

This applies uniformly across every first-party client — the Hub web app and every
SDK's `AccountSession` type (Rust, C#, TypeScript) mint that signature automatically
wherever it's required; a caller never hand-constructs one. It does not create any new
way for a capability-gated integrator credential to escalate into account-level power
— that boundary (`IntegratorSession`/the integrator `Session` in each SDK) is
unaffected.

`crates/server/src/signature_gate.rs`'s `require_fresh_signature` is the shared
enforcement point every flagged handler calls before its mutation, reusing the same
event-signature verification mechanism `register_finish` already uses for
`identity.created`. A signature-required request carries `signing_key_id` (a `Uuid`
naming one of the caller's own non-revoked `identity_signing_keys` rows) and
`signature` (base64 Ed25519) alongside its ordinary fields; the server independently
rebuilds the exact byte string that key must have signed and verifies against it — a
client never gets to hand the server pre-computed "this is valid" bytes. The signed
message is always `avalon:<action_tag>:v1:<field1>:<field2>:...`, built from a short,
explicit, versioned action tag followed by that action's own load-bearing fields in a
fixed order, so a signature minted for one action or target can never verify against a
different one.

Actions requiring a fresh signature include: approving a device-pairing request
(mints a brand-new session for a different device off nothing but the approver's
ambient session); approving a device-signing-key grant; revoking an identity's last
remaining passkey; removing a recovery guardian or raising the recovery threshold;
connecting to an integrator (granting it standing capabilities); creating, updating, or
deleting a guild role or a guild permission override; transferring guild ownership; and
changing another guild member's role; and reversing an event through post-compromise
rollback (`rollback.reverse`, signed over `event_id`, `identity_id`, `since`). Everything else — reads, chat, presence updates,
reversible social-graph edits, guild membership churn (invite/accept/leave/kick),
channel/event CRUD gated by an existing permission, and revoking a non-last passkey or
device — stays authorized by the ambient session alone, since it is either fully
reversible or only ever narrows the caller's own exposure.

A request in the signature-required tier that omits `signing_key_id`/`signature`
entirely is rejected with `NO_REGISTERED_SIGNING_KEY` (nothing to sign with) or
`FRESH_SIGNATURE_REQUIRED` (a key exists but didn't sign this request); a
`signing_key_id` that doesn't resolve to a non-revoked key owned by the caller gets
`SIGNING_KEY_NOT_FOUND`; a signature that fails to verify gets `INVALID_FRESH_SIGNATURE`.

### Cross-device pairing for a WebAuthn-incapable client

WebAuthn login above assumes the client has some ceremony surface — a browser, or a
platform authenticator. A game engine with no embedded browser, a console, or any
other headless client doesn't, and never needs to: the SDK's `authenticate()` already
takes a bearer session token, not a live WebAuthn exchange, so the only real gap is
*how such a client obtains that token in the first place*. Cross-device pairing
(`crates/server/src/device_pairing.rs`) solves it the way platform account systems
solve the identical problem: the incapable client requests a pairing (`POST
/auth/device/start`, unauthenticated) and gets back a short, human-typeable
`user_code` (shown to the user, e.g. as a QR code pointing at `verification_uri`) plus
an opaque `device_code` it alone holds. The user completes a real WebAuthn login on a
capable device — the Hub, in a browser — and approves the pairing there (`POST
/auth/device/approve`); the waiting client polls (`POST /auth/device/poll`, bearer =
`device_code`) until it receives an ordinary session token, minted through the exact
same mechanism a normal login uses — not a second, differently-trusted token type.

The security boundary is deliberately not the `user_code`'s secrecy: approval requires
the *approver's own already-authenticated session*, so there is no path from "knows
the user_code" alone to a minted session. The `user_code` only disambiguates which
pending pairing to act on; it still carries real entropy (8 chars from an alphabet
with `0`/`O`/`1`/`I`/`L` removed), a ~10-minute expiry, and single-use poll delivery
(an approved pairing's token is returned exactly once; every later poll of that
`device_code` gets `expired`), but none of those are the reason this is safe — the
authenticated-approver requirement is. This solves a genuinely different problem than
the device-signing-key grant flow below — bootstrapping a session for a client with
*no* prior session at all, versus adding a trusted signing device to an identity
that's already authenticated somewhere — so it lives in its own module and its own
`device_pairings` table. No password field or password-shaped persistent secret is
introduced anywhere in this flow. `avalon pair-device` (`crates/cli`) drives the
`start`/`poll` side as a stand-in incapable client, for testing this without a real
console/engine. The SDK's `AvalonClient::login()`/`DeviceLogin::wait()` is the real
integration surface for the waiting side, resolving straight to a `Session`.

### Recovery when every passkey is lost

A layered answer, since the options aren't mutually exclusive. Multi-device/
multi-passkey registration (add a second passkey at onboarding or any time after) is
the cheap, near-term mitigation for the common single-device-loss case — it does
nothing for someone who only ever registers one passkey and then loses it, which is
exactly why it's a mitigation, not the full answer. Social recovery via an M-of-N set
of trusted guardians — drawn from the identity's own Avalon friends, gated by a
mandatory public time-delay so the real owner can veto a malicious attempt — is the
real answer for losing every device at once (see below); it composes naturally with
the social graph already being network-owned and avoids a centralized custodian. An
opt-in custodial fallback (email/SMS) stays explicitly off the table as a *default*:
it reintroduces exactly the shared-secret, centralized-trust surface this design
exists to eliminate, and would only ever ship as a clearly-labeled, separately
opted-into weaker-security tier, never silently. Onboarding must make the total-loss
consequence of relying on a single passkey loud and explicit for an identity owner who
hasn't configured guardians, not a buried settings toggle discovered only after
someone has already lost everything.

Separately open: migrating an existing username/password identity (none exist outside
development, so not currently applicable), and whether identities can be transferred.

### Hybrid transport ("use a phone or tablet")

WebAuthn recognizes three authenticator categories: platform (Touch ID, Windows Hello
— built into the device doing the ceremony), cross-platform hardware (a USB/NFC
security key), and **hybrid transport** — the browser shows a QR code, a nearby phone
proves proximity over Bluetooth and unlocks its own resident passkey, and the
desktop/browser session is authenticated through it without the passkey ever leaving
the phone. This matters on a desktop browser with no platform authenticator and no
hardware key handy — incognito/private mode commonly disables platform authenticators
too — since hybrid transport is otherwise the only way such a browser can use a
passkey at all.

This is a different problem from cross-device pairing above: pairing is for a client
with *no* WebAuthn/browser surface whatsoever (a game engine, a console). Hybrid
transport is a built-in feature of an ordinary WebAuthn-capable browser and OS —
Avalon doesn't implement it, doesn't need to, and can't disable it short of explicitly
restricting authenticator attachment.

Neither of the two levers that would suppress hybrid transport (`authenticator_attachment`
on registration, or a `transports` hint on the stored credential) is set anywhere in
`crates/server/src/auth.rs`/`handlers.rs`, confirmed by inspecting the raw JSON
`/identities/register/start` and `/sessions/start` return, and by a live test
completing a full register-then-login round trip through a generic (non-platform)
virtual authenticator (`crates/server/tests/hybrid_transport.rs`, `--ignored`).
`require_resident_key(false)` is orthogonal — it controls whether the browser must
create a *resident* credential, not which transport may satisfy the ceremony.

What this does not establish: an actual end-to-end hybrid ceremony — scanning a QR
code with a real phone over real Bluetooth — has not been run. That requires a real,
non-headless browser and a real phone; the configuration-level check above is the
strongest verification available without that hardware, not a substitute for it.

This entire question is invisible to the SDK: an integrator never performs a WebAuthn
ceremony through the SDK at all — `authenticate()` exchanges an already-issued bearer
session token for `GET /me`/`GET /me/grants`, nothing else. Which authenticator
category produced that token has no representation anywhere in that exchange, by
construction, not by omission.

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
- The mnemonic itself is shown to the user exactly once, at creation, and is never
  stored anywhere — any device holding it can re-derive the identical key offline,
  with no server round-trip (the Profile page's "recover your signing key" section,
  shown when the current device has none). The server only ever sees the resulting
  *public* key.

This mnemonic recovery is the fallback path. The primary path is a
device-registration/linked-device grant model, no phrase in the common case: each
device gets its own signing keypair (`identity_signing_keys`, already multiple rows
per identity), and a grant only ever *authorizes* a new device's public key — it never
transfers a private key. An already-trusted device (`crates/server/src/devices.rs`)
approves a new one by signing the grant with its own key; the server verifies that
signature against the approver's still-active key before registering the new one.
Revocation (`POST /me/devices/:id/revoke`) is unilateral — any authenticated session
for the identity can revoke any signing key, including its own, without the revoked
device's cooperation. Recovering or granting a signing key never by itself
authenticates a login — that stays a separate concern from the passkey (login
credential). The WebAuthn passkey has no equivalent client-storage decision here: it
never leaves the platform authenticator, already synced across devices by whatever
passkey provider the identity's owner uses. The passkey's *public* credential
material (never anything secret) is durable/mirrored, a distinct fact from this
section's signing-key custody story.

### Session continuation across nodes

An opaque `sessions`-table bearer token is node-local by construction — minted by
whichever node ran the WebAuthn login, checked only against that node's own `sessions`
table. If that node goes offline, the token is dead even though every other trusted
node may have fully mirrored the identity's ledger history and could otherwise serve
it fine.

The fix doesn't touch the opaque token at all — it adds a second, additive credential
kind a client can present in the same `Authorization: Bearer` slot: a
**session-continuation token** (`avalon_protocol::continuation::ContinuationToken`), a
short-lived (60 seconds by default), self-signed assertion `{identity_id,
signing_key_id, nonce, issued_at, expires_at, signature}`, minted entirely client-side
with the identity's own Ed25519 event-signing key — the same key it already holds for
authoring events. No server ever issues one; a node's only job is verification.

Verification (`crates/server/src/continuation.rs`, wired into every existing
session-gated route with no per-route changes) needs nothing but: the token's own
claimed fields (checked for a sane, not-too-long expiry window and not already
expired); the signing key's public half, read from the durable
`identity_signing_keys` projection — the *same* projection whether this node authored
the key locally or only ever mirrored it; and a one-time-use nonce, checked against a
node-local anti-replay table (not itself durable/mirrored — a continuation token that
got replayed against a different node before its 60-second window closed would still
succeed there, a narrow, accepted residual risk rather than building cross-node nonce
coordination for it).

A continuation token is never a login credential by itself — it only extends an
already-established session; nothing routes it into registration or session-start,
which authenticate via a real WebAuthn ceremony. Revoking the signing key makes every
future continuation token minted with it fail immediately, on any node, the same
durable-revocation guarantee passkeys get.

On the client side, the identity's signing key only ever lives in the Hub's browser
storage — third-party games/tools using the SDK receive an already-authenticated
session token, never the raw key, so minting happens in the Hub's own client code. The
trigger lives centrally in the Hub's one API request function: a 401 against whatever
token was passed is retried exactly once, with a freshly-minted continuation token,
but only when that token is exactly the currently-persisted opaque session token —
the specific case a 401 there can mean "this session's origin node doesn't recognize
this token," whether because the viewer explicitly switched server URLs or the
original node went offline and a different one is now answering at the same URL. The
minted token is never persisted back into the opaque-token storage slot. WebSocket
reconnection and the non-Hub SDK surface are out of scope for this mechanism; a
generic SDK-level node-failover story is a separate, still-open piece of work.

### Social recovery via M-of-N guardians

This is the real answer to losing every registered passkey at once — multi-passkey
registration only helps if a second device was registered *before* the loss. An
identity owner designates a set of guardians, drawn only from their current friends
(the only pool this is allowed to draw from), plus a threshold M-of-N.

Configuring or changing that guardian set (`PUT /me/recovery/guardians`) requires the
identity's *current* session, same as every other session-gated route. An attacker who
has compromised only a not-yet-valid new device can never reach it. That's what makes
"changing the guardian set requires the current set of valid credentials" true by
construction, not by a special-case check.

Recovery itself is a four-stage state machine, one `recovery_requests` row per
attempt:

1. **Request.** From a new device with no valid session, a caller names the identity
   to recover and completes a WebAuthn registration ceremony for that device (`POST
   /recovery/requests/start` then `/finish`). This is the one deliberate exception to
   "every route requires a session," since the entire premise is that the caller has
   none for the identity in question. It's not an open door: the identity id must be
   real; the identity must actually have guardians configured (an unconfigured
   identity can never satisfy any M); at most one *active* request may exist per
   identity at a time (a partial unique index, not an application-level
   check-then-act); and a rolling 24-hour window caps how many requests may be
   initiated against a single identity regardless of outcome. The new device's passkey
   is captured but not yet a valid credential until the request finalizes.
2. **Approval.** Each guardian independently approves (`POST
   /recovery/requests/:id/approve`), gated on currently — not historically — being one
   of the identity's guardians. Once approvals reach the threshold that was in effect
   at request time (frozen so a guardian-set change mid-attempt can't retroactively
   change what the attempt needs), the request enters the delay phase.
3. **Mandatory public time-delay.** `AVALON_RECOVERY_DELAY_HOURS` (default 48 — long
   enough that an owner who only logs in occasionally plausibly notices, short enough
   that a genuine all-devices-lost recovery doesn't drag on for a week) must elapse
   with no veto. The delay's existence and countdown are public — `GET
   /identities/:id/recovery/status` requires no auth at all, since the delay is meant
   to be a public marker on the identity, not merely something the owner happens to be
   told. `GET /me/recovery/status` is the session-authenticated mirror of the same
   data.
4. **Veto or finalize.** The original owner (any session for the identity itself) or
   any *current* guardian may cancel at any point before finalization — a former
   guardian who has since been removed cannot, matching the config-change invariant
   above: the owner's own recourse against a compromised guardian is to remove them,
   not to leave their veto/approval power intact. `POST
   /recovery/requests/:id/finalize` is deliberately public and idempotent: it grants
   nothing beyond what approvals and the elapsed delay already authorized. It inserts
   the pending passkey as an ordinary new `identity_keys` row and never touches or
   revokes anything the real owner might still hold.

The node operator has no path anywhere in this flow that bypasses guardian approval or
the delay — finalize only ever acts on what approvals and elapsed time already
durably recorded in Postgres, not on anything an operator can unilaterally assert.
Every phase transition is durable history via the outbox:
`identity.recovery_configured`, `.recovery_requested`, `.recovery_approved`,
`.recovery_cancelled`, `.recovered` — see [`./protocol-events.md`](./protocol-events.md).

A guardian can discover every identity relying on them (`GET
/me/recovery/guardian-of`) and remove themselves from any one of those designations
without the owner's cooperation (`DELETE /me/recovery/guardian-of/{identity_id}`). A
self-removal that drops the owner below their configured threshold clamps the
threshold down to the new guardian count rather than leaving an unsatisfiable M-of-N.

Not currently built: a background sweep that auto-finalizes every eligible request the
moment its delay elapses (today, finalize is caller-triggered — the recovering
device's own client calls it once it observes a ready request past its delay); Rust
SDK and C# binding surface for this flow beyond the account-level session helpers.

### Post-compromise rollback

Recovery adds a new passkey and marks the request completed; it does not undo anything
an attacker did while holding the old credentials. Rollback is the self-service way for
a recovered owner to supersede those actions. It is a signed, append-only
*compensating event* (`friend.relationship_reversed`, `guild.membership_reversed`),
never a generic undo: the original ledger entry is neither rewritten nor deleted, the
same posture achievement revocation takes, and the compensating event is atomic with
its ledger entry via the outbox like every other write. Projections apply it
idempotently, so a rebuild from the ledger reproduces the same state.

**Eligibility window.** An event is eligible only if all of these hold:

- it was authored by this identity;
- its event timestamp is strictly before `completed_at` of the identity's latest
  completed recovery request; and
- its event timestamp is at or after `since`, a required timestamp the owner supplies to
  declare when they believe the compromise began.

Nothing in the system records when a compromise began, so the window is bounded above by
the recovery and below by the owner's own declaration. An identity with no completed
recovery has nothing eligible (`ROLLBACK_NO_COMPLETED_RECOVERY`); a `since` at or after
the recovery time is `INVALID_ROLLBACK_WINDOW`.

**What can be reversed.** Only reversals that need no other party's action are offered.

| Original event | Reversal | Condition |
|---|---|---|
| `friend.accepted` | friendship removed | the friendship still exists |
| `guild.member_added` (subject is the owner) | membership removed | still a member, and not the guild's owner (the owner must transfer ownership or delete the guild) |
| `guild.member_removed` with reason `left`, actor is the owner | membership restored | the guild exists, its join policy is currently open, and the identity is not currently a member |
| `friend.removed` | not reversible | restoring a friendship needs the counterparty; the owner sends a new friend request |
| `guild.member_removed` from an invite-only guild | not reversible | rejoining needs an invitation from a guild authority |
| any event already reversed | not reversible | a compensating event referencing it already exists; a reversal cannot be applied twice |

A restored membership is granted at the default member role. Any previous role would be
a grant that needs a guild authority, so it is not restored. Removing a membership also
clears the identity's `main_guild` if it pointed at that guild, as leaving does.

**Endpoints.** `GET /me/rollback/candidates?since=<RFC 3339>` lists every eligible event
with `reversible`, `reason` and `already_reversed`. `POST /me/rollback/{event_id}/reverse`
takes the same `since` and returns `{ reversal_event_id }`; it re-validates the window
and the current state itself and is in the fresh-signature tier, signing
`avalon:rollback.reverse:v1:<event_id>:<identity_id>:<since>` with `since` exactly as
sent. Refusals use `ROLLBACK_EVENT_NOT_ELIGIBLE`, `ROLLBACK_NOT_REVERSIBLE` and
`ROLLBACK_ALREADY_REVERSED`.

**Deferred.** Reversing anything that changes another party's state (restoring a removed
friendship, re-inviting into an invite-only guild, restoring previous guild roles); an
open-ended dispute flow when the owner and another party disagree; and expiry of the
eligibility window.

## What identity is not

- Not a universal integrator account. An integrator asks for scoped capabilities and gets
  only those.
- Not a universal character. See
  [`./bindings.md`](./bindings.md).
- Not a platform identity in the Steam/Xbox sense. No single operator owns it;
  see [`./nodes.md`](./nodes.md).
- **Not a real-world or government identity system, and not headed toward becoming
  one.** An Avalon identity identifies a keypair, never a person. Nothing in
  `identity.created`'s payload or anywhere else in identity creation asks for a legal
  name, date of birth, government ID, biometric, phone number, or email — there is no
  field for one, and no KYC step. This isn't a policy that could quietly change later;
  it's structural. Because identity is self-custodied with no central issuer, there is
  no party anywhere in the system — including Avalon's own maintainers or any node
  operator — that ever holds, or could be compelled to produce, a mapping from a
  keypair back to a real person, because that mapping is never created in the first
  place. An identity's owner who loses every passkey with no recovery configured loses
  the identity outright; nobody can "look it up" and reissue it. What Avalon
  identifies and makes portable is *online activity* — friendships, guild membership,
  achievements — never personhood. The heavy investment in decentralization throughout
  this protocol (no validator set, no platform lock-in, self-hostable nodes; see
  [`./nodes.md`](./nodes.md)) exists in large part to keep that true by construction
  rather than by promise.

## Portability means mirrored, not movable

"Your identity isn't trapped in one game's world" is a claim about *mirrored
history*, not about an end user ever needing to export and re-import their own data to
change which node they primarily use. An identity's durable facts — friend/guild/
achievement events — are already visible from any node mirroring the network's
history, the same way node-to-node infrastructure already lets a Settlement node be
reached from anywhere in the mesh. There is currently no user-facing export bundle,
and none is planned: there is nothing to export, because there is nothing a person is
trapped inside in the first place.

**The real limit, stated honestly**: an identity's *authentication* ceremony (WebAuthn
passkey registration, `identity_keys`) is local to the node it was registered on,
distinct from the durable facts above — and this is a real, load-bearing gap, not a
footnote. A passkey registered on node A cannot be presented in a WebAuthn ceremony
against node B at all (RP-ID scoping is part of the WebAuthn spec's own security
model, not something this protocol's design can route around). Session-continuation
tokens solve the *already-logged-in* half of reaching a different node, but never
count as a login credential by themselves — they prove key possession, not human
presence. Genuinely proving identity to a node never previously registered on, for the
first time, is solved by a destination-bound, human-approved signed grant extending
the cross-device pairing pattern above — see [`./nodes.md`](./nodes.md)'s "Identity and
social actions are not shard-locked" section, which also covers what "home node" stops
meaning once that lands as an ongoing dependency for anything except the initial login
step.

## Current implementation

- `crates/protocol/src/identity.rs` — `Identity { id, created_at }` and
  `Profile { identity_id, display_name, avatar_url, bio, favorite_genres, pronouns,
  banner_url, status, links, timezone, theme_color, location, main_guild }`, plus the
  `Genre` enum `favorite_genres` draws its fixed vocabulary from. No reference to any
  character schema, and no reference to WebAuthn/Ed25519 either — those stay
  server-side implementation detail, by design.
- `crates/protocol/src/ids.rs` — `IdentityId(Uuid)`.
- `crates/server/src/handlers.rs` — `register_start`/`register_finish`,
  `session_start`/`session_finish`, `me`, `update_profile`, working end-to-end against
  Postgres. `register_finish` verifies both the WebAuthn ceremony and the Ed25519
  event signature before writing anything, and enqueues `identity.created` into the
  outbox in the same transaction as the identity/profile/key rows. `update_profile`
  emits `profile.updated` whenever a promised-durable field changes. `list_profiles`
  (`GET /identities/profiles?ids=…`) resolves *other* identities' public profile
  fields (`display_name`, `avatar_url`) in a batch for roster surfaces (friends, guild
  members) — deliberately does not include `bio`/`favorite_genres`/`pronouns`, since
  this endpoint has no further visibility gating beyond session auth. `get_identity_profile`
  (`GET /identities/{id}/profile`) is a separate, single-identity endpoint exposing the
  fuller self-description field set for exactly one identity per request, so the batch
  endpoint's exposure stays narrow. It omits `discoverable` — that's the *viewed*
  identity's own search-visibility setting, not something the viewer needs.
- `apps/hub/src/views/Profile.vue` — surfaces and edits `bio`/`favorite_genres`/
  `pronouns` on the user's own profile; editable there only, readable on another
  identity's profile card via `get_identity_profile`, never editable there.
- `apps/hub/src/views/UserProfile.vue` — a read-only profile card for another
  identity, reachable by clicking a friend row or a guild member row, rendering the
  full self-description fields plus live presence. No shared-guilds list yet — no
  endpoint exposes another identity's guild memberships to a viewer.
- `crates/server/src/auth.rs` — builds the `Webauthn` instance
  (`AVALON_WEBAUTHN_RP_ID`/`AVALON_WEBAUTHN_ORIGIN`), verifies Ed25519 event
  signatures, and generates opaque session tokens. Carries its own in-process tests
  exercising a full register-then-authenticate ceremony against a virtual
  authenticator, no Postgres required.
- `crates/server/src/outbox.rs` — the outbox pattern: `enqueue` inside a transaction, a
  background worker that drains pending rows into `avalon-chain`, `status` for `avalon
  outbox-status`.
- `crates/server/db/migrations/0001_identity_and_auth/` — `identities`, `profiles`,
  `identity_keys` (passkeys), `identity_signing_keys` (Ed25519), `webauthn_ceremonies`
  (ephemeral ceremony state), `sessions`. No `credentials` table.
- `crates/server/src/passkeys.rs` — `POST /me/passkeys/register/start`, `POST
  /me/passkeys/register/finish`, `GET /me/passkeys`, `PATCH /me/passkeys/:id`
  (rename), `POST /me/passkeys/:id/revoke[?confirm=true]`. Session-gated, not
  identity-creation-gated. Revoking a passkey is a hard delete (no `revoked_at`
  column, unlike `identity_signing_keys`); no protocol event is emitted either way,
  since passkeys are operational state, not promised-durable. Revoking the identity's
  last remaining passkey without `?confirm=true` returns 409; the count check and the
  delete happen inside one transaction with a row lock so a concurrent revoke can't
  race two unconfirmed revokes past the guard at once.
- `crates/server/src/devices.rs` — `POST /me/devices/grants`, `GET
  /me/devices/grants[?status=]`, `GET /me/devices/grants/:id`, `POST
  /me/devices/grants/:id/approve`, `GET /me/devices`, `PATCH /me/devices/:id`
  (rename), `POST /me/devices/:id/revoke`. The first device's `identity_signing_keys`
  row is labeled at registration too (`register_finish`'s optional `device_label`).
- `crates/server/db/migrations/0029_social_recovery/` — `recovery_guardian_settings`
  (per-identity threshold), `recovery_guardians` (the guardian set, a friend-only rule
  enforced at the handler layer against `friendships`, not a DB constraint),
  `recovery_requests` (one row per attempt, with a partial unique index capping one
  *active* attempt per identity), and `recovery_approvals`.
- `crates/server/src/recovery.rs` — the full guardian-configuration and
  recovery-request state machine described above: `PUT`/`GET
  /me/recovery/guardians`, `POST /recovery/requests/start`, `POST
  /recovery/requests/finish`, `POST /recovery/requests/:id/approve`, `POST
  /recovery/requests/:id/cancel`, `POST /recovery/requests/:id/finalize`, `GET
  /recovery/requests/:id`, `GET /identities/:id/recovery/status`, `GET
  /me/recovery/status`, `GET /me/recovery/guardian-requests`. Every
  security-load-bearing invariant (threshold enforcement, delay enforcement, veto
  authority, rate limiting) is factored into a pure, unit-tested function.
  `crates/server/tests/recovery.rs` (`--ignored`) covers the full flow against a live
  server: a 3-guardian 2-of-3 recovery, a lone guardian below threshold never
  finalizing, an owner veto, a removed guardian losing approve/cancel authority, and
  guardian-set changes requiring a session.
- `crates/cli/src/main.rs` — `avalon create-identity` drives a real WebAuthn
  registration via a virtual authenticator and prints the loss-of-everything warning;
  it also persists that virtual passkey locally so `avalon login <identity_id>` can
  reload it and drive a real login ceremony later, printing a session token — a
  dev/test convenience, not a pattern for real deployment. `avalon outbox-status`.
- `apps/hub/src/crypto/webauthn.ts` — the real browser WebAuthn ceremonies via
  `@simplewebauthn/browser`, verified field-for-field against the server's
  request/response shapes. `apps/hub/src/crypto/signingKey.ts` — Ed25519 signing via
  `@noble/curves`, keys derived from a BIP39 mnemonic (`@scure/bip39`) per the section
  above. `apps/hub/src/api/identity.ts` ties it together with the
  `/identities/register/*` and `/sessions/*` API calls into
  `createIdentity()`/`login()`/`recoverSigningKey()`. `CreateIdentity.vue` shows the
  mnemonic once, right after the identity id, and asks for an optional device label
  passed through to `register_finish`. `apps/hub/src/api/passkeys.ts` orchestrates the
  authenticated add-a-passkey ceremony. `Profile.vue` has the recovery form and the
  grant-request form, shown only when the current device has no signing key stored for
  the logged-in identity; once it does, "Your devices" lists every registered device
  with rename and revoke. Separately, a "Passkeys" card always lists every registered
  passkey with rename and revoke, plus an "Add another passkey" button — visible
  regardless of signing-key state, since a passkey and a signing key are unrelated
  credentials. Revoking the last remaining passkey surfaces the server's 409 as a
  plain confirm prompt before retrying with `?confirm=true`. `CreateIdentity.vue` also
  shows a non-dismissible total-loss warning right after the first passkey is created
  — losing that device with no second one registered means permanently losing the
  identity and everything durable it carries. The same warning resurfaces on the
  Passkeys card for as long as exactly one passkey is registered, not a one-time
  dismiss — it disappears the moment a second passkey is registered.
- `apps/hub/src/api/recovery.ts` orchestrates guardian configuration and the
  recovery-initiation ceremony — `startRecovery()` is the one function in the Hub's
  API layer that deliberately never sends a bearer token. `Profile.vue` has a
  "Recovery guardians" card (a friend checklist plus an M-of-N threshold stepper), an
  owner-visible in-progress banner backed by `GET /me/recovery/status`, and a
  "Recovery requests to approve" card for when the logged-in identity is currently a
  guardian for someone else. `RecoverIdentity.vue` (routed at `/recover-identity`,
  linked from `Login.vue`) is the unauthenticated initiation flow: enter the identity
  id, drive the real WebAuthn ceremony, then poll the public recovery-status endpoint
  — no session anywhere on this page.
- Naming a guardian via `PUT /me/recovery/guardians` is unilateral (opt-out, not
  opt-in: a named guardian is active immediately), but a guardian can discover every
  identity relying on them and remove themselves from any one of those designations
  without the owner's cooperation, surfaced in the Hub as a "You're a recovery
  guardian for" card.
- `crates/server/db/migrations/0042_device_pairings/` — `device_pairings`:
  `device_code`/`user_code`, `status` (`pending`/`approved`/`denied`/`expired`),
  nullable `identity_id`/`session_token` (set only on approval), `expires_at`,
  `last_polled_at`.
- `crates/server/src/device_pairing.rs` — `POST /auth/device/start`, `POST
  /auth/device/poll`, `POST /auth/device/approve`, `POST /auth/device/deny`; see the
  cross-device pairing section above. `crates/server/tests/device_pairing.rs`
  (`--ignored`) covers the full `start → approve → poll` round trip against a live
  server, plus wrong `user_code`, denial, single-use consumption, and poll-rate
  limiting.
- `crates/cli/src/dev_tools.rs` — `avalon pair-device` drives the `start`/`poll` side
  of the flow as a stand-in incapable client.
- `apps/hub/src/views/PairDevice.vue`, routed at `/pair` (matching
  `verification_uri`'s `?user_code=` shape) — a `user_code` field plus approve/deny
  buttons, using the user's existing authenticated Hub session.

## Open questions

Identity recovery when every passkey is lost and no guardians are configured;
migrating an existing username/password identity (not currently applicable, since none
exist outside development); whether identities can be transferred; the minimum
replication guarantee for identity-bearing shards, so a single operator's node
disappearing can't strand the identities that live there.
</content>
</invoke>
