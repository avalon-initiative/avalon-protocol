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

- `crates/protocol/src/guilds.rs` — `Guild` (now carrying `join_policy`),
  `JoinPolicy` (`InviteOnly` | `Open`, issue #21), `GuildRole`, `GuildMember`,
  `GuildGameAssociation`, `GuildChannel`, `GuildMessage`, plus
  `GuildPermission` (issue #20): the fixed milestone-1 permission set a
  role can carry — `manage_guild`, `manage_roles`, `manage_members`,
  `manage_channels`, not yet extensible.
- Guild creation, rename/retag/redescribe, roles, ownership transfer, and
  membership lifecycle are real and served by `avalon-server`
  (`crates/server/src/guilds.rs`, issues #20 and #21): `POST /guilds`,
  `GET /guilds/{id}`, `PATCH /guilds/{id}`, `GET /guilds/{id}/roles`,
  `POST /guilds/{id}/roles`, `PATCH /guilds/{id}/roles/{idx}`,
  `POST /guilds/{id}/transfer-ownership`, `POST /guilds/{id}/games/{game_id}`,
  `POST /guilds/{id}/invites`, `POST /guilds/{id}/invites/{invite_id}/accept`,
  `POST /guilds/{id}/invites/{invite_id}/decline`, `POST /guilds/{id}/join`
  (open guilds only), `POST /guilds/{id}/leave`,
  `DELETE /guilds/{id}/members/{identity_id}`,
  `PATCH /guilds/{id}/members/{identity_id}`, `GET /guilds/{id}/members`, and
  `GET /me/guilds`. Every route is session-authenticated only, same as
  `friends.rs`. `guild.created`, `guild.updated`, `guild.role_defined`,
  `guild.owner_transferred`, `guild.member_added`, `guild.member_removed`,
  and `guild.role_changed` are written into the outbox in the same
  transaction as the `guilds`/`guild_roles`/`guild_game_associations`/
  `guild_members` projection change (`crates/server/db/migrations/0008_guilds`,
  `0009_guild_membership`). Invites, declines, and withdrawals are
  deliberately not durable — resolving one is a plain `guild_invites`
  projection update, no event, same pattern `friends.rs` uses for
  declined/withdrawn friend requests; a pending invite is idempotent
  (re-inviting while one is outstanding returns the existing invite rather
  than erroring or duplicating it). A guild is created with a fixed starter
  role set (`owner`, `officer`, `member`) and its owner's own
  `guild_members` row at `role_index = 0`; `owner` always has every
  permission structurally (the `guilds.owner` column), not through its role
  row, so it can't be edited away or removed, and the owner must transfer
  ownership before leaving. `actor_role_permissions` does a real
  `guild_members` JOIN `guild_roles` lookup, so `manage_guild`/`manage_roles`
  permission checks now work for non-owner members too; `member_count` on
  `GET /guilds/{id}` is a real `COUNT(*)` over `guild_members`. Removing a
  member holding an elevated (non-`member`) role additionally requires
  `manage_roles`, not just `manage_members` — an officer can remove a plain
  member but not another officer.
- Guild chat channels and messages are real and served by `avalon-server`
  (issue #22): `crates/server/src/channels.rs` (`GET`/`POST
  /guilds/{id}/channels`, `PATCH .../channels/{cid}`, `POST
  .../channels/{cid}/archive`) and `crates/server/src/guild_messages.rs`
  (`GET`/`POST .../channels/{cid}/messages`, `DELETE
  .../channels/{cid}/messages/{mid}`) — migration
  `crates/server/db/migrations/0010_guild_channels`. Every guild is seeded
  with a default `general` channel on creation. Channel *structure*
  (create/rename/archive) is durable history — `guild.channel_created`,
  `guild.channel_renamed`, `guild.channel_archived` go into the outbox in
  the same transaction as the `guild_channels` row change, gated on
  `manage_channels`. Individual chat *messages* are deliberately not:
  `guild_messages` rows never touch the outbox or the ledger, same
  "ephemeral, non-interoperable state" treatment already given to presence.
  **Retention**: messages are kept indefinitely up to a configurable cap
  per channel (`GUILD_CHANNEL_MESSAGE_CAP` env var, default 10,000), oldest
  pruned once a channel exceeds it — no client should assume guild chat
  history is permanent. Sending/reading requires current guild membership
  (via #21's `guild_members` table, now merged alongside #22); moderators
  (`manage_channels`) can hard-delete a message outright, since there's no
  history to preserve.
- The Rust SDK's guild surface (issue #23) is real:
  `crates/sdk/src/guilds.rs`'s `Session::guilds()` (`guilds.read`) lists the
  caller's own memberships, `Session::guild(id).roster()` (`guilds.read`)
  and `.channels()`/`.channel(cid).messages()`/`.send()` (`guilds.chat`)
  read the roster and chat and post as the player — never as the game.
  Creating guilds, inviting, kicking, changing roles, and managing channels
  stay Hub-only, not exposed on the SDK. See `docs/architecture/sdk.md`.
- Hub guild views (issue #24) are real: `/guilds` (my guilds + create),
  `/guilds/:id` (overview, roster grouped by role, roles, channels,
  management actions), `/guilds/:id/channels/:cid` (chat) in `apps/hub`,
  nested under the authenticated `HubShell` layout like every other page.
  Six new `packages/ui` components (`AvalonGuildCard`,
  `AvalonGuildMemberRow`, `AvalonRoleBadge`, `AvalonChannelList`,
  `AvalonChatMessage`, `AvalonChatComposer`) follow the same props-in/
  events-out, CSS-Modules pattern as the friends components from #18.
  Presence on roster rows is merged client-side
  (`apps/hub/src/api/guilds.ts::listMembersWithPresence`), same gap and
  same fix as `GET /friends`. Every management action (rename/describe,
  define roles, invite, kick, change role, transfer ownership, create/
  archive channels, associate a game) is gated client-side on the caller's
  own resolved permission list, but the server is the real authority — a
  hidden-but-still-reachable action shows a plain error on a 403 rather
  than crashing. `apps/mobile-hub` isn't wired to guilds yet (still #60).
  **Correction (2026-09-08, issue #24):** building this surfaced that
  `crates/server/src/channels.rs`'s `manage_channels` check was still
  calling a leftover stub (always-empty permissions) from when #22 was
  written concurrently with #21, rather than #21's real
  `guild_members`/`guild_roles` lookup — fixed alongside landing #24 by
  having `channels.rs` reuse `guilds::actor_role_permissions` directly
  instead of its own duplicate. Two real gaps remain, not worked around
  with a Hub-only endpoint: there is no endpoint that lists a player's own
  pending guild invites — `POST /guilds/{id}/invites` returns an invite id,
  but nothing resolves "invites addressed to me" the way
  `GET /friends/requests` does for friend requests, so an invited player
  has no way to discover or accept an invite through the Hub UI today; the
  invite id has to be shared out of band. And no endpoint ever sets
  `join_policy` to `"open"` (`CreateGuildRequest`/`UpdateGuildRequest`
  don't take it), so every guild is `invite_only` in practice — the Hub's
  "Join" button is wired for the day that changes but is currently dead
  code by construction, not by a Hub-side restriction.

## Decisions and tickets

- [#74](https://github.com/LunarVagabond/avalon-protocol/issues/74) — ADR: guilds
  are network-level primitives, not game-owned.
- [#75](https://github.com/LunarVagabond/avalon-protocol/issues/75) — ADR: durable
  history is canonical; the roster is a projection.
- [#19](https://github.com/LunarVagabond/avalon-protocol/issues/19) — Epic: Guilds
  & Guild Communication, with
  [#20](https://github.com/LunarVagabond/avalon-protocol/issues/20) (CRUD + roles,
  done — creation, rename/retag/redescribe, role definitions, ownership
  transfer, game association),
  [#21](https://github.com/LunarVagabond/avalon-protocol/issues/21) (membership,
  done — invites, join/leave, removal, per-member role assignment, and the
  owner-departure-without-transfer guard),
  [#22](https://github.com/LunarVagabond/avalon-protocol/issues/22) (channels +
  messages), [#23](https://github.com/LunarVagabond/avalon-protocol/issues/23)
  (SDK), [#24](https://github.com/LunarVagabond/avalon-protocol/issues/24) (Hub).
- [#87](https://github.com/LunarVagabond/avalon-protocol/issues/87) — visibility
  scopes, including roster visibility.
- Open questions from [Proposal §32](../stakeholders/Proposal.md#32-open-questions): guild
  ownership, leadership transfer.
