# Game Profiles and Bindings

**A binding says "this Avalon identity participates in this game" — nothing
more.** Everything a game knows about the player beyond that (characters, race,
class, level, appearance, progression, inventory) belongs to the game and lives
in the game's own database. **Game authority is scoped to the game's own
binding**: Game A can create and manage its profile of a player and issue its
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
as an attestation. The character rows are keyed on the game's side, referencing
the binding or the identity id; Avalon stores none of their attributes.

## What a binding is

| Field | Meaning |
|---|---|
| `identity_id` | the player |
| `game_id` | the game |
| `established_at` | when the player consented |
| `ended_at` | when the player (or the game) ended it, if ever |

- A binding is established by the **player**, through the consent flow, when
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

    cannot:
        read or write Game B's profile of the same identity
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

## Scenario A — a player enters a second game

Player X has a binding to Game A. They connect to Game B. Game B sees an
identity with a display name, whatever capabilities X granted, and X's
authentic attestations from Game A (which Game B may or may not recognize —
[`./trust-model.md`](./trust-model.md)). Game B creates its own character for X
in its own database. It never sees Game A's character, and Game A never learns
about Game B's unless X's permissions expose it.

## Today in the repo

- No binding type exists yet. The nearest thing is
  `PermissionGrant { identity_id, game_id, capability, ... }` in
  `crates/protocol/src/permissions.rs`, which answers "what may Game Y do" but
  not "is X a player of Y".
- `crates/protocol/src/games.rs` — `Game`, `GameRegistration`,
  `GameCredential`; the game side of the relationship.
- `crates/sdk/src/lib.rs` — `authenticate()` returns a game-scoped `Session`
  with an empty grant list; it does not yet check for a binding.

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
