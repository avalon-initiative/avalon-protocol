# Identity

**An Avalon identity is self-owned and game-independent.** It is the one
thing that survives any single game, server, or database disappearing. A game
never owns it, never defines it, and never gets to rewrite its history. Games
establish their own scoped participation under it (see
[`./game-bindings.md`](./game-bindings.md)); the identity itself stays the same
across all of them.

Narrative: [`../stakeholders/Proposal.md` §7](../stakeholders/Proposal.md#7-persistent-identity)
and [§19](../stakeholders/Proposal.md#19-identity-vs-game-data).

## The model

```text
Avalon Identity
    ├── Profile (identity-controlled metadata)
    ├── Friends                       ./social-graph.md
    ├── Guild memberships             ./guilds.md
    ├── Achievements / attestations   ./achievements-and-attestations.md
    ├── Permission grants             ./privacy.md
    └── Game bindings                 ./game-bindings.md
          ├── Game A → characters (game-owned)
          └── Game B → characters (game-owned)
```

An identity is an opaque, stable handle (`IdentityId`, a UUID). It is never
derived from a display name, a username, a wallet address, or anything its
owner might want to change later. Everything human-facing hangs off it as
profile data.

## Self-described metadata is self-expression, not fact

The profile carries what an identity's owner chooses to say about themselves:
display name, avatar, bio, favorite genres (drawn from a fixed, small
vocabulary — `avalon_protocol::identity::Genre` — not free text, so it stays
useful for matching/filtering later), and pronouns (issue #155). None of it is
an authoritative game fact.

Someone writing "I am an Avion" in their bio does not make Avion a
network-level race. A game can display that, interpret it, or ignore it. Facts
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

| State | Promised durable? | Canonical record | Notes |
|---|---|---|---|
| identity exists, `created_at` | yes | `identity.created` | self-signed, see below |
| `display_name` | yes | `identity.created` (initial), `profile.updated` (changes) | emitted in the same transaction as the `profiles` row, via the outbox |
| handle discriminator | yes | `identity.created` (initial), `profile.updated` (on rename) | server-chosen, so it's carried in the event — a rebuild must land on the same `name#1234` |
| `avatar_url` | yes | `profile.updated` | `null` in the payload means explicitly cleared; absent means untouched |
| `bio` | yes | `profile.updated` | free text, capped at 500 characters; `null` means explicitly cleared, absent means untouched (#155) |
| `favorite_genres` | yes | `profile.updated` | fixed, small controlled vocabulary (`Genre`), capped at 5 entries; unknown values rejected, not dropped; a present key always fully replaces the list, including to `[]` (#155) |
| `pronouns` | yes | `profile.updated` | free text, capped at 40 characters; `null` means explicitly cleared, absent means untouched (#155) |
| future title / labels | classify when added | `profile.updated` | the rule: promised-durable means it emits, or it isn't promised |
| WebAuthn passkey(s) | operational state, not an event | — | `identity_keys` table; see below |
| event-signing public key | yes, at registration | `identity.created`'s issuer | see below |
| credentials (password hash) | **no**, pruned entirely | — | #73, done |
| sessions / tokens | **no** | — | ephemeral server state |
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
  below).
- **A raw Ed25519 key** (`identity_signing_keys` table) proves authorship of a
  specific durable event. It signs `identity.created` at registration —
  `issuer` on that event is `identity:<id>:self:created`, not
  `network:avalon-server:...` — so a hosted node can no longer fabricate an
  identity that never actually registered, the same guarantee that stops a
  node fabricating a game's attestation (see
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
human-typeable `user_code` (shown to the player, e.g. as a QR code pointing
at `verification_uri`) plus an opaque `device_code` it alone holds. The
player completes a real WebAuthn login on a capable device — the Hub, in a
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
provider the identity's owner uses.

### Social recovery via M-of-N guardians (#201)

The real answer to losing every registered passkey at once — #200's
multi-passkey registration only helps if a second device was registered
*before* the loss. An identity owner designates a set of guardians (drawn only from
their current friends, issue #15's network-level primitive — the only pool
this is allowed to draw from) and a threshold M-of-N. Configuring or
changing that set (`PUT /me/recovery/guardians`) requires the identity's
*current* session, same as every other session-gated route in this crate —
never reachable by an attacker who has compromised only a not-yet-valid new
device, which is what makes "changing the guardian set requires the current
set of valid credentials" true by construction rather than by a special-case
check.

Recovery itself is a four-stage state machine, one `recovery_requests` row
per attempt:

1. **Request.** From a new device with no valid session, a caller names the
   identity to recover and completes a WebAuthn registration ceremony for
   that device (`POST /recovery/requests/start` then `/finish` — the same
   two-step shape `handlers::register_start`/`register_finish` and
   `passkeys::register_start`/`register_finish` already use). This is the
   one deliberate exception to "every route requires a session" in this
   crate, since the entire premise is that the caller has none for the
   identity in question. It is not an open door: the identity id must be
   real, the identity must actually have guardians configured (an
   unconfigured identity can never satisfy any M, so there is nothing to
   spam toward), at most one *active* request may exist per identity at a
   time (a partial unique index on `recovery_requests`, not an
   application-level check-then-act), and a rolling 24-hour window caps how
   many requests may be initiated against a single identity regardless of
   outcome. The new device's passkey is captured but not yet a valid
   credential — it sits in `recovery_requests.pending_passkey_data` until
   the request actually finalizes.
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
`finalize_request` is called lazily — by the recovering device polling, or
by anyone else who happens to check — which is correct but not
self-triggering); Rust SDK and C# binding surface for this flow; and Hub UI
polish beyond a functional guardian-management card, initiation flow, and
approval list (an owner-visible in-progress banner shows wherever
`GET /me/recovery/status` is checked, but a dedicated real-time alert is
future work). None of these affect the state machine or its invariants.

## What identity is not

- Not a universal game account. A game asks for scoped capabilities and gets
  only those.
- Not a universal character. See
  [`./game-bindings.md`](./game-bindings.md).
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

## Today in the repo

- `crates/protocol/src/identity.rs` — `Identity { id, created_at }` and
  `Profile { identity_id, display_name, avatar_url, bio, favorite_genres,
  pronouns }` (the last three added by #155), plus the `Genre` enum
  `favorite_genres` draws its fixed vocabulary from. No reference to any
  character schema, and no reference to WebAuthn/Ed25519 either — those stay
  server-side implementation detail, by design.
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
  identities' public profile fields (`display_name`, `discriminator`,
  `avatar_url`) in a batch — the gap every roster surface (friends, guild
  members) previously had to leave as a raw identity id. Deliberately does
  NOT include `bio`/`favorite_genres`/`pronouns`: this endpoint has no
  further visibility gating beyond session auth (any identity can
  batch-resolve arbitrary ids), so it stays at the same minimal exposure
  level it had before #155 rather than silently widening what any stranger
  can bulk-collect. Exposing those fields there, if ever wanted, is a
  scoping decision for its own ticket.
- `apps/hub/src/views/Profile.vue` (#277) — surfaces and edits
  `bio`/`favorite_genres`/`pronouns` on the player's own profile, closing
  the gap #155 left open: the server has supported all three fields since
  #155, but the Hub's own type layer (`apps/hub/src/api/types.ts`) never
  declared them and `Profile.vue` never rendered them, so they were only
  ever reachable by calling `PATCH /me` directly. Bio/pronouns reuse the
  same per-field `AvalonEditableField` save pattern `display_name`/
  `avatar_url` already use; `favorite_genres` is a fixed checkbox picker
  over the closed `Genre` vocabulary (capped at 5 client-side, matching
  `MAX_FAVORITE_GENRES`) with its own explicit Save, mirroring the
  recovery-guardians picker on the same page. Own-profile view only — per
  `list_profiles`'s note just above, these three fields stay deliberately
  absent from any other identity's profile view.
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
  token for a game-scoped `Session`, unchanged by any of this — a game never
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
  approve/deny buttons, using the player's existing authenticated Hub
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
  `crates/server/tests/recovery.rs`. Scoped out this pass: a background
  auto-finalize sweep (finalize is currently caller-triggered, not
  self-triggering), Rust SDK / C# binding surface, and Hub UI polish beyond
  a functional guardian-management/initiation/approval flow — see the
  section above for the full scoping rationale.
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
- [#2](https://github.com/LunarVagabond/avalon-protocol/issues/2) — Epic:
  Identity & Player Profile.
