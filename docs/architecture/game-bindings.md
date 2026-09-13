# Game Profiles and Bindings

**A binding says "this Avalon identity participates in this game" — nothing
more.** Everything a game knows about the identity beyond that (characters, race,
class, level, appearance, progression, inventory) belongs to the game and lives
in the game's own database. **Game authority is scoped to the game's own
binding**: Game A can create and manage its profile of an identity and issue its
own attestations; it cannot touch Game B's.

Narrative: [`../stakeholders/Proposal.md` §19](../stakeholders/Proposal.md#19-identity-vs-game-data).

## The tree

```text
Avalon Identity X
    │
    ├── Binding: Ashen Realms
    │       └── (game-side) Character: Avion Ranger, level 72
    │
    └── Binding: Ocean World
            └── (game-side) Character: Sea Lion Navigator, level 14
```

Avalon holds the two bindings. It does not know what an Avion or a Sea Lion is,
and it never will unless a game explicitly promotes a fact into durable history
as an attestation, **or explicitly publishes it as instance data against a
schema it published itself** (Game Space's data-exposure mechanism, #255/#384,
decided #381) — subject to that schema's own declared visibility. Absent one
of those two explicit acts, the character rows stay keyed on the game's side,
referencing the binding or the identity id; Avalon stores none of their
attributes. Publishing instance data is deliberately narrow in spirit even
though it's technically unrestricted in shape: small, portable, fun-to-carry
flavor data (a character's name, level, race, class, titles), never a
character's full mechanical state (inventory, skills, stats used for game
balance) — see [`./identity-aggregate-view.md`](./identity-aggregate-view.md)
for a full worked example and the visibility rules.

## What a binding is

| Field | Meaning |
|---|---|
| `identity_id` | the identity |
| `game_id` | the game |
| `established_at` | when the identity consented |
| `ended_at` | when the identity (or the game) ended it, if ever |

- A binding is established by the **identity**, through the consent flow, when
  they first connect their identity to a game. A game cannot create one
  unilaterally.
- Capability grants hang off the binding. No active binding, no grants; ending
  the binding ends every grant under it.
- Ending a binding does not delete history. Attestations the game issued while
  the binding was active remain in history with their provenance.
- Bindings are durable protocol events (`game.binding_established`,
  `game.binding_ended`), so "which games has this identity participated in" is
  reconstructable and so the registry can count players honestly.

## Scoped authority

```text
Game A
    can:
        establish a Game A profile for a consenting identity
        manage its own game-side character data
        issue Game A attestations about that identity
        read whatever Game B has explicitly published (attestations,
            or instance data Game B opted into being visible, #381)

    cannot:
        read or write Game B's own internal, unpublished profile of the
            same identity
        write to Game B's published data under any circumstance
        issue attestations under Game B's issuer identity
        alter the identity itself, its friends, or its guild history
```

This is enforced by the permission model (every grant is scoped to a game and a
capability) and by issuer keys (every attestation is signed by the issuing
game's own key). See [`./security-model.md`](./security-model.md) and
[`./games-and-issuers.md`](./games-and-issuers.md).

## Bindings and the registry

"Players of Game A" means distinct identities holding an active binding to Game
A, observed through protocol activity — not a number the game reports about
itself. See [`./game-registry.md`](./game-registry.md).

## Scenario A — an identity enters a second game

Identity X has a binding to Game A. They connect to Game B. Game B sees an
identity with a display name, whatever capabilities X granted, and X's
authentic attestations from Game A (which Game B may or may not recognize —
[`./trust-model.md`](./trust-model.md)). Game B creates its own character for X
in its own database. It never sees Game A's character, and Game A never learns
about Game B's unless X's permissions expose it.

## Today in the repo

- `crates/protocol/src/games.rs` — `GameBinding { identity_id, game_id,
  established_at, ended_at }`, real now (#83), alongside `Game`,
  `GameRegistration`, `GameCredential`. No game-specific field exists on it,
  by design.
- `crates/server/src/connections.rs` (#27/#83) — `POST /games/{slug}/connect`
  is the consent flow: it validates every approved capability against what
  the game declared at registration (`GET /games/{slug}` /
  `game_requested_capabilities`, rejecting anything undeclared), creates the
  `bindings` row only if the caller has no active binding to that game yet
  (idempotent — reconnecting grants any newly-approved capabilities without
  duplicating the binding or re-firing `game.binding_established`), and
  inserts one `permission_grants` row per approved capability, all in one
  transaction via the outbox. `DELETE /games/{slug}/grants/{capability}`
  revokes a single grant; `DELETE /games/{slug}/connect` ends the binding
  and revokes every active grant under it in the same transaction; `GET
  /me/connections` lists the caller's active bindings with their active
  grants. `bindings`/`permission_grants` (migration `0012_game_bindings`)
  are projections — `game.binding_established`, `game.binding_ended`,
  `permission.granted`, `permission.revoked` are the durable history, and
  are network-attributed for now (the same milestone-1 stand-in
  `game.registered` uses), not yet identity-signed despite what the
  event-kind catalogue eventually intends.
- `crates/sdk/src/lib.rs` — `authenticate()` now calls `GET /me/grants`
  (identified by `AvalonConfig::game_credential_key_id`) and populates
  `Session.granted` from the caller's real active grants for that game,
  rather than always returning an empty list.

## Decisions and tickets

- [#83](https://github.com/LunarVagabond/avalon-protocol/issues/83) — game
  profile/binding type, events, endpoints, and the SDK check.
- [#67](https://github.com/LunarVagabond/avalon-protocol/issues/67) — ADR:
  identity is separate from game characters.
- [#27](https://github.com/LunarVagabond/avalon-protocol/issues/27) —
  capability grant/revoke consent flow (produces the binding).
- [#28](https://github.com/LunarVagabond/avalon-protocol/issues/28) —
  permission enforcement middleware.
- [#25](https://github.com/LunarVagabond/avalon-protocol/issues/25) — Epic:
  Game Registration & Permissions.
