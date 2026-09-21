# Guilds

**An Avalon guild is a first-class primitive of the network, not of any game.**
It exists before its members enter a particular integrator, has members playing many
integrators at once, and survives any one of those integrators shutting down. **An integrator is a
client of a guild, never its owner.** Guild membership, roles, permissions, and
history belong to the network; an integrator may render and consume them, but cannot
govern them through its integrator authority.

This is decided, not aspirational — see
[#74](https://github.com/LunarVagabond/avalon-protocol/issues/74). The narrative
version is [Proposal §10](../../../stakeholders/Proposal.md#10-guilds-and-social-identity).

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
├── Integrators
│
└── Assets / Ownership
```

Guilds are a sibling of integrators, not a child of them. The same guild can be seen
from inside Ashen Realms, inside WorldZero, and from the Hub with no integrator open:

```text
Dragon Hunters

Members: 1,284
Founded: 2027
Integrators: 7
Chat: Active

Members currently playing:
    42 — Ashen Realms
    18 — WorldZero
     7 — Integrator C
```

Nothing above is integrator-scoped. The per-integrator counts are realtime presence (see
[presence](./presence.md)), not something any one integrator reports about "its" guild.

## What Avalon owns

| Concern | Owner | Notes |
|---|---|---|
| Guild existence, name, tag, description | Network | `Guild` in the protocol crate |
| Guild metadata (motd, banner, links, recruiting) | Network | owner/`manage_guild`-editable via `PATCH /guilds/{id}`; `recruiting` feeds the discovery board |
| Membership | Network | joins/leaves/removals are protocol events |
| Roles and permissions | Network | assigned by guild members with authority |
| Guild history | Network | append-only, reconstructable |
| Guild chat channels and messages | Network | delivered to any authorized client |
| Reputation, governance | Network | later; see [future layers](./future-layers.md) |
| An integrator's in-world rendering of a guild | Integrator | banners, halls, roster UI, whatever |
| Integrator-internal clans that never touch Avalon | Integrator | stay integrator-side entirely |

An integrator that wants purely internal clans keeps them in its own database and does
not put them on Avalon. Avalon does not try to model every integrator-internal group.

## Integrator as client

"Join Dragon Hunters" inside an integrator calls Avalon social functionality; the
membership change happens in the network layer, and the integrator then consumes it.
The only relationship between a guild and an integrator is `GuildIntegratorAssociation`:
opt-in, many-to-many, non-owning, removable without affecting the guild.

What an integrator can do through the association:

- render the roster and member presence (subject to [visibility](./privacy.md))
- render guild chat in its own UI as a client of the channel
- ask "is this identity a member of guild X, with role Y?"
- unlock integrator-side content based on membership

What an integrator cannot do: rename, dissolve, transfer, or govern a guild; remove
members; grant roles. Those are guild-role authorizations, not integrator credentials.
See [security model](./security-model.md) for the scoped-authority rules.

**[#160](https://github.com/LunarVagabond/avalon-protocol/issues/160) (decided)
reshapes this going forward**: a guild's connection to an integrator is never
something a manager declares for an integrator none of their members have actually
played — it can only ever come from what an integrator itself already knows via
[integrator bindings](./bindings.md) (#83, shipped). This is a **read/display
feature**, not new durable guild structure: a role with sufficient authority
(`manage_guild` or an equivalent permission) can see an aggregated,
derived breakdown of which integrators guildmates play or have played, with a
count/fraction of members per integrator — computed from binding data, not a
`guild.game_associated` protocol event, same "hot state, not history"
treatment already given to presence and chat. That role chooses whether to
show or hide this breakdown on the guild's public profile, and can pin up to
5 integrators as the guild's curated "favorites" for display — pinning requires
the underlying binding-derived association to already exist; it's never a
way to manufacture one. No minimum-member threshold is needed since nothing
is being gated on/off automatically — it's a display choice, not a system
verdict. Implementation tracked as
[#206](https://github.com/LunarVagabond/avalon-protocol/issues/206)
(affinity view) and
[#207](https://github.com/LunarVagabond/avalon-protocol/issues/207)
(favorites pin), both under the Guilds epic (#19); #20's original manual
`associate_integrator` endpoint predates this decision and is superseded by it
going forward.

## Guild chat is a network primitive

A channel belongs to the guild. It is visible through the Hub, the mobile-hub
companion app, an integrator that chooses to render it, a web client, and later a
Discord bridge or other authorized client. User A in Integrator A and User B in
Integrator B talk in the same channel. See
[Proposal §12](../../../stakeholders/Proposal.md#12-communication).

Guild chat is not gameplay chat. Integrators keep their own local chat; Avalon carries
the cross-integrator social channel.

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

## A user's main guild

A user may belong to several guilds at once, but an integrator building a
guild-chat-style UI often wants just one to build around rather than
supporting arbitrarily-many simultaneous memberships in its own interface.
`main_guild` (no ticket) is a self-chosen pointer to one of a user's own
current memberships, living on the identity's profile, not on the guild —
see [`./identity.md`](./identity.md#what-is-promised-durable) for the full
spec (validation, three-state PATCH semantics, the earliest-joined default
when unset). It never affects guild-side data: no guild is ever "the" main
guild, only a user's own pointer at one of their memberships.

## Analytics phrasing

Never describe network guilds as belonging to an integrator.

Wrong, unless the guilds are explicitly integrator-owned:

```text
Integrator A has 18,291 guilds.
```

Right:

```text
18,291 Avalon guild members are currently associated with Integrator A.
4,217 Avalon guilds have members who play Integrator A.
```

Metrics the [integrator registry](./registry.md) may derive: guild members
associated with an integrator, guilds with members playing an integrator, cross-integrator guild
activity, membership growth, active members, guild participation in integrator events,
guilds spanning multiple integrators.

## Scenario H — network guild

Can a guild exist outside Integrator A and have members simultaneously playing Integrator A,
Integrator B, and Integrator C? Yes, by construction: the guild has no integrator parent, members
have [bindings](./bindings.md) to whichever integrators they play, and presence
reports where each member currently is. If Integrator A shuts down, the guild, its
roster, its channels, and its history are unaffected; only the `GuildIntegratorAssociation`
with Integrator A becomes historical.

## Today in the repo

The full build-by-build detail lives in its own file —
[`./guilds-implementation-log.md`](./guilds-implementation-log.md) — so this
section stays a short index instead of a changelog. Everything below is real
and implemented unless noted otherwise.

- **Core types** (`crates/protocol/src/guilds.rs`): `Guild`, `JoinPolicy`
  (invite-only/open, #21), `GuildRole`, `GuildMember`, `GuildIntegratorAssociation`,
  `GuildChannel`, `GuildMessage`, and the fixed `GuildPermission` vocabulary (#20).
- **Per-resource permission overrides** (#250, decided by #243) — a role's
  guild-wide base permissions can be overridden per channel or per event,
  resolved through `has_resource_permission`; `manage_guild`/`manage_roles`/
  `manage_members` stay guild-wide only.
- **`event_manage`** (#250) — events got their own permission, split out from
  `manage_channels`.
- **Announcement-only channels** (#250) and **channel topics** (#276) — a
  per-channel posting gate, and a short topic line in the channel header.
- **Role descriptions and badges** (#152) — free-text description plus a
  closed icon/color badge enum, no custom image upload yet.
- **Guild page navigation** (#241) — tabbed layout (Overview/Members/
  Channels/Events/Roles/Settings), chat folded into the Channels tab.
- **Roster visibility and "members currently playing"** (#57) — real today,
  with a documented visibility gap tracked under #87.
- **Guild MOTD, banner, links, recruiting metadata** (#153), and **guild
  icon** (#246) as an independent image slot.
- **Independent `recruiting`/`public` settings** (#449, decided; #455,
  implemented) — `recruiting` governs the Discover board and join-request
  eligibility only; `public` independently governs whether the roster and
  public events are visible to any authenticated identity, regardless of
  recruiting status. Before #449, `recruiting` alone controlled roster
  visibility; a guild can now be public without recruiting, or recruiting
  without a public roster.
- **Guild discovery board** (#154) — browse/search recruiting guilds; its
  member-count and membership-scoped filters read `indexer_guild_members`
  directly (#506) rather than through a dedicated read-model function,
  since they're fragments of one larger dynamically-composed query
  (`guilds::build_discover_query`), not standalone lookups.
- **Guild integrator affinity view** (#206, implementing decision #160) and
  **favorite integrators pin list** (#207) — derived from real member bindings,
  never manager-declared.
- **Guild events calendar + RSVP** (#169), plus **per-member RSVP roster**
  (#248) — who's going/maybe/can't-go, not just aggregate counts.
  `EventResponse` also includes the caller's own RSVP status (`my_rsvp`,
  #463) alongside the aggregate `rsvp_counts`, so a client can pre-select
  its RSVP control without a separate roster fetch — always the caller's
  own row, never another member's.
- **Per-event public visibility** (#448) — a `public` flag on each
  `GuildEvent`, defaulted `false` (member-only, unchanged from before this
  shipped). A non-member of a `public` guild (#449) sees only that guild's
  `public` events via `GET /guilds/{id}/events`, instead of being 403'd
  outright; a member sees every event *unless* a `view`/`view_details`
  override says otherwise (#458, below). Deliberately per-event rather
  than all-or-nothing like the roster override — an event can be genuinely
  internal (officer planning, loot council) even in an otherwise-public
  guild.
- **Role-gated `view`/`view_details` permissions** (#458, implementing
  #454's decision) — two more entries in `GuildPermission`, resolved
  specially (`resolve_view_permission`, not the generic per-resource
  override fallback every other permission uses): a member's *baseline*
  (no override row) is `view = true, view_details = true`, unchanged
  behavior for any guild with no overrides at all. A role denied `view` on
  a specific event/channel never sees it in the list; a role denied only
  `view_details` still sees it exist (title/time for an event, the
  channel entry itself) but its content is stripped
  (`EventResponse.details_visible: false` zeroes
  `description`/`channel_id`/`rsvp_counts`/`my_rsvp`; a channel's message
  endpoints 403 instead). An explicit `view_details` grant always implies
  `view`, even under an explicit `view` deny — the two can never be a
  contradictory pair. Non-members follow the resource's own `public` flag
  for both, same as #448's existing behavior, extended to channels via a
  **new `guild_channels.public` column** (channels had no non-member
  visibility concept before this ticket at all — `GET
  /guilds/{id}/channels` and the message-read endpoints now honor it the
  same way events already did). Hub: `ResourcePermissionOverrides.vue`
  (generalized from the channel-only `ChannelPermissionOverrides.vue`)
  covers the role x permission grid for both resource kinds;
  `AvalonEventCard`'s `detailsVisible` prop renders the stripped state.
- **Guild join requests** (#242) and **withdrawing your own request** (#256).
- **Invite discovery** (#442) — `GET /me/guild-invites` lists every
  unresolved invite where the caller is the invitee, so accepting/declining
  no longer depends on the sender sharing the raw invite id out of band.
- **Role name uniqueness, open-guild joining, and role deletion** — the
  smaller correctness rules tying the rest of this together.

See [`./guilds-implementation-log.md`](./guilds-implementation-log.md) for
exact types, endpoints, and migrations behind every item above.

- **Guild chat survives a node's loss (#540).** Guild channel messages are
  now asynchronously replicated to at least one additional node beyond
  the one that received the write — see
  [`./communication.md`](./communication.md)'s "Today in the repo" entry
  for the full mechanics (a separate, foreign-key-free replica table,
  never the live `guild_messages` table itself).

## Decisions and tickets

- [#74](https://github.com/LunarVagabond/avalon-protocol/issues/74) — ADR: guilds
  are network-level primitives, not integrator-owned.
- [#75](https://github.com/LunarVagabond/avalon-protocol/issues/75) — ADR: durable
  history is canonical; the roster is a projection.
- [#19](https://github.com/LunarVagabond/avalon-protocol/issues/19) — Epic: Guilds
  & Guild Communication, with
  [#20](https://github.com/LunarVagabond/avalon-protocol/issues/20) (CRUD + roles,
  done — creation, rename/retag/redescribe, role definitions, ownership
  transfer, integrator association),
  [#21](https://github.com/LunarVagabond/avalon-protocol/issues/21) (membership,
  done — invites, join/leave, removal, per-member role assignment, and the
  owner-departure-without-transfer guard),
  [#22](https://github.com/LunarVagabond/avalon-protocol/issues/22) (channels +
  messages), [#23](https://github.com/LunarVagabond/avalon-protocol/issues/23)
  (SDK), [#24](https://github.com/LunarVagabond/avalon-protocol/issues/24) (Hub,
  done), [#57](https://github.com/LunarVagabond/avalon-protocol/issues/57)
  (roster/chat completion against #87's currently-real visibility rule, the
  "members currently playing" summary, and the honest guild-history gap —
  #57's proposed `AvalonRosterRow`/`AvalonMessageList`/`AvalonMessageComposer`
  component names describe what #24 already built as `AvalonGuildMemberRow`/
  `AvalonChatMessage`/`AvalonChatComposer`; not duplicated under new names).
- [#241](https://github.com/LunarVagabond/avalon-protocol/issues/241) — guild
  page tabbed layout, done: Overview/Members/Channels/Events/Roles/Settings
  tabs, chat folded into a Channels tab (persistent sidebar, no per-channel
  route hop) replacing the standalone `GuildChannel.vue` route, and the
  first Hub wiring for #153's `motd`/`banner`/`links`/`recruiting` fields.
- [#152](https://github.com/LunarVagabond/avalon-protocol/issues/152) — role
  descriptions and badges, done.
- [#153](https://github.com/LunarVagabond/avalon-protocol/issues/153) — guild
  MOTD/banner/links/recruiting metadata, done.
- [#154](https://github.com/LunarVagabond/avalon-protocol/issues/154) — guild
  discovery board (browse + search recruiting guilds), done; its
  member/membership queries now read `indexer_guild_members`, closed
  alongside [#506](https://github.com/LunarVagabond/avalon-protocol/issues/506).
- [#160](https://github.com/LunarVagabond/avalon-protocol/issues/160) — decided:
  guild-integrator association is derived from real member bindings, never
  manager-declared; superseded #20's `associate_integrator`. Implemented by
  [#206](https://github.com/LunarVagabond/avalon-protocol/issues/206) (integrator
  affinity breakdown, done) and
  [#207](https://github.com/LunarVagabond/avalon-protocol/issues/207)
  (favorites pin, done — a curated top-5 subset of #206's breakdown, gated
  the same way and validated against the same live data, never a way to
  manufacture an association #206 wouldn't itself show).
- [#246](https://github.com/LunarVagabond/avalon-protocol/issues/246) — guild
  icon, done: a small badge image, independent of #153's `banner`.
- [#258](https://github.com/LunarVagabond/avalon-protocol/issues/258) — Discover
  cards surface `banner`/`icon`, done: closes the gap #246 left open between
  the "My guilds" list and the Discover tab.
- [#248](https://github.com/LunarVagabond/avalon-protocol/issues/248) —
  per-member RSVP roster, done: who's going/maybe/can't-go per event, not
  just aggregate counts.
- [#242](https://github.com/LunarVagabond/avalon-protocol/issues/242) — guild
  join applications (request-to-join + manager review), done.
- [#256](https://github.com/LunarVagabond/avalon-protocol/issues/256) — let an
  applicant withdraw their own pending join request, done.
- [#243](https://github.com/LunarVagabond/avalon-protocol/issues/243) —
  decided: expand the fixed milestone-1 `GuildPermission` set for events +
  channels. Implemented by
  [#250](https://github.com/LunarVagabond/avalon-protocol/issues/250)
  (per-resource permission overrides, done), described above.
- [#193](https://github.com/LunarVagabond/avalon-protocol/issues/193) — decided:
  guild chat message retention keeps the existing count cap, but cap-pruned
  messages move to a long-window archive tier instead of being hard-deleted.
  Implemented by [#253](https://github.com/LunarVagabond/avalon-protocol/issues/253)
  (archive tier, done): `guild_messages_archive` table, archive read access
  scoped to current membership (not membership as of send time), and
  moderation `delete_message` reaching an already-archived row so a
  takedown can't be defeated by pruning timing. The Hub reads this tier
  too ([#464](https://github.com/LunarVagabond/avalon-protocol/issues/464),
  done): `useGuildChat.ts`'s "load older" falls through to
  `GET .../messages/archive` once the live table's before-cursor
  pagination comes up short of a full page, rather than treating that as
  the end of history.
- [#87](https://github.com/LunarVagabond/avalon-protocol/issues/87) — visibility
  scopes, including roster visibility.
- [#449](https://github.com/LunarVagabond/avalon-protocol/issues/449) —
  decided: `recruiting` and `public` are independent guild settings, not one
  boolean doing both jobs. Implemented by
  [#455](https://github.com/LunarVagabond/avalon-protocol/issues/455), done:
  `Guild.public`, `list_members`'s roster-visibility override re-keyed from
  `recruiting` to `public`, a second "Public" toggle in the Hub's guild
  settings tab.
- [#448](https://github.com/LunarVagabond/avalon-protocol/issues/448) —
  per-event public visibility, done: a `public` flag on `GuildEvent`, a
  non-member of a `public` guild (#449) sees only that guild's public
  events instead of a flat 403, and the Hub's Events/Calendar tabs are
  reachable read-only for that case (RSVP/roster stay member-only).
- [#450](https://github.com/LunarVagabond/avalon-protocol/issues/450) —
  decided: an integrator reads a guild's already-public data (per its own
  visibility settings) the same way `GET /guilds/{id}` works today —
  authenticate only, no `Capability` binding — and at resource-level
  granularity (a flag per roster/event), not a per-field mask. Not yet
  implemented as an SDK-facing read path.
- [#454](https://github.com/LunarVagabond/avalon-protocol/issues/454) —
  decided: a binary public/private flag per resource isn't enough for
  guild events/chat specifically (a public "community mixer" vs. an
  officers-only meeting is an audience question, not on/off) — role-gated
  `view`/`view_details` permissions, layered on top of (not replacing)
  #448/#449/#450's public-flag baseline. Implemented by
  [#458](https://github.com/LunarVagabond/avalon-protocol/issues/458),
  done — see "Today in the repo" above.
- Open questions from [Proposal §32](../../../stakeholders/Proposal.md#32-open-questions): guild
  ownership, leadership transfer.
