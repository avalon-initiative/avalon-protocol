# Guilds

**An Avalon guild is a first-class primitive of the network, not of any game.**
It exists before its members enter a particular game, has members playing many
games at once, and survives any one of those games shutting down. **A game is a
client of a guild, never its owner.** Guild membership, roles, permissions, and
history belong to the network; a game may render and consume them, but cannot
govern them through its game authority.

This is decided, not aspirational — see
[#74](https://github.com/LunarVagabond/avalon-protocol/issues/74). The narrative
version is [Proposal §10](../stakeholders/Proposal.md#10-guilds-and-social-identity).

## Where guilds sit

```text
Avalon Network
│
├── Identities
│
├── Guilds
│   ├── Members
│   ├── Roles
│   ├── Reputation
│   ├── History
│   └── Chat
│
├── Achievements / Attestations
│
├── Games
│
└── Assets / Ownership
```

Guilds are a sibling of games, not a child of them. The same guild can be seen
from inside Ashen Realms, inside WorldZero, and from the Hub with no game open:

```text
Dragon Hunters

Members: 1,284
Founded: 2027
Games: 7
Chat: Active

Members currently playing:
    42 — Ashen Realms
    18 — WorldZero
     7 — Game C
```

Nothing above is game-scoped. The per-game counts are realtime presence (see
[presence](./presence.md)), not something any one game reports about "its" guild.

## What Avalon owns

| Concern | Owner | Notes |
|---|---|---|
| Guild existence, name, tag, description | Network | `Guild` in the protocol crate |
| Membership | Network | joins/leaves/removals are protocol events |
| Roles and permissions | Network | assigned by guild members with authority |
| Guild history | Network | append-only, reconstructable |
| Guild chat channels and messages | Network | delivered to any authorized client |
| Reputation, governance | Network | later; see [future layers](./future-layers.md) |
| A game's in-world rendering of a guild | Game | banners, halls, roster UI, whatever |
| Game-internal clans that never touch Avalon | Game | stay game-side entirely |

A game that wants purely internal clans keeps them in its own database and does
not put them on Avalon. Avalon does not try to model every in-game group.

## Game as client

"Join Dragon Hunters" inside a game calls Avalon social functionality; the
membership change happens in the network layer, and the game then consumes it.
The only relationship between a guild and a game is `GuildGameAssociation`:
opt-in, many-to-many, non-owning, removable without affecting the guild.

What a game can do through the association:

- render the roster and member presence (subject to [visibility](./privacy.md))
- render guild chat in its own UI as a client of the channel
- ask "is this player a member of guild X, with role Y?"
- unlock game-side content based on membership

What a game cannot do: rename, dissolve, transfer, or govern a guild; remove
members; grant roles. Those are guild-role authorizations, not game credentials.
See [security model](./security-model.md) for the scoped-authority rules.

## Guild chat is a network primitive

A channel belongs to the guild. It is visible through the Hub, the mobile-hub
companion app, a game that chooses to render it, a web client, and later a
Discord bridge or other authorized client. Player A in Game A and Player B in
Game B talk in the same channel. See
[Proposal §12](../stakeholders/Proposal.md#12-communication).

Guild chat is not gameplay chat. Games keep their own local chat; Avalon carries
the cross-game social channel.

## History vs current state

Membership history is durable protocol history; the roster is a projection.

```text
guild.created           2027-01-01  Dragon Hunters, founder: Identity X
guild.member_added      2027-01-01  Identity X (Leader)
guild.member_added      2027-02-10  Identity Y (Member)
guild.role_changed      2027-04-14  Identity Y -> Officer
guild.member_removed    2028-02-10  Identity Y (left)
```

Current projection:

```text
Dragon Hunters
    Members: Identity X (Leader)
```

Both are useful. The history is reconstructable from
[protocol events](./protocol-events.md); the roster is rebuilt from it by the
[indexer](./query-and-indexing.md). Nothing about a guild that Avalon promises to
preserve lives only in a mutable row — see [#75](https://github.com/LunarVagabond/avalon-protocol/issues/75).

## Analytics phrasing

Never describe network guilds as belonging to a game.

Wrong, unless the guilds are explicitly game-owned:

```text
Game A has 18,291 guilds.
```

Right:

```text
18,291 Avalon guild members are currently associated with Game A.
4,217 Avalon guilds have members who play Game A.
```

Metrics the [game registry](./game-registry.md) may derive: guild members
associated with a game, guilds with members playing a game, cross-game guild
activity, membership growth, active members, guild participation in game events,
guilds spanning multiple games.

## Scenario H — network guild

Can a guild exist outside Game A and have members simultaneously playing Game A,
Game B, and Game C? Yes, by construction: the guild has no game parent, members
have [bindings](./game-bindings.md) to whichever games they play, and presence
reports where each member currently is. If Game A shuts down, the guild, its
roster, its channels, and its history are unaffected; only the `GuildGameAssociation`
with Game A becomes historical.

## Today in the repo

- `crates/protocol/src/guilds.rs` — `Guild`, `GuildRole`, `GuildMember`,
  `GuildGameAssociation`, `GuildChannel`, `GuildMessage`. Pure types; the
  association is documented as opt-in and non-owning.
- No server endpoints, storage, events, or SDK methods exist yet. `avalon-server`
  serves identity/auth only.
- No guild events are catalogued yet; `guild.created`, `guild.member_added`,
  `guild.member_removed`, `guild.role_changed` are the expected kinds
  ([#82](https://github.com/LunarVagabond/avalon-protocol/issues/82)).
- Hub guild views (`apps/hub`, `apps/mobile-hub`, `packages/ui`) are scaffolding.

## Decisions and tickets

- [#74](https://github.com/LunarVagabond/avalon-protocol/issues/74) — ADR: guilds
  are network-level primitives, not game-owned.
- [#75](https://github.com/LunarVagabond/avalon-protocol/issues/75) — ADR: durable
  history is canonical; the roster is a projection.
- [#19](https://github.com/LunarVagabond/avalon-protocol/issues/19) — Epic: Guilds
  & Guild Communication, with
  [#20](https://github.com/LunarVagabond/avalon-protocol/issues/20) (CRUD + roles),
  [#21](https://github.com/LunarVagabond/avalon-protocol/issues/21) (membership),
  [#22](https://github.com/LunarVagabond/avalon-protocol/issues/22) (channels +
  messages), [#23](https://github.com/LunarVagabond/avalon-protocol/issues/23)
  (SDK), [#24](https://github.com/LunarVagabond/avalon-protocol/issues/24) (Hub).
- [#87](https://github.com/LunarVagabond/avalon-protocol/issues/87) — visibility
  scopes, including roster visibility.
- Open questions from [Proposal §32](../stakeholders/Proposal.md#32-open-questions): guild
  ownership, leadership transfer.
