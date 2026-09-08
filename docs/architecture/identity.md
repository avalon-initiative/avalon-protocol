# Identity

**An Avalon identity is player-owned and game-independent.** It is the one
thing that survives any single game, server, or database disappearing. A game
never owns it, never defines it, and never gets to rewrite its history. Games
establish their own scoped participation under it (see
[`./game-bindings.md`](./game-bindings.md)); the identity itself stays the same
across all of them.

Narrative: [`../stakeholders/Proposal.md` §7](../stakeholders/Proposal.md#7-persistent-player-identity)
and [§19](../stakeholders/Proposal.md#19-identity-vs-game-data).

## The model

```text
Avalon Identity
    ├── Profile (player-controlled metadata)
    ├── Friends                       ./social-graph.md
    ├── Guild memberships             ./guilds.md
    ├── Achievements / attestations   ./achievements-and-attestations.md
    ├── Permission grants             ./privacy.md
    └── Game bindings                 ./game-bindings.md
          ├── Game A → characters (game-owned)
          └── Game B → characters (game-owned)
```

An identity is an opaque, stable handle (`IdentityId`, a UUID). It is never
derived from a display name, a username, a wallet address, or anything a
player might want to change later. Everything human-facing hangs off it as
profile data.

## Player-controlled metadata is self-expression, not fact

The profile carries what a player chooses to say about themselves: display
name, avatar, and later bio, preferred title, self-described labels, and
interests. None of it is an authoritative game fact.

A player writing "I am an Avion" in their bio does not make Avion a
network-level race. A game can display that, interpret it, or ignore it. Facts
about what a player *has done* come from issuer attestations with provenance
(see [`./provenance.md`](./provenance.md)), never from the profile.

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
| `display_name` | yes | `profile.updated` | no event today — #86 |
| `avatar_url` | yes | `profile.updated` | no event today — #86 |
| future bio / title / labels | classify when added | `profile.updated` | #86 sets the rule |
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
  ceremony each time after. Multiple passkeys per identity are already
  supported by the schema (issue #99's cheapest recovery mitigation), though
  the endpoint to add a second one isn't built yet.
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

Still open: recovery after every passkey is lost
([#99](https://github.com/LunarVagabond/avalon-protocol/issues/99)),
migrating an existing username/password identity (none exist outside
development, so not applicable yet), and whether identities can be
transferred (Proposal §32).

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
- The mnemonic itself is shown to the player exactly once, at creation
  (`CreateIdentity.vue`), and is never stored anywhere — any device holding
  it can re-derive the identical key offline, with no server round-trip
  (`Profile.vue`'s "recover your signing key" section, shown when the
  current device has none). The server only ever sees the resulting
  *public* key.

This mnemonic recovery is the fallback path; the primary path — a
device-registration/linked-device grant model, no phrase in the common
case — is a separate, not-yet-built piece
([#135](https://github.com/LunarVagabond/avalon-protocol/issues/135)). Both
were decided together in
[#122](https://github.com/LunarVagabond/avalon-protocol/issues/122), which
is distinct from [#99](https://github.com/LunarVagabond/avalon-protocol/issues/99):
#99 covers a lost *passkey* (login credential), #122 covers the *signing
key* (what authenticates a player's authored events) — recovering it never
by itself authenticates a login. The WebAuthn passkey has no equivalent
client-storage decision here: it never leaves the platform authenticator,
already synced across devices by whatever passkey provider the player uses.

## What identity is not

- Not a universal game account. A game asks for scoped capabilities and gets
  only those.
- Not a universal character. See
  [`./game-bindings.md`](./game-bindings.md).
- Not a platform identity in the Steam/Xbox sense. No single operator owns it;
  see [`./nodes.md`](./nodes.md).

## Today in the repo

- `crates/protocol/src/identity.rs` — `Identity { id, created_at }` and
  `Profile { identity_id, display_name, avatar_url }`. No reference to any
  character schema, and no reference to WebAuthn/Ed25519 either — those stay
  server-side implementation detail, by design.
- `crates/protocol/src/ids.rs` — `IdentityId(Uuid)`.
- `crates/server/src/handlers.rs` — `register_start`/`register_finish`,
  `session_start`/`session_finish`, `me`, `update_profile`, working end-to-end
  against Postgres. `register_finish` verifies both the WebAuthn ceremony and
  the Ed25519 event signature before writing anything, and enqueues
  `identity.created` into the outbox in the same transaction as the
  identity/profile/key rows (#71, done for this path). `update_profile` still
  emits nothing (#86).
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
- `crates/cli/src/main.rs` — `avalon create-identity` drives a real WebAuthn
  registration via a virtual authenticator (`passkey-authenticator`'s
  `testable` feature) and prints the loss-of-everything warning #99 calls
  for; `avalon outbox-status`.
- `crates/sdk/src/lib.rs` — `AvalonClient::authenticate()` exchanges a player
  token for a game-scoped `Session`, unchanged by any of this — a game never
  creates identities or logs a player in itself.
- `apps/hub/src/crypto/webauthn.ts` — the real browser WebAuthn ceremonies via
  `@simplewebauthn/browser`, verified field-for-field against
  `crates/server/src/handlers.rs`'s request/response shapes (both follow the
  same base64url/camelCase WebAuthn JSON convention, no adapter layer
  needed). `apps/hub/src/crypto/signingKey.ts` — Ed25519 signing via
  `@noble/curves`, keys derived from a BIP39 mnemonic (`@scure/bip39`) per
  the section above, storage unchanged. `apps/hub/src/api/identity.ts` ties
  it together with the `/identities/register/*` and `/sessions/*` API calls
  into `createIdentity()`/`login()`/`recoverSigningKey()`.
  `CreateIdentity.vue` shows the mnemonic once, right after the identity id;
  `Profile.vue` has the recovery form, shown only when the current device
  has no signing key stored for the logged-in identity.

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
- [#99](https://github.com/LunarVagabond/avalon-protocol/issues/99) — open
  decision: identity recovery when every passkey is lost.
- [#55](https://github.com/LunarVagabond/avalon-protocol/issues/55) — Hub
  identity creation/login UI: the real WebAuthn + Ed25519 flow, in-browser.
- [#122](https://github.com/LunarVagabond/avalon-protocol/issues/122) —
  release-blocking decision: Ed25519 signing-key custody. See the section
  above; [#134](https://github.com/LunarVagabond/avalon-protocol/issues/134)
  and [#135](https://github.com/LunarVagabond/avalon-protocol/issues/135)
  are the two halves it decided on.
- [#86](https://github.com/LunarVagabond/avalon-protocol/issues/86) — profile
  updates still emit no event; classify promised-durable identity state.
- [#2](https://github.com/LunarVagabond/avalon-protocol/issues/2) — Epic:
  Identity & Player Profile.
