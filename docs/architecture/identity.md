# Identity

**An Avalon identity is player-owned and game-independent.** It is the one
thing that survives any single game, server, or database disappearing. A game
never owns it, never defines it, and never gets to rewrite its history. Games
establish their own scoped participation under it (see
[`./game-bindings.md`](./game-bindings.md)); the identity itself stays the same
across all of them.

Narrative: [`../Proposal.md` §7](../Proposal.md#7-persistent-player-identity)
and [§19](../Proposal.md#19-identity-vs-game-data).

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
| identity exists, `created_at` | yes | `identity.created` | already emitted |
| `display_name` | yes | `profile.updated` | no event today — #86 |
| `avatar_url` | yes | `profile.updated` | no event today — #86 |
| future bio / title / labels | classify when added | `profile.updated` | #86 sets the rule |
| public key(s) for the identity | yes, once #73 lands | key lifecycle events | never a secret |
| credentials (password hash) | **no** | — | never enters history |
| sessions / tokens | **no** | — | ephemeral server state |
| presence | **no** | — | [`./presence.md`](./presence.md) |
| visibility settings | no, unless later promoted | — | [`./privacy.md`](./privacy.md) |

A password hash, a session token, or any other secret is never part of an event
payload. The `identity.created` event currently carries `username`, which goes
away with #73.

## Authentication

Today, an identity is created with a username and password and a login yields
an opaque session token. That is a milestone-1 mechanism, not the protocol's
identity model.

The agreed direction, not yet designed or built, is
[#73](https://github.com/LunarVagabond/avalon-protocol/issues/73): the identity
*is* a keypair. Mutations to an identity's own data are authorized by a
signature from its private key via challenge-response; passkeys/WebAuthn are the
practical default so a player taps a fingerprint rather than managing a seed
phrase. Once that lands, the identity signs its own `identity.created` and
`profile.updated` events and the network only records them — a hosted node can
no longer fabricate an identity, which is the same guarantee that stops a node
fabricating a game's attestation (see
[`./security-model.md`](./security-model.md)).

Still open under #73 and Proposal §32: recovery after a lost device, multiple
keys per identity, migration from username/password, and whether identities
can be transferred.

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
  character schema, by design.
- `crates/protocol/src/ids.rs` — `IdentityId(Uuid)`.
- `crates/server/src/handlers.rs` — `register`, `login`, `me`,
  `update_profile`, working end-to-end against Postgres. `register` emits
  `identity.created`; `update_profile` emits nothing (#86). Identity and
  ledger writes are not yet atomic (#71).
- `crates/server/src/auth.rs` — Argon2id password hashing and opaque session
  tokens; the part #73 replaces.
- `crates/server/db/migrations/0001_identity_and_auth/` — `identities`,
  `profiles`, `credentials`, `sessions`.
- `crates/sdk/src/lib.rs` — `AvalonClient::authenticate()` exchanges a player
  token for a game-scoped `Session`.

## Decisions and tickets

- [#67](https://github.com/LunarVagabond/avalon-protocol/issues/67) — ADR:
  identity is separate from game characters.
- [#75](https://github.com/LunarVagabond/avalon-protocol/issues/75) — ADR:
  durable history is canonical; the table above follows from it.
- [#73](https://github.com/LunarVagabond/avalon-protocol/issues/73) — decision,
  open: identity as a self-custodied keypair.
- [#86](https://github.com/LunarVagabond/avalon-protocol/issues/86) — profile
  updates emit no event; classify promised-durable identity state.
- [#71](https://github.com/LunarVagabond/avalon-protocol/issues/71) — identity
  creation and its ledger entry aren't atomic.
- [#2](https://github.com/LunarVagabond/avalon-protocol/issues/2) — Epic:
  Identity & Player Profile.
