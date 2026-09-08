# Privacy and Visibility

**Nothing about a player is exposed because the protocol can technically expose
it.** Every read path names the visibility scope it checks. **Network analytics
are aggregates**, never per-player data. A portable identity makes surveillance
portable too ([Proposal §31](../stakeholders/Proposal.md#31-major-risks)); the answer is
intentional scoping, not hoping nobody looks.

## Scopes

| Scope | Who can see |
|---|---|
| public | anyone, including unauthenticated readers of a mirror |
| authenticated-only | any Avalon identity |
| friend-visible | identities in the subject's friends list |
| guild-visible | members of a given guild (or of any shared guild) |
| game-visible | a game holding the relevant capability under an active binding |
| private | the subject only |
| operator-only | node operators, for diagnostics; never surfaced through the API |

A scope applies per resource: profile fields, presence, friends list, guild
membership list, achievement history (with per-claim hide/feature on top). Guilds
also set a policy on their own roster (public, members-only, hidden).

## Composition

A game's capability grant never widens what non-game viewers can see, and a
public visibility setting never grants a game a capability it wasn't given. For a
read by a game the order is: active [binding](./game-bindings.md) → active
`PermissionGrant` for the specific capability → visibility scope. For a read by a
person: relationship to the subject → visibility scope. One authorization helper
answers (viewer, subject, resource); no endpoint does its own ad-hoc check.

## Proposed defaults

Player-controlled, changeable through the API and the Hub. To be finalized in
[#87](https://github.com/LunarVagabond/avalon-protocol/issues/87), not by
accident:

| Resource | Proposed default |
|---|---|
| display name | public |
| avatar | public |
| presence | friend-visible |
| friends list | private |
| guild membership | guild-visible |
| achievement history | public, individually hideable |
| game bindings (which games a player plays) | private |

Visibility settings are player state, not durable protocol history, unless a
later decision promotes them.

## Presence and guilds

Presence is the most sensitive realtime signal ("where is this person right
now") and defaults to friends. See [presence](./presence.md). Guild membership
visibility is policy-controlled by both the player and the guild; a guild that
wants a hidden roster gets one. See [guilds](./guilds.md).

## Analytics

The [game registry](./game-registry.md) publishes counts and aggregates:

```text
2,481,392 unique players
```

Never a list of who they are. Metrics are derived from protocol events, but the
events themselves are subject to the same scopes when read individually — a
public transparency log ([settlement](./settlement.md)) is public, which is
exactly why what goes *into* it is limited to promised-durable facts and never
includes presence, credentials, or anything a player did not choose to make
durable.

## Today in the repo

- `crates/protocol/src/permissions.rs` — `Capability` and `PermissionGrant`
  (game-visible only). No `Visibility` type.
- `crates/server/src/handlers.rs` — `/me` reads the caller's own profile; there is
  no cross-identity read path yet, so no scope check exists yet either.
- The ledger (`crates/server/db/migrations/0002_ledger`) is readable by anyone
  with database access; the `identity.created` payload currently includes a
  username, which is removed under
  [#73](https://github.com/LunarVagabond/avalon-protocol/issues/73) and
  [#86](https://github.com/LunarVagabond/avalon-protocol/issues/86).

## Decisions and tickets

- [#87](https://github.com/LunarVagabond/avalon-protocol/issues/87) — visibility
  scopes for profile, presence, friends, and guild membership.
- [#78](https://github.com/LunarVagabond/avalon-protocol/issues/78) — ADR:
  presence is ephemeral and permissioned.
- [#74](https://github.com/LunarVagabond/avalon-protocol/issues/74) — ADR: guilds
  are network primitives; roster visibility is guild policy.
- [#89](https://github.com/LunarVagabond/avalon-protocol/issues/89) — registry:
  aggregates only.
- [#28](https://github.com/LunarVagabond/avalon-protocol/issues/28) — permission
  enforcement middleware (the write-side counterpart).
- Open in [Proposal §32](../stakeholders/Proposal.md#32-open-questions): how much social
  information should be portable; cross-game blocking.
