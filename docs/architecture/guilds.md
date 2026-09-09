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
| Guild metadata (motd, banner, links, recruiting) | Network | owner/`manage_guild`-editable via `PATCH /guilds/{id}`; `recruiting` feeds the discovery board |
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

**[#160](https://github.com/LunarVagabond/avalon-protocol/issues/160) (decided)
reshapes this going forward**: a guild's connection to a game is never
something a manager declares for a game none of their members have actually
played — it can only ever come from what a game itself already knows via
[game bindings](./game-bindings.md) (#83, shipped). This is a **read/display
feature**, not new durable guild structure: a role with sufficient authority
(`manage_guild` or an equivalent permission) can see an aggregated,
derived breakdown of which games guildmates play or have played, with a
count/fraction of members per game — computed from binding data, not a
`guild.game_associated` protocol event, same "hot state, not history"
treatment already given to presence and chat. That role chooses whether to
show or hide this breakdown on the guild's public profile, and can pin up to
5 games as the guild's curated "favorites" for display — pinning requires
the underlying binding-derived association to already exist; it's never a
way to manufacture one. No minimum-member threshold is needed since nothing
is being gated on/off automatically — it's a display choice, not a system
verdict. Implementation tracked as
[#206](https://github.com/LunarVagabond/avalon-protocol/issues/206)
(affinity view) and
[#207](https://github.com/LunarVagabond/avalon-protocol/issues/207)
(favorites pin), both under the Guilds epic (#19); #20's original manual
`associate_game` endpoint predates this decision and is superseded by it
going forward.

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
- **Role descriptions and badges (issue #152).** A guild role now carries a
  `description` (free text, capped at 200 characters) and a `badge` — a
  small, fixed visual identity, not a free-form upload: an icon id from a
  closed milestone-1 enum (`RoleBadgeIcon` — `shield`, `crown`, `star`,
  `sword`, `wrench`, `heart`, `flag`, `bolt`) paired with a color id from a
  closed enum (`RoleBadgeColor` — `gray`, `red`, `orange`, `gold`, `green`,
  `blue`, `purple`), both defined in `crates/protocol/src/guilds.rs` as
  `RoleBadge { icon, color }`. No user-supplied image hosting is in scope
  for milestone 1; the icon+color shape is deliberately open to grow into a
  richer badge system later (more icons/colors, tiers, an uploaded custom
  asset as an additional variant) without a breaking change to callers that
  just want "an icon and a color" out of a role.
  `crates/server/src/guilds.rs`'s `create_role`/`update_role` accept and
  validate both fields — an out-of-range description or an unrecognized
  badge icon/color is a rejected (400) request, not silently dropped or
  coerced to a default, same posture `validate_tag` already takes for a
  guild's own tag. `guild.role_defined`'s payload grew to include
  `description` and `badge`, no new event kind. The starter roles get
  sensible defaults (`owner` → crown/gold, `officer` → shield/blue,
  `member` → star/gray); the migration
  (`crates/server/db/migrations/0019_guild_role_badges`) backfills existing
  rows with those same column defaults rather than leaving them null, so no
  reader needs a null-handling branch. `packages/ui`'s `AvalonRoleBadge`
  component (tracked in #24) is what will eventually render this — that
  component itself isn't built by #152, which is backend-only.
- Guild creation, rename/retag/redescribe, roles, ownership transfer, and
  membership lifecycle are real and served by `avalon-server`
  (`crates/server/src/guilds.rs`, issues #20 and #21): `POST /guilds`,
  `GET /guilds/{id}`, `PATCH /guilds/{id}`, `GET /guilds/{id}/roles`,
  `POST /guilds/{id}/roles`, `PATCH /guilds/{id}/roles/{idx}`,
  `POST /guilds/{id}/transfer-ownership`, `POST /guilds/{id}/games/{game_id}`,
  `GET /guilds/{id}/game-breakdown` (issue #206, see below),
  `GET`/`PUT /guilds/{id}/favorite-games` (issue #207, see below),
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
  `0009_guild_membership`, `0019_guild_role_badges`). Invites, declines, and withdrawals are
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
- **Roster visibility today, and the #87 gap (issue #57).** #57 asked for
  roster/chat UI against #87's public/members-only/hidden-roster visibility
  scopes, but #87 is still an open, undecided
  [decision](https://github.com/LunarVagabond/avalon-protocol/issues/87) —
  nothing in this pass invents fine-grained visibility that doesn't exist
  server-side. The concrete rule that does exist today and is what the UI
  actually enforces: `GET /guilds/{id}/members`, channels, and messages all
  require current guild membership (session-authenticated, checked against
  `guild_members`), so a member sees the roster and chat and a non-member's
  request is rejected outright — there is no separate "public" or "hidden"
  roster mode to render differently. When #87 lands, the Hub renders
  whatever additional scopes it defines; until then this is the whole
  story, stated here rather than simulated in the UI.
- **"Members currently playing" (#57).** `apps/hub/src/api/guilds.ts`'s
  `groupMembersPlayingByGame`/`formatPlayingSummary` group the roster's
  merged presence by `playing` game id and render "N members playing X" —
  the #74-safe phrasing, never "Game X's guild" — in `Guild.vue`'s
  "Currently playing" card, labeled as live presence, never a durable
  stat. Same honest-empty-state posture as `Friends.vue`'s own
  "Playing &lt;game&gt;" gap: `PresenceResponse.playing` is always null in
  practice today (no game has a live presence-publish binding yet), so
  this card renders "No members currently reporting an in-game presence"
  in every real guild right now — the grouping/formatting logic itself is
  real and tested, and needs no further wiring once a game actually
  publishes `playing`.
- **Guild MOTD and metadata (issue #153).** `Guild` (`crates/protocol/src/guilds.rs`)
  carries `motd` (capped prose, `None` means unset), `banner` (an `http`/
  `https` URL, same validation as a profile's `avatar_url`), `links` (an
  ordered, capped list of `{ label, url }` pairs, stored as one JSONB
  column — full replace on update, not a per-entry patch), and `recruiting`
  (a plain boolean, defaulting to `false`). All four are owner/
  `manage_guild`-editable via `PATCH /guilds/{id}`
  (`crates/server/db/migrations/0020_guild_metadata`) and returned by
  `GET /guilds/{id}` alongside the fields #20 already made public. None of
  this widens what's public — name/tag/description/member_count were
  already "readable by any authenticated identity" per #20; this is more of
  the same kind of field.
- **Guild discovery board (issue #154).** `GET /guilds/discover?q=&recruiting=&tag=&game=&sort=&limit=&cursor=`
  (`crates/server/src/guilds.rs::discover_guilds`) is a paged, filterable,
  session-authenticated browse over the same already-public guild metadata
  — no membership requirement, and no new visibility tier: it's a
  browsable surface over data #20/#153 already made public, not a
  privacy boundary of its own. **Milestone-1 stand-in**, explicitly: this
  is a direct query against the `guilds`/`guild_members`/
  `guild_game_associations` projections in `server`, not #42's real indexer
  read model — the same pragmatic call #44 documents for reads generally.
  When #42's guild read models land, this endpoint's implementation should
  move into `crates/indexer`, unchanged at the HTTP surface.
  - **Filters**: `q=` does a case-insensitive substring match across
    `name`/`tag`/`description`; `tag=` is an exact case-insensitive match
    (indexed via `crates/server/db/migrations/0021_guild_discovery_index`'s
    `guilds_tag_lower_idx`, alongside #153's existing partial
    `guilds_recruiting_idx`); `game=` filters to guilds with a matching row
    in `guild_game_associations` (#20's `associate_game` — no new
    game-association logic invented here).
  - **`recruiting` visibility rule**: a non-recruiting guild must never
    appear in a *stranger's* browse/search results, in any filter
    combination — only exact id/tag lookup (`GET /guilds/{id}`, unchanged
    from #20) reaches it. `recruiting=true` is a plain exact filter (no
    membership gate needed — recruiting guilds are already public-by-design
    per #20). Omitting it falls back to "recruiting guilds, plus any guild
    the caller already belongs to." `recruiting=false` explicitly is
    **still membership-gated**, not a raw exact filter — it returns only
    the caller's own non-recruiting guilds; without that gate a stranger
    could pass `recruiting=false` to bulk-enumerate every non-recruiting
    guild's public metadata, which is exactly what this endpoint must not
    allow.
  - **Sort**: `newest` (default, `created_at DESC`), `alphabetical`
    (`name ASC`), `most_members` (a `COUNT(*)` over `guild_members`,
    `DESC`) — no "trending"/engagement ranking in milestone 1, deliberately
    (that's the kind of derived stat #96's minimum-cohort-size thinking
    would need to apply to first).
  - **Pagination**: cursor-based. `cursor=` is the last guild id from the
    previous page (not an opaque blob) — the server re-resolves that row's
    own sort key via a subquery keyed on the id and does a keyset
    `(sort_key, id) < (...)` comparison, the same pattern
    `guild_messages::list_messages`'s `before=` (#22) already established,
    just under the field name this ticket's own endpoint spec uses. This is
    the first cursor-paginated endpoint to use `cursor=` as the field name
    specifically; #22's message pagination coordinated on `before=` instead
    — both are the same underlying "id of the last-seen row" shape, so a
    future pagination helper can treat them identically regardless of
    field name.
  - Hub: a "Discover" tab on `/guilds` (`apps/hub/src/views/Guilds.vue`,
    `apps/hub/src/composables/useDiscoverGuilds.ts`) — search box,
    recruiting-only toggle (on by default), tag filter, and a "Load more"
    button walking `next_cursor`. `AvalonGuildCard` (#24) grew an optional
    `recruiting` prop to render a "Recruiting" pill, rather than a new
    duplicate card component.
- **Guild game affinity view (issue #206, implementing decision #160).**
  `GET /guilds/{id}/game-breakdown` (`crates/server/src/guilds.rs::game_breakdown`)
  returns, for a guild, how many current members hold an active
  [`GameBinding`](./game-bindings.md) (#83) to each game they play —
  computed on every read from `guild_members` JOIN `bindings`
  (`ended_at IS NULL`) JOIN `games`, grouped by game. Same milestone-1
  direct-query stand-in #154's discovery board already established
  (`build_game_breakdown_query`, split out and unit-tested the same way
  `build_discover_query` is), not #42's real indexer read model. No
  protocol event and no durable table backs the breakdown itself — it's
  derived/computed data, the same "hot state, not history" tier as
  presence (#57) and the discovery board, never touching
  `SettlementProvider`. No minimum-member threshold: every game with at
  least one bound member appears, since this is a display of real counts,
  not a system verdict (#160's rejection of #96-style cohort-size gating
  here). There is no "add" action anywhere in this surface — the only way
  a game appears is a member actually holding an active binding to it,
  which supersedes #20's original manual `POST /guilds/{id}/games/{game_id}`
  (`associate_game`) as the honest source of "what games is this guild
  connected to"; that endpoint still exists unchanged (removing it is out
  of this ticket's scope) but is no longer the intended way to express a
  guild-game connection going forward.
  - **Permission gate.** `can_view_game_breakdown` reuses the existing
    `manage_guild` permission (or guild ownership, via
    `has_guild_permission`'s structural owner check) rather than inventing
    a new one, per #160's decided shape — a `manage_guild` holder can
    always see the breakdown, regardless of the setting below. Anyone else
    (including a non-member) is only let in when the guild has opted into
    public exposure. A rejected caller gets `AppError::MissingGuildPermission`
    (403), the same error every other guild authorization failure in this
    module already returns.
  - **Public-profile exposure toggle.** `guilds.game_breakdown_public`
    (`crates/server/db/migrations/0023_guild_game_breakdown`) is a plain
    boolean column, `false` by default — a real, `manage_guild`-editable
    guild setting alongside `motd`/`banner`/`links`/`recruiting` from
    #153, not a derived fact. Edited via the same `PATCH /guilds/{id}`
    (`UpdateGuildRequest.game_breakdown_public`, three-state-free — just
    omitted-means-untouched, like `recruiting`) and folded into
    `guild.updated`'s existing payload, no new event kind. It controls
    only whether `game_breakdown` lets a non-permitted caller (a
    non-member, or the discovery board from #154) through — it never
    gates the `manage_guild` role's own internal view, which is the
    authority deciding whether to expose the breakdown, not something to
    be gated from seeing it.
  - **Response shape** (`GameBreakdownResponse`): `guild_id`,
    `total_members` (the guild's current membership — the denominator for
    "N of M members play X"; not the same as summing every entry's
    `member_count`, since a member can hold zero, one, or several active
    bindings), and `breakdown: GameBreakdownEntry[]` (`game_id`,
    `game_slug`, `game_name`, `member_count`), ordered by `member_count`
    descending. This is deliberately a clean, queryable shape for #207
    (favorites pin, see below) to build on.
  - Hub: `Guild.vue`'s "Game affinity" card renders each entry via
    `apps/hub/src/api/guilds.ts::formatGameBreakdownEntry` ("N of M
    members play X", the exact phrasing this ticket's design calls for)
    and shows the public-exposure toggle to a `manage_guild` holder. The
    breakdown is fetched independently of the rest of the guild page
    (`useGuildDetail.ts`'s `gameBreakdown`/`gameBreakdownError`) since a
    403 here — not permitted, and the guild hasn't made it public — is an
    expected, common outcome for a non-member, not a page-level error like
    the rest of the guild fetch.
- **Guild favorite games: curated top-5 pin list (issue #207, implementing
  decision #160).** Layered directly on #206's affinity breakdown above: a
  `manage_guild` holder may pin up to 5 games, in order, as the guild's
  curated "favorites" for public display — but only games that already
  show up in the breakdown (at least one currently-actively-bound member).
  There is no way to pin a game the guild has no real, live connection to;
  the same invariant #206/#160 already established for the breakdown
  itself now also holds for this curated subset of it.
  - **Storage.** `guild_favorite_games` (`(guild_id, game_id, position)`,
    `crates/server/db/migrations/0025_guild_favorite_games`) — a small
    table, not a capped JSONB/array column like #153's `guilds.links`,
    because a pin's validity depends on live data in another table
    (`bindings`, via `guild_members`), not just static per-entry
    validation, and each pin needs its own stable position for reordering.
    Postgres enforces "no duplicate pin per game" (composite primary key)
    and "distinct positions per guild" (a unique index on
    `(guild_id, position)`) structurally; the 5-entry cap and the
    live-affinity check are application-level
    (`crates/server/src/guilds.rs::MAX_GUILD_FAVORITE_GAMES`,
    `validate_favorite_game_ids`), same "caps live in code, not the
    schema" posture #153/#206 already document. No durable history table
    beyond the outbox event on each write — like
    `guild_game_breakdown_public` before it, this is current-state-only.
  - **Validation reuses #206's own query.**
    `guilds::guild_bound_game_ids` calls the exact same
    `build_game_breakdown_query` #206's `game_breakdown` endpoint queries,
    collecting the set of game ids with at least one actively-bound
    member. A pin attempt for any other game id is rejected with
    `AppError::FavoriteGameNotBound` (403) — checked against this live
    query at write time, never a cached/stale value, so "pinnable" can
    never drift from "what the breakdown itself would show".
  - **Endpoints and permission gate.** `PUT /guilds/{id}/favorite-games`
    (`SetFavoriteGamesRequest { game_ids: Vec<Uuid> }`) always sends the
    full desired ordered list — same "resend the whole list, not a
    per-entry patch" convention #153's `links` established — and is gated
    by `has_guild_permission(..., GuildPermission::ManageGuild)`, the exact
    same check `update_guild`/#206's breakdown-visibility toggle already
    use, not a new one. Rejects more than
    `MAX_GUILD_FAVORITE_GAMES` (5) entries
    (`AppError::TooManyFavoriteGames`), a duplicate game id in the same
    request (`AppError::DuplicateFavoriteGame`), or any id failing the
    live-affinity check above. On success it replaces the stored rows
    (delete + reinsert under one transaction) and records a
    `guild.favorite_games_updated` outbox event, matching every other
    guild mutation in this module. `GET /guilds/{id}/favorite-games`
    returns the same shape read-only, gated only by session
    authentication (no `manage_guild` requirement) — see below for why.
  - **Staleness, not silent removal.** If a pinned game's last bound
    member later unbinds, the pin is *not* auto-removed — per #207's
    design, that would churn the guild's public display on a single
    member's binding change. Instead, every read
    (`guilds::fetch_favorite_games`) recomputes `stale: bool` per entry
    against the same live `guild_bound_game_ids` set, so a `manage_guild`
    holder sees exactly which pins no longer reflect a real binding and
    can choose to unpin them; a non-manager viewing the public profile
    still sees the pin (a guild's curated choice stays visible until the
    guild itself changes it) with the same `stale` flag available to any
    client that wants to render it differently.
  - **Public display.** Unlike the full breakdown (gated behind
    `game_breakdown_public`), the favorites list is always part of a
    guild's public profile: `GuildResponse.favorite_games` (populated by
    `guild_response`, no extra gate) is returned from
    both `GET /guilds/{id}` and, therefore, anywhere that endpoint's
    response already reaches (the guild's own profile page today; #154's
    discovery board list endpoint is a separate summary shape and doesn't
    embed favorites, to avoid an N+1 query per browsed guild — the
    dedicated `GET /guilds/{id}/favorite-games` endpoint or the profile
    fetch are the intended read paths). This is the guild's own
    deliberate curation choice — the same "always public" treatment
    `motd`/`banner`/`links` already get — distinct from the raw breakdown,
    which a guild may have reasons to keep internal.
  - Hub: `Guild.vue`'s "Favorite games" card (below "Game affinity") shows
    the pinned list (with staleness rendered inline via
    `apps/hub/src/api/guilds.ts::formatFavoriteGameEntry`) to anyone once
    there's something to show, and adds pin/unpin/reorder controls for a
    `manage_guild` holder. Pin candidates are drawn only from
    `gameBreakdown.value.breakdown` (`pinnableBreakdownEntries`) — since
    viewing the full breakdown is itself `manage_guild`-gated, there is no
    UI path to even attempting a pin without real affinity. All four
    mutations (`addFavoriteGameId`/`removeFavoriteGameId`/
    `reorderFavoriteGameIds`) are pure functions returning the next full
    ordered id list, sent via `api.setFavoriteGames` (`PUT`), then the
    page `refresh()`s so `guild.value.favorite_games` picks up the result.
- **Guild history section (#57).** `Guild.vue` has a "History" card, but it
  states plainly that history isn't available yet rather than fabricating
  a feed from the current roster/role snapshot — there is no
  `GET /guilds/{id}/history` endpoint and no indexer projection over
  `guild.*` events exposed to any client today (#82 tracks the event
  catalogue/indexer work this would need). The events themselves are
  durable (see "History vs current state" above); only a read path for
  them is missing.

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
  (SDK), [#24](https://github.com/LunarVagabond/avalon-protocol/issues/24) (Hub,
  done), [#57](https://github.com/LunarVagabond/avalon-protocol/issues/57)
  (roster/chat completion against #87's currently-real visibility rule, the
  "members currently playing" summary, and the honest guild-history gap —
  #57's proposed `AvalonRosterRow`/`AvalonMessageList`/`AvalonMessageComposer`
  component names describe what #24 already built as `AvalonGuildMemberRow`/
  `AvalonChatMessage`/`AvalonChatComposer`; not duplicated under new names).
- [#152](https://github.com/LunarVagabond/avalon-protocol/issues/152) — role
  descriptions and badges, done.
- [#153](https://github.com/LunarVagabond/avalon-protocol/issues/153) — guild
  MOTD/banner/links/recruiting metadata, done.
- [#154](https://github.com/LunarVagabond/avalon-protocol/issues/154) — guild
  discovery board (browse + search recruiting guilds), done as a milestone-1
  `server`-side stand-in pending [#42](https://github.com/LunarVagabond/avalon-protocol/issues/42)'s
  real indexer read model.
- [#160](https://github.com/LunarVagabond/avalon-protocol/issues/160) — decided:
  guild-game association is derived from real member bindings, never
  manager-declared; superseded #20's `associate_game`. Implemented by
  [#206](https://github.com/LunarVagabond/avalon-protocol/issues/206) (game
  affinity breakdown, done) and
  [#207](https://github.com/LunarVagabond/avalon-protocol/issues/207)
  (favorites pin, done — a curated top-5 subset of #206's breakdown, gated
  the same way and validated against the same live data, never a way to
  manufacture an association #206 wouldn't itself show).
- [#87](https://github.com/LunarVagabond/avalon-protocol/issues/87) — visibility
  scopes, including roster visibility.
- Open questions from [Proposal §32](../stakeholders/Proposal.md#32-open-questions): guild
  ownership, leadership transfer.
