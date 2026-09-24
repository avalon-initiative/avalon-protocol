# Guilds

**An Avalon guild is a first-class primitive of the network, not of any game.**
It exists before its members enter a particular integrator, has members playing many
integrators at once, and survives any one of those integrators shutting down. **An integrator is a
client of a guild, never its owner.** Guild membership, roles, permissions, and
history belong to the network; an integrator may render and consume them, but cannot
govern them through its integrator authority.

The narrative version is [Proposal §10](../../../stakeholders/Proposal.md#10-guilds-and-social-identity).

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

A guild's connection to an integrator is never something a manager declares for an
integrator none of their members have actually played — it can only ever come from
what an integrator itself already knows via [integrator bindings](./bindings.md). This
is a read/display feature, not durable guild structure: a role with sufficient
authority (`manage_guild` or an equivalent permission) can see an aggregated, derived
breakdown of which integrators guildmates play or have played, with a count/fraction of
members per integrator, computed from binding data rather than a durable event — the
same "hot state, not history" treatment given to presence and chat. That role chooses
whether to show or hide this breakdown on the guild's public profile, and can pin up to
5 integrators as the guild's curated "favorites" for display; pinning requires the
underlying binding-derived association to already exist, and is never a way to
manufacture one. There is no minimum-member threshold — this is a display choice, not
a system verdict.

A manual "associate an integrator" endpoint (`POST
/guilds/{id}/integrations/{integrator_id}`) also exists and still works, but is
superseded by the binding-derived affinity view above as the intended way to express a
guild-integrator connection.

## Guild chat is a network primitive

A channel belongs to the guild. It is visible through the Hub, the hub-app
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
preserve lives only in a mutable row — durable history is canonical, and the roster is
always a projection over it.

## A user's main guild

A user may belong to several guilds at once, but an integrator building a
guild-chat-style UI often wants just one to build around rather than
supporting arbitrarily-many simultaneous memberships in its own interface.
`main_guild` is a self-chosen pointer to one of a user's own
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

## Current implementation

### Core types and permissions

`crates/protocol/src/guilds.rs` defines `Guild` (carrying `join_policy`), `JoinPolicy`
(`InviteOnly` | `Open`), `GuildRole`, `GuildMember`, `GuildIntegratorAssociation`,
`GuildChannel` (carrying `announcement_only`, `topic`, and `public`), `GuildMessage`,
and `GuildPermission` — a fixed, closed vocabulary a role's base permission list draws
from: `manage_guild`, `manage_roles`, `manage_members`, `manage_channels`,
`event_manage`, `channel_post`, plus the role-gated `view`/`view_details` pair described
below. There are no custom permission names.

**Per-resource permission overrides.** A role's `permissions` list is its base grant,
applying guild-wide by default. `GuildPermissionOverride` rows, stored in
`guild_permission_overrides`, let a `manage_roles` holder grant or deny one
`GuildPermission` to one role, scoped to a single channel or event
(`GuildResourceKind::Channel` / `Event`). Resolution
(`crates/server/src/guilds.rs::resolve_resource_permission`, driven by
`has_resource_permission`): the guild owner's structural bypass is untouched by any
override; for anyone else, an override for the exact (role, resource, permission)
triple — when one exists — decides the outcome outright (an explicit deny beats a base
grant, an explicit grant beats a base absence); with no override, the role's flat base
list is the answer. An override pointing at a since-deleted channel/event is inert, not
an error — every endpoint that consults overrides already fetches (and 404s on) the
resource first, so a dangling override is never reached.
`GET`/`PUT /guilds/{id}/permission-overrides` and `DELETE
/guilds/{id}/permission-overrides/{override_id}` (all `manage_roles`-gated) are the CRUD
surface; the Hub's Channels tab exposes per-role overrides for the active channel via
its own permission-grid component, reused for events too.
`manage_guild`/`manage_roles`/`manage_members` stay guild-wide only — they have no
per-instance resource to scope to — so every endpoint gated on one of those three uses
the flat `has_guild_permission` check; only channel/event endpoints with an obvious
resource use the resource-aware check, and creation endpoints (no resource id yet at
that point) use a flat, guild-wide check.

`event_manage` is its own permission, split out of `manage_channels`: `create_event`
checks it guild-wide (no event exists yet to scope a resource-aware check to);
`update_event`/`delete_event` check it resource-aware against the specific event, so a
role can be granted or denied management of one event via an override, on top of or
instead of holding `event_manage` guild-wide.

**Announcement-only channels.** `GuildChannel.announcement_only` (default `false`) is a
per-channel flag: when set, posting requires the `channel_post` permission for that
specific channel (resolved through the override layer above), instead of the default
"any current guild member may post." A regular channel keeps the default behavior. No
role holds `channel_post` in its base list by default; a guild opts a role into posting
in a specific announcement-only channel by granting it a `channel_post` override on
that channel, or by adding `channel_post` to the role's base list.

**Channel topics.** `GuildChannel.topic` (default `NULL`) is a short line (capped at 200
characters) describing what a channel is for. `None`/unset means no topic; an empty
string is never stored. Editable via `PATCH .../channels/{cid}` alongside a rename,
gated the same `manage_channels` (resource-aware) check as the rest of
`crates/server/src/channels.rs`. The guild's MOTD lives in its own persistent banner
above the tab bar on the guild page, not buried in the Overview tab.

**Role descriptions and badges.** A guild role carries a `description` (free text,
capped at 200 characters) and a `badge` — a small, fixed visual identity, not a
free-form upload: an icon id from a closed enum (`RoleBadgeIcon` — `shield`, `crown`,
`star`, `sword`, `wrench`, `heart`, `flag`, `bolt`) paired with a color id from a closed
enum (`RoleBadgeColor` — `gray`, `red`, `orange`, `gold`, `green`, `blue`, `purple`),
both defined as `RoleBadge { icon, color }`. There is no user-supplied image hosting for
badges; the icon+color shape leaves room to grow into a richer badge system later
without a breaking change. `create_role`/`update_role` validate both fields — an
out-of-range description or unrecognized badge icon/color is a rejected (400) request.
The starter roles get sensible defaults (`owner` → crown/gold, `officer` → shield/blue,
`member` → star/gray).

### Guild, membership, and channel lifecycle

Guild creation, rename/retag/redescribe, roles, ownership transfer, and membership
lifecycle are served by `avalon-server` (`crates/server/src/guilds.rs`): `POST
/guilds`, `GET /guilds/{id}`, `PATCH /guilds/{id}`, `GET /guilds/{id}/roles`, `POST
/guilds/{id}/roles`, `PATCH /guilds/{id}/roles/{idx}`, `POST
/guilds/{id}/transfer-ownership`, `POST /guilds/{id}/integrations/{integrator_id}`, `GET
/guilds/{id}/integrator-breakdown`, `GET`/`PUT /guilds/{id}/favorite-integrators`, `POST
/guilds/{id}/invites`, `POST /guilds/{id}/invites/{invite_id}/accept`, `POST
/guilds/{id}/invites/{invite_id}/decline`, `POST /guilds/{id}/join` (open guilds only),
`POST /guilds/{id}/leave`, `DELETE /guilds/{id}/members/{identity_id}`, `PATCH
/guilds/{id}/members/{identity_id}`, `GET /guilds/{id}/members`, and `GET /me/guilds`.
Every route is session-authenticated only. `guild.created`, `guild.updated`,
`guild.role_defined`, `guild.owner_transferred`, `guild.member_added`,
`guild.member_removed`, and `guild.role_changed` are written into the outbox in the
same transaction as the corresponding projection change. Invites, declines, and
withdrawals are deliberately not durable — resolving one is a plain
`guild_invites` projection update, no event; a pending invite is idempotent
(re-inviting while one is outstanding returns the existing invite rather than erroring
or duplicating it). A guild is created with a fixed starter role set (`owner`,
`officer`, `member`) and its owner's own `guild_members` row at `role_index = 0`;
`owner` always has every permission structurally (the `guilds.owner` column), not
through its role row, so it can't be edited away or removed, and the owner must
transfer ownership before leaving. `actor_role_permissions` does a real `guild_members`
JOIN `guild_roles` lookup, so `manage_guild`/`manage_roles` permission checks work for
non-owner members too; `member_count` on `GET /guilds/{id}` is a real `COUNT(*)` over
`guild_members`. Removing a member holding an elevated (non-`member`) role additionally
requires `manage_roles`, not just `manage_members` — an officer can remove a plain
member but not another officer.

`GET /me/guild-invites` lists every unresolved invite where the caller is the invitee,
so accepting or declining doesn't depend on the sender sharing the raw invite id out of
band.

Guild chat channels and messages are served by `crates/server/src/channels.rs`
(`GET`/`POST /guilds/{id}/channels`, `PATCH .../channels/{cid}`, `POST
.../channels/{cid}/archive`) and `crates/server/src/guild_messages.rs` (`GET`/`POST
.../channels/{cid}/messages`, `DELETE .../channels/{cid}/messages/{mid}`). Every guild is
seeded with a default `general` channel on creation. Channel *structure*
(create/rename/archive) is durable history — `guild.channel_created`,
`guild.channel_renamed`, `guild.channel_archived` go into the outbox in the same
transaction as the `guild_channels` row change, gated on `manage_channels`. Individual
chat *messages* are deliberately not: `guild_messages` rows never touch the outbox or
the ledger, the same "ephemeral, non-interoperable state" treatment given to presence.

**Retention.** Messages are kept indefinitely up to a configurable cap per channel
(`GUILD_CHANNEL_MESSAGE_CAP` env var, default 10,000); once a channel exceeds it, the
oldest messages move into a `guild_messages_archive` table instead of being deleted
outright. The archive itself is held only for a long, separately configurable window
(`GUILD_MESSAGE_ARCHIVE_RETENTION_DAYS`, default 730 days), after which a background
worker hard-deletes whatever falls past it — a genuine, final delete, nothing
recoverable afterward. The archive is operational/server-policy state, not durable
protocol history — no `guild.message_*` ledger event exists, and neither tier ever
touches the outbox or `SettlementProvider`. No client should assume guild chat history
is permanent, in the live table or the archive. `GET
.../channels/{cid}/messages/archive` reads the archive, same newest-first,
cursor-paginated shape as the live endpoint, gated on current guild membership rather
than membership as of when each archived message was originally sent (`guild_members`
is a live projection with no point-in-time history). Moderators (`manage_channels`) can
hard-delete a message outright, since there's no history to preserve; `delete_message`
checks both `guild_messages` and `guild_messages_archive` for the target id, so a
moderator's takedown for cause isn't defeated by cap-based pruning having already
archived the row first. The Hub's chat view falls through to the archive endpoint once
the live table's before-cursor pagination comes up short of a full page, rather than
treating that as the end of history.

**Guild chat survives a node's loss.** Guild channel messages are asynchronously
replicated to at least one additional node beyond the one that received the write —
see [`./communication.md`](./communication.md)'s replication section for the full
mechanics (a separate, foreign-key-free replica table, never the live `guild_messages`
table itself).

### SDK and Hub surfaces

The Rust SDK's guild surface exposes `Session::guilds()` (`guilds.read`, lists the
caller's own memberships), `Session::guild(id).roster()` (`guilds.read`) and
`.channels()`/`.channel(cid).messages()`/`.send()` (`guilds.chat`), reading the roster
and chat and posting as the identity, never as the integrator. Creating guilds,
inviting, kicking, changing roles, and managing channels stay Hub-only, not exposed on
the SDK.

Hub guild views live at `/guilds` (my guilds + create), `/guilds/:id` (overview, roster
grouped by role, roles, channels, management actions), `/guilds/:id/channels/:cid`
(chat), nested under the authenticated Hub shell layout like every other page. The
guild page is tabbed — Overview/Members/Channels/Events/Roles/Settings — rather than
one long scrolling page. Overview carries header info, MOTD/banner/links (read-only),
integrator affinity, favorite integrators, and associated integrators (visible to any
member, read-only); Members carries the roster, currently-playing summary, and invites;
Roles and Events are self-contained tabs; Settings carries the recruiting toggle,
MOTD/banner/link editing, and ownership transfer, all `canManageGuild`-gated. Chat is
folded into the Channels tab as a persistent channel-list sidebar next to the active
channel's messages, rather than a separate route per channel — picking a different
channel just reassigns the loaded channel id, with no route navigation or component
remount per switch. `/guilds/:id/channels/:cid` still resolves to the same page with
that channel pre-selected, so deep links keep working; selecting a channel from the
sidebar updates the URL without adding a history entry, so a channel stays
link-shareable.

Every management action (rename/describe, define roles, invite, kick, change role,
transfer ownership, create/archive channels, associate an integrator) is gated
client-side on the caller's own resolved permission list, but the server is the real
authority — a hidden-but-still-reachable action shows a plain error on a 403 rather
than crashing. `avalon-hub/apps/hub-app` is not yet wired to guilds.

Roster, channel, and message reads all require current guild membership
(session-authenticated, checked against `guild_members`); a non-member's request is
rejected outright. There is no separate "public"/"hidden" roster mode beyond the
`view`/`view_details` role-gating and guild/event/channel `public` flags described
below.

"Members currently playing" groups the roster's merged presence by `playing`
integrator id and renders "N members playing X" — never "Integrator X's guild" — as a
live presence stat, never a durable one. `PresenceResponse.playing` is null unless an
integrator actively publishes a live presence binding, so this renders an honest empty
state ("No members currently reporting an in-game presence") until one does.

There is a "History" card on the guild page, but it states plainly that history isn't
available yet rather than fabricating a feed from the current roster/role snapshot —
there is no `GET /guilds/{id}/history` endpoint or indexer projection over `guild.*`
events exposed to any client today. The events themselves are durable; only a read
path for them is missing.

### Guild metadata, icon, and discovery

`Guild` carries `motd` (capped prose, `None` means unset), `banner` (an `http`/`https`
URL, same validation as a profile's `avatar_url`), `links` (an ordered, capped list of
`{ label, url }` pairs, stored as one JSONB column — full replace on update, not a
per-entry patch), and `recruiting` (a plain boolean, defaulting to `false`). All four
are owner/`manage_guild`-editable via `PATCH /guilds/{id}` and returned by `GET
/guilds/{id}` alongside name/tag/description/member_count, all readable by any
authenticated identity.

`Guild.icon` is a second, independent image field alongside `banner` — a small
badge/identity mark (recruitment card, member-list-style avatar) rather than
`banner`'s wide cover-image role. Same validation and update convention as `banner`:
`http`/`https`-only, length-capped, three-state `PATCH /guilds/{id}` update (omitted
untouched, `""` clears, non-empty validates and sets). No protocol event of its own —
the same operational-state posture `motd`/`banner`/`links` have. The Hub surfaces it in
the guild page header, the Settings tab's profile card, and on guild cards in both the
"My guilds" list and the Discover tab.

**`recruiting` and `public` are independent guild settings.** `recruiting` governs the
Discover board and join-request eligibility only; `public` independently governs
whether the roster and public events are visible to any authenticated identity,
regardless of recruiting status — a guild can be public without recruiting, or
recruiting without a public roster.

**Guild discovery board.** `GET
/guilds/discover?q=&recruiting=&tag=&integrator=&sort=&limit=&cursor=`
(`crates/server/src/guilds.rs::discover_guilds`) is a paged, filterable,
session-authenticated browse over already-public guild metadata — no membership
requirement, and no new visibility tier. Its membership-scoped filters/counts read
`indexer_guild_members` (the indexer's `guild_rosters` projection) directly, as
fragments of one larger dynamically-composed `QueryBuilder` query
(`build_discover_query`), rather than through a dedicated read-model function.

- **Filters**: `q=` does a case-insensitive substring match across
  `name`/`tag`/`description`; `tag=` is an exact case-insensitive match (indexed via
  `guilds_tag_lower_idx`, alongside the existing partial `guilds_recruiting_idx`);
  `integrator=` filters to guilds with a matching row in
  `guild_integrator_associations`.
- **`recruiting` visibility rule**: a non-recruiting guild must never appear in a
  *stranger's* browse/search results, in any filter combination — only exact id/tag
  lookup (`GET /guilds/{id}`) reaches it. `recruiting=true` is a plain exact filter (no
  membership gate needed — recruiting guilds are already public-by-design). Omitting it
  falls back to "recruiting guilds, plus any guild the caller already belongs to."
  `recruiting=false` explicitly is **still membership-gated**, not a raw exact filter —
  it returns only the caller's own non-recruiting guilds, so a stranger cannot use it
  to bulk-enumerate every non-recruiting guild's public metadata.
- **Sort**: `newest` (default, `created_at DESC`), `alphabetical` (`name ASC`),
  `most_members` (a `COUNT(*)` over `indexer_guild_members`, `DESC`). No
  trending/engagement ranking.
- **Pagination**: cursor-based. `cursor=` is the last guild id from the previous page
  (not an opaque blob) — the server re-resolves that row's own sort key via a subquery
  keyed on the id and does a keyset `(sort_key, id) < (...)` comparison, the same
  pattern message pagination's `before=` uses under a different field name.
- Hub: a "Discover" tab on `/guilds` — search box, recruiting-only toggle (on by
  default), tag filter, and a "Load more" button walking `next_cursor`.

### Integrator affinity and favorites

`GET /guilds/{id}/integrator-breakdown` (`crates/server/src/guilds.rs::game_breakdown`)
returns, for a guild, how many current members hold an active
[`IntegratorBinding`](./bindings.md) to each integrator they play — computed on every
read from `indexer_guild_members` JOIN `bindings` (`ended_at IS NULL`) JOIN
`integrators`, grouped by integrator. No protocol event and no durable table backs the
breakdown itself — it's derived/computed data, the same "hot state, not history" tier
as presence and the discovery board. No minimum-member threshold: every integrator with
at least one bound member appears, since this is a display of real counts, not a
system verdict. There is no "add" action — the only way an integrator appears is a
member actually holding an active binding to it.

- **Permission gate.** `can_view_game_breakdown` reuses the existing `manage_guild`
  permission (or guild ownership via the structural owner check) — a `manage_guild`
  holder can always see the breakdown. Anyone else (including a non-member) is only let
  in when the guild has opted into public exposure. A rejected caller gets a 403
  (`MissingGuildPermission`).
- **Public-profile exposure toggle.** `guilds.game_breakdown_public` is a plain boolean
  column, `false` by default, edited via `PATCH /guilds/{id}`
  (`UpdateGuildRequest.game_breakdown_public`, omitted-means-untouched) and folded into
  `guild.updated`'s existing payload. It controls only whether `game_breakdown` lets a
  non-permitted caller through — it never gates the `manage_guild` role's own internal
  view.
- **Response shape** (`GameBreakdownResponse`): `guild_id`, `total_members` (the
  guild's current membership — the denominator for "N of M members play X"), and
  `breakdown: GameBreakdownEntry[]` (`integrator_id`, `integrator_slug`,
  `integrator_name`, `member_count`), ordered by `member_count` descending.
- Hub: the guild page's "Integrator affinity" card renders each entry as "N of M
  members play X" and shows the public-exposure toggle to a `manage_guild` holder,
  fetched independently of the rest of the guild page since a 403 here is an expected,
  common outcome for a non-member.

**Favorite integrators** layer directly on the affinity breakdown: a `manage_guild`
holder may pin up to 5 integrators, in order, as the guild's curated "favorites" for
public display — but only integrators that already show up in the breakdown (at least
one currently-actively-bound member). There is no way to pin an integrator the guild
has no real, live connection to.

- **Storage.** `guild_favorite_games` (`(guild_id, integrator_id, position)`) is a
  small table rather than a capped JSONB/array column, because a pin's validity
  depends on live data in another table (`bindings`, via `guild_members`), not just
  static per-entry validation, and each pin needs its own stable position for
  reordering. Postgres enforces "no duplicate pin per integrator" (composite primary
  key) and "distinct positions per guild" (a unique index on `(guild_id, position)`)
  structurally; the 5-entry cap and the live-affinity check are application-level. No
  durable history table beyond the outbox event on each write — this is
  current-state-only.
- **Validation reuses the affinity query.** `guilds::guild_bound_integrator_ids` calls
  the exact same query the breakdown endpoint uses, collecting the set of integrator
  ids with at least one actively-bound member. A pin attempt for any other integrator
  id is rejected (403, `FavoriteGameNotBound`) — checked against this live query at
  write time, never a cached/stale value.
- **Endpoints and permission gate.** `PUT /guilds/{id}/favorite-integrators`
  (`SetFavoriteGamesRequest { integrator_ids: Vec<Uuid> }`) always sends the full
  desired ordered list, and is gated by `has_guild_permission(...,
  GuildPermission::ManageGuild)`, the same check `update_guild`/the breakdown-visibility
  toggle use. Rejects more than 5 entries (`TooManyFavoriteGames`), a duplicate
  integrator id in the same request (`DuplicateFavoriteGame`), or any id failing the
  live-affinity check. On success it replaces the stored rows (delete + reinsert under
  one transaction) and records a `guild.favorite_games_updated` outbox event. `GET
  /guilds/{id}/favorite-integrators` returns the same shape read-only, gated only by
  session authentication.
- **Staleness, not silent removal.** If a pinned integrator's last bound member later
  unbinds, the pin is not auto-removed, since that would churn the guild's public
  display on a single member's binding change. Instead, every read recomputes
  `stale: bool` per entry against the same live bound-integrator set, so a
  `manage_guild` holder sees exactly which pins no longer reflect a real binding and
  can choose to unpin them; a non-manager viewing the public profile still sees the
  pin with the same `stale` flag available.
- **Public display.** Unlike the full breakdown (gated behind `game_breakdown_public`),
  the favorites list is always part of a guild's public profile:
  `GuildResponse.favorite_games` is returned from `GET /guilds/{id}`. The discovery
  board's list endpoint is a separate summary shape and doesn't embed favorites, to
  avoid an N+1 query per browsed guild.
- Hub: the guild page's "Favorite integrators" card shows the pinned list (with
  staleness rendered inline) to anyone once there's something to show, and adds
  pin/unpin/reorder controls for a `manage_guild` holder. Pin candidates are drawn only
  from the caller's own fetched breakdown, so there is no UI path to attempting a pin
  without real affinity.

### Guild events, RSVP, and visibility

A guild plans things — raid nights, tournament prep, meetups — regardless of which
integrator (if any) members currently have open. `GuildEvent`/`GuildEventRsvp`
(`crates/protocol/src/guilds.rs`) are served by `crates/server/src/guild_events.rs`:
`GET`/`POST /guilds/{id}/events`, `PATCH`/`DELETE /guilds/{id}/events/{eid}`, `PUT
/guilds/{id}/events/{eid}/rsvp`. This is distinct from integrator event *result*
attestations — durable claims issued after the fact about outcomes; a guild event is
scheduling something upcoming.

**Durability posture.** Unlike guild channels, whose structure is durable history
behind `guild.channel_*` outbox events, a guild event gets no protocol event kind at
all, for either the event row or its RSVPs. A scheduled event doesn't have the durable
structure a channel does: a raid night gets rescheduled or cancelled repeatedly, and
that churn isn't history worth preserving forever any more than the chat that happens
in a channel is. The whole feature — `guild_events` and `guild_event_rsvps` alike —
gets the same "hot state, not history" treatment given to `guild_messages` and
presence. Deleting an event is a real hard delete and cascades to its RSVPs via the
`guild_event_rsvps` table's `ON DELETE CASCADE` foreign key. Creating, rescheduling,
and deleting an event are gated on `event_manage`. RSVPing is self-service and
idempotent: `PUT .../rsvp` always upserts the caller's own `(event_id, identity_id)`
row, replacing any prior status rather than accumulating rows; a member can never
target another member's RSVP. Listing and RSVPing both require current guild
membership, same as channels/messages. The Rust SDK exposes a read-only
`Session::guild(id).events()` (`guilds.read`), mirroring `channels()`; create/update/
delete/RSVP stay identity-authority Hub-only actions, same posture channel management
already has.

**Per-member RSVP roster.** The event list's `rsvp_counts` is aggregate-only. `GET
/guilds/{id}/events/{eid}/rsvps` returns every row (`identity_id`, `status`,
`responded_at`) unaggregated, gated the same as `list_events`/`rsvp_counts` — current
guild membership only, since RSVP status is ordinary guild-internal social info rather
than a moderation concern. `EventResponse` also includes the caller's own RSVP status
(`my_rsvp`) alongside the aggregate `rsvp_counts`, so a client can pre-select its RSVP
control without a separate roster fetch. Hub resolves the returned identity ids to
display names via the batched profile lookup, groups them into going/maybe/can't-go
buckets, and renders the roster from a shared modal component reachable by clicking an
event card from either the Events tab or the Calendar tab.

**Per-event public visibility.** A `public` flag on each `GuildEvent`, defaulted
`false` (member-only). A non-member of a `public` guild sees only that guild's
`public` events via `GET /guilds/{id}/events`, instead of being 403'd outright; a
member sees every event unless a `view`/`view_details` override says otherwise. This is
deliberately per-event rather than all-or-nothing like the roster override — an event
can be genuinely internal (officer planning, loot council) even in an otherwise-public
guild.

**Role-gated `view`/`view_details` permissions.** Two more entries in
`GuildPermission`, resolved specially (`resolve_view_permission`, not the generic
per-resource override fallback every other permission uses): a member's baseline (no
override row) is `view = true, view_details = true`, unchanged behavior for any guild
with no overrides at all. A role denied `view` on a specific event/channel never sees it
in the list; a role denied only `view_details` still sees it exist (title/time for an
event, the channel entry itself) but its content is stripped
(`EventResponse.details_visible: false` zeroes `description`/`channel_id`/
`rsvp_counts`/`my_rsvp`; a channel's message endpoints 403 instead). An explicit
`view_details` grant always implies `view`, even under an explicit `view` deny — the
two can never be a contradictory pair. Non-members follow the resource's own `public`
flag for both, extended to channels via a `guild_channels.public` column — channels
had no non-member visibility concept before this at all; `GET /guilds/{id}/channels`
and the message-read endpoints now honor it the same way events already did. Hub:
a shared role x permission grid component covers both resource kinds; the event card
renders the stripped state when `details_visible` is `false`.

- **Events** (`guild_events::list_events`): a per-event `can_view` check filters the
  list (denied `view` → not returned at all); a per-event `view_details` check
  controls whether `event_response` returns real content or a stripped placeholder.
  `EventResponse` gained `details_visible: bool` — `false` means
  `channel_id`/`description`/`rsvp_counts`/`my_rsvp` are zeroed placeholders, not real
  data, while `id`/`guild_id`/`title`/`starts_at`/`ends_at`/`created_by`/`created_at`/
  `public` stay real. Always `true` for create/update/RSVP responses, since those all
  require the actor to already hold `event_manage` or be RSVPing to their own record.
- **Channels** (`channels::list_channels`): message content
  (`guild_messages::list_messages`/`list_archive`) is gated on `view_details` instead
  of plain membership — same baseline for an existing channel with no overrides, but
  now also reachable by a non-member of a public channel in a public guild, and
  deniable per-role per-channel like everything else in the override layer.

### Join requests

`guild_join_requests` is the applicant-initiated counterpart to `guild_invites` — a
stranger applying to a `recruiting` guild from the Discover board instead of waiting on
a manager to invite them. Same projection posture as `guild_invites`: a
pending/approved/rejected/withdrawn transition is not itself durable history, only the
resulting membership-add on approval is (through the same `add_member` helper
`accept_invite`/`join_guild` already use, so approval can't drift from what an accepted
invite or a direct open-guild join produce). `POST /guilds/{id}/join-requests` rejects
applying to a non-recruiting guild (`GuildNotRecruiting`) and to a guild the caller
already belongs to; a second apply while one is already pending is idempotent,
returning the existing pending row rather than erroring or duplicating it (a partial
unique index on `(guild_id, applicant) WHERE status = 'pending'`). `GET
/guilds/{id}/join-requests` (pending-only by default, `?status=all` for the full
history), `POST .../join-requests/{id}/approve`, and `POST
.../join-requests/{id}/reject` are all `manage_members`-gated; `DELETE
/guilds/{id}/join-requests/{id}` lets only the applicant withdraw their own pending
request.

`GET /guilds/{id}/join-requests/mine` lets an applicant discover and manage their own
pending request without `manage_members` — it only ever returns the caller's own
pending request for the guild, or `null` on a 200 rather than a 404 for "none." The
Hub's Membership card checks it directly: a pending request shows its status and a
"Withdraw request" button, alongside — or instead of, for a recruiting invite-only
guild with no pending request — an "Apply to join" action. Scoped to the single
guild's own page; there is no cross-guild "my applications" view.

Hub: an "Apply to join" action on Discover guild cards (shown only for recruiting
guilds the caller isn't already a member of), and an "Applications" section on the
guild page (`manage_members`-gated) for managers to review pending requests.

### Smaller correctness rules

`guild_roles` has a case-insensitive unique index on `name` (`guild_roles_name_lower_idx`,
`(guild_id, lower(name))`), with a unique-violation caught in `create_role`/`update_role`
as `GuildRoleNameTaken` (409). `join_policy` (`invite_only`/`open`) can be toggled via
`UpdateGuildRequest.join_policy`, with `can_join_directly` gating the direct-join path
for open guilds. `update_role` rejects a `permissions` change on the owner role (name
index `0`) — the owner's authority comes from `guilds.owner`, not this row — but allows
cosmetic edits (name/description/badge). `DELETE /guilds/{id}/roles/{idx}` rejects
deleting the owner and member roles outright, and relies on `guild_members`'s existing
foreign key into `guild_roles` to reject deleting any other role still held by a member,
mapped to `RoleHasMembers` (409). Hub: the Roles tab's permission matrix has a pencil
icon that unlocks a row for renaming/permission edits, and a Delete button for any
unlocked, non-base role.

## Open questions

Guild roster/event visibility scopes remain an area of ongoing design beyond the
`public`/`view`/`view_details` model above. Guild ownership and leadership transfer
mechanics beyond the current owner-transfer flow are tracked as open questions in
[Proposal §32](../../../stakeholders/Proposal.md#32-open-questions).
</content>
</invoke>
