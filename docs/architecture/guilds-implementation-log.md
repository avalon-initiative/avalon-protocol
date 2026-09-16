# Guilds: Implementation Log

Detailed, per-feature build notes for [`./guilds.md`](./guilds.md) — what
shipped, which issue it came from, and how it actually works in the code
today. Split out into its own file so `guilds.md` itself can stay focused
on the conceptual model; this is the file to open when you need the real
detail behind one line of that doc's own summary.

Same discipline as everywhere else in `docs/architecture/`: if this doc and
the actual code ever disagree, the code is right and this doc is stale.

- `crates/protocol/src/guilds.rs` — `Guild` (now carrying `join_policy`),
  `JoinPolicy` (`InviteOnly` | `Open`, issue #21), `GuildRole`, `GuildMember`,
  `GuildIntegratorAssociation`, `GuildChannel` (now carrying `announcement_only`,
  issue #250), `GuildMessage`, plus `GuildPermission` (issue #20): the
  fixed milestone-1 vocabulary a role's base permission list draws from —
  `manage_guild`, `manage_roles`, `manage_members`, `manage_channels`,
  `event_manage`, `channel_post`. Still a small, closed vocabulary (no
  custom permission names), but no longer a *flat, guild-wide-only* one:
  issue #250 (shape decided by #243) adds a per-resource override layer on
  top, described below.
- **Per-resource permission overrides (issue #250, decided by #243).** A
  role's `permissions` list (its base grants, unchanged from #20) still
  applies guild-wide by default. On top of that, `GuildPermissionOverride`
  (`crates/protocol/src/guilds.rs`) rows — stored in
  `guild_permission_overrides`
  (`crates/server/db/migrations/0033_guild_permission_overrides`) — let a
  `manage_roles` holder grant or deny one `GuildPermission` to one role,
  scoped to a single channel or event (`GuildResourceKind::Channel` /
  `Event`). Resolution (`crates/server/src/guilds.rs::resolve_resource_permission`,
  driven by `has_resource_permission`): the guild owner's structural
  bypass is untouched by any override; for anyone else, an override for
  the exact (role, resource, permission) triple — when one exists —
  decides the outcome outright (an explicit deny beats a base grant, an
  explicit grant beats a base absence); with no override, the role's flat
  base list is the answer, exactly as before #250. An override pointing
  at a since-deleted channel/event is inert, not an error — every
  endpoint that consults overrides already fetches (and 404s on) the
  resource first, so a dangling override is simply never reached.
  `GET`/`PUT /guilds/{id}/permission-overrides` and
  `DELETE /guilds/{id}/permission-overrides/{override_id}` (all
  `manage_roles`-gated) are the CRUD surface; Hub's Channels tab
  (`ChannelPermissionOverrides.vue`) uses them to expose per-role
  overrides for the active channel. The announcement-only toggle used to
  live in that same component (a strange home for a simple per-channel
  setting); issue #276 moved it to sit with the channel's own
  name/topic in the channel header instead, leaving
  `ChannelPermissionOverrides.vue` focused purely on the role x
  permission grid. `manage_guild`/`manage_roles`/`manage_members` stay guild-wide
  only — they have no per-instance resource to scope to — so every
  endpoint gated on one of those three keeps using the original flat
  `has_guild_permission` check; only channel/event endpoints with an
  obvious resource moved to the resource-aware check
  (`crate::channels::require_manage_channel_resource`,
  `crate::guild_events::require_manage_event_resource`), and creation
  endpoints (no resource id yet at that point) still use a flat,
  guild-wide check.
- **`event_manage` (issue #250).** Previously, guild-event
  create/update/delete piggybacked on `manage_channels` (#169's
  workaround, since #20's `GuildPermission` set wasn't meant to be
  extended casually). #250 gives events their own permission instead:
  `create_event` checks it guild-wide (no event exists yet to scope a
  resource-aware check to); `update_event`/`delete_event` check it
  resource-aware against the specific event, so a role can be granted (or
  denied) management of one event via an override, on top of or instead
  of holding `event_manage` guild-wide. The migration backfills
  `event_manage` onto every existing role that already held
  `manage_channels`, so authority over events doesn't silently regress
  for guilds created before this ticket.
- **Announcement-only channels (issue #250) — the ticket's
  channel-organization proof point.** `GuildChannel.announcement_only`
  (`guild_channels.announcement_only`, default `false`) is a per-channel
  flag: when set, `POST .../channels/{cid}/messages` additionally
  requires the `channel_post` permission for that specific channel
  (resolved through the override layer above), instead of today's "any
  current guild member may post." A regular channel keeps that original
  behavior unchanged — this is strictly additive per-channel, not a
  change to the guild-wide permission model. No role holds `channel_post`
  in its base list by default; a guild opts a role into posting in a
  specific announcement-only channel by granting it a `channel_post`
  override on that channel (or, if it ever wants a role to post
  everywhere, by adding `channel_post` to that role's base list on the
  Roles tab instead).
- **Channel topics (issue #276).** `GuildChannel.topic`
  (`guild_channels.topic`, default `NULL`) is a short, capped (200
  characters, `crate::channels::CHANNEL_TOPIC_MAX_CHARS`) line describing
  what a channel is for — the Discord/Slack/Zoom-style affordance the
  guild page's channel header was missing. Same "no value"/normalize-blank
  convention as `Guild::motd`: `None`/unset means no topic, an empty
  string is never stored. Editable via `PATCH .../channels/{cid}`
  alongside a rename, gated the same `manage_channels` (resource-aware)
  check as everything else in `crates/server/src/channels.rs`. Also the
  occasion for moving the guild's MOTD (issue #153) out of the Overview
  tab's About card and into its own persistent banner above the tab bar
  (`Guild.vue`) — an MOTD nobody navigates to see wasn't actually
  functioning as one.
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
  `POST /guilds/{id}/transfer-ownership`, `POST /guilds/{id}/integrations/{integrator_id}`,
  `GET /guilds/{id}/integrator-breakdown` (issue #206, see below),
  `GET`/`PUT /guilds/{id}/favorite-integrators` (issue #207, see below),
  `POST /guilds/{id}/invites`, `POST /guilds/{id}/invites/{invite_id}/accept`,
  `POST /guilds/{id}/invites/{invite_id}/decline`, `POST /guilds/{id}/join`
  (open guilds only), `POST /guilds/{id}/leave`,
  `DELETE /guilds/{id}/members/{identity_id}`,
  `PATCH /guilds/{id}/members/{identity_id}`, `GET /guilds/{id}/members`, and
  `GET /me/guilds`. Every route is session-authenticated only, same as
  `friends.rs`. `guild.created`, `guild.updated`, `guild.role_defined`,
  `guild.owner_transferred`, `guild.member_added`, `guild.member_removed`,
  and `guild.role_changed` are written into the outbox in the same
  transaction as the `guilds`/`guild_roles`/`guild_integrator_associations`/
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
  per channel (`GUILD_CHANNEL_MESSAGE_CAP` env var, default 10,000); once a
  channel exceeds it, the oldest messages move into a `guild_messages_archive`
  table instead of being deleted outright (issue #253, implementing #193's
  decision). The archive is itself held only for a long, separately
  configurable window (`GUILD_MESSAGE_ARCHIVE_RETENTION_DAYS`, default 730
  days / ~2 years), after which a background worker hard-deletes whatever
  falls past it — that expiry is a genuine, final delete, nothing
  recoverable afterward. This does **not** change the #74/#75 classification:
  the archive is still operational/server-policy state, not durable protocol
  history — no `guild.message_*` ledger event exists or is planned, and
  neither tier ever touches the outbox or `SettlementProvider`. No client
  should assume guild chat history is permanent, in the live table or the
  archive. `GET .../channels/{cid}/messages/archive` reads the archive
  (`crates/server/src/guild_messages.rs`'s `list_archive`), same
  newest-first, cursor-paginated shape as the live endpoint. Sending/reading
  requires current guild membership (via #21's `guild_members` table, now
  merged alongside #22); the archive read endpoint uses the same
  *current*-membership check as the live channel, not membership as of when
  each archived message was originally sent — `guild_members` is a live
  projection with no point-in-time history of its own, and reconstructing
  one would mean building real historical-membership machinery for a tier
  that is explicitly not protocol history. Moderators (`manage_channels`)
  can hard-delete a message outright, since there's no history to preserve;
  `delete_message` checks both `guild_messages` and `guild_messages_archive`
  for the target id, so a moderator's takedown for cause isn't defeated just
  because cap-based pruning already archived the row first — moderation
  intent wins regardless of which tier currently holds it.
- The Rust SDK's guild surface (issue #23) is real:
  `crates/sdk/src/guilds.rs`'s `Session::guilds()` (`guilds.read`) lists the
  caller's own memberships, `Session::guild(id).roster()` (`guilds.read`)
  and `.channels()`/`.channel(cid).messages()`/`.send()` (`guilds.chat`)
  read the roster and chat and post as the identity — never as the integrator.
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
  archive channels, associate an integrator) is gated client-side on the caller's
  own resolved permission list, but the server is the real authority — a
  hidden-but-still-reachable action shows a plain error on a 403 rather
  than crashing. `apps/mobile-hub` isn't wired to guilds yet (still #60).
- **Guild page navigation: tabs, and chat folded into the page (issue
  #241).** `/guilds/:id` (`apps/hub/src/views/Guild.vue`) was one long
  scrolling page; it's now Overview/Members/Channels/Events/Roles/Settings
  tabs, plain client-side state (`activeTab` ref) using the same
  `local.tabs`/`local.tab`/`local.tabActive` CSS-module pattern
  `Guilds.vue`'s "My guilds"/"Discover" tabs already established — no
  routing involved for switching between them. Overview carries header
  info, MOTD/banner/links (read-only), integrator affinity, favorite integrators, and
  associated integrators (now visible to any member, read-only — previously the
  whole "Associated integrators" card was hidden from non-managers, not just its
  "associate an integrator" action); Members carries the roster, currently-playing
  summary, and invites; Roles and Events are unchanged content, just
  relocated; Settings carries the recruiting toggle, MOTD/banner/link
  editing, and ownership transfer, all still `canManageGuild`-gated exactly
  as before. `motd`/`banner`/`links`/`recruiting` (issue #153) previously
  had no Hub UI at all despite being live on `PATCH /guilds/{id}` since
  #153 landed — `apps/hub/src/api/types.ts`'s `GuildResponse`/
  `UpdateGuildRequest` grew those fields and the Settings tab is their
  first real caller.

  The standalone `GuildChannel.vue` route/view is gone. `apps/hub/src/composables/useGuildChat.ts`
  is now driven from inside the Channels tab instead: a persistent
  `AvalonChannelList` sidebar next to the active channel's messages, and
  picking a different channel just reassigns the `channelId` ref
  `useGuildChat` already watches (`watch([guildId, channelId], load)`) —
  no route navigation and no component remount per channel switch, which
  is what made guild → channel → back to guild "messy" before. The
  composable itself grew guards for an empty `channelId` (no channel
  selected/available yet is a quiet state, not an error) since it's now
  mounted for as long as the guild page is, not just while viewing one
  channel. `/guilds/:id/channels/:cid` still resolves — to the same
  `Guild.vue` component, opening the Channels tab with that channel
  pre-selected — so existing deep links keep working; selecting a channel
  from the sidebar updates the URL via `router.replace` (no history entry
  per switch) so a channel stays link-shareable, while switching to any
  other tab, or away from Channels, is plain in-memory state with no URL
  effect.
  **Correction (2026-09-08, issue #24):** building this surfaced that
  `crates/server/src/channels.rs`'s `manage_channels` check was still
  calling a leftover stub (always-empty permissions) from when #22 was
  written concurrently with #21, rather than #21's real
  `guild_members`/`guild_roles` lookup — fixed alongside landing #24 by
  having `channels.rs` reuse `guilds::actor_role_permissions` directly
  instead of its own duplicate. Two real gaps remain, not worked around
  with a Hub-only endpoint: there is no endpoint that lists an identity's own
  pending guild invites — `POST /guilds/{id}/invites` returns an invite id,
  but nothing resolves "invites addressed to me" the way
  `GET /friends/requests` does for friend requests, so an invited identity
  has no way to discover or accept an invite through the Hub UI today; the
  invite id has to be shared out of band. `join_policy` toggling has since
  shipped (issue #21): `UpdateGuildRequest.join_policy` lets an owner/officer
  flip a guild between `invite_only` and `open`, and `Guild.vue`'s Settings
  tab exposes it as a live toggle (`onToggleJoinPolicy`) — see the open-guild
  joining section below for the current, real behavior.
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
  `groupMembersPlayingByIntegrator`/`formatPlayingSummary` group the roster's
  merged presence by `playing` integrator id and render "N members playing X" —
  the #74-safe phrasing, never "Integrator X's guild" — in `Guild.vue`'s
  "Currently playing" card, labeled as live presence, never a durable
  stat. Same honest-empty-state posture as `Friends.vue`'s own
  "Playing &lt;integrator&gt;" gap: `PresenceResponse.playing` is always null in
  practice today (no integrator has a live presence-publish binding yet), so
  this card renders "No members currently reporting an in-game presence"
  in every real guild right now — the grouping/formatting logic itself is
  real and tested, and needs no further wiring once an integrator actually
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
- **Guild icon (issue #246).** `Guild.icon` is a second, independent image
  field alongside `banner` — a small badge/identity mark (recruitment
  card, member-list-style avatar, an external integrator's own badge UI)
  rather than `banner`'s wide cover-image role; stretching or cropping one
  into the other's shape doesn't hold up as a substitute, hence a real
  second column (`crates/server/db/migrations/0031_guild_icon`) rather than
  deriving an icon from the banner. Same validation and update convention
  as `banner`: `http`/`https`-only, length-capped
  (`validate_guild_icon`/`MAX_GUILD_ICON_URL_LEN` in
  `crates/server/src/guilds.rs`), three-state `PATCH /guilds/{id}` update
  (omitted untouched, `""` clears, non-empty validates and sets). No
  protocol event of its own — same "operational state, not durable
  history" posture `motd`/`banner`/`links` already have. Hub surfaces it in
  the guild page header as a small badge, on the Settings tab's profile
  card alongside MOTD/banner, and on `AvalonGuildCard` (`packages/ui`) as
  optional `iconUrl`/`bannerUrl` props — wired for both the "My guilds"
  list (`GuildResponse`-backed) and the Discover tab (issue #258:
  `DiscoverGuildSummary` now carries `banner`/`icon` too, and
  `build_discover_query`'s `SELECT` includes `g.banner`/`g.icon`). No new
  visibility exposure — both were already public via `GET /guilds/{id}`;
  this only widens the browse listing to include them, same as the rest of
  the discovery board's "browsable surface over already-public data"
  posture.
- **Guild discovery board (issue #154).** `GET /guilds/discover?q=&recruiting=&tag=&integrator=&sort=&limit=&cursor=`
  (`crates/server/src/guilds.rs::discover_guilds`) is a paged, filterable,
  session-authenticated browse over the same already-public guild metadata
  — no membership requirement, and no new visibility tier: it's a
  browsable surface over data #20/#153 already made public, not a
  privacy boundary of its own. **Milestone-1 stand-in**, explicitly: this
  is a direct query against the `guilds`/`guild_members`/
  `guild_integrator_associations` tables in `server`, not routed through
  `crates/indexer`'s `guild_rosters` projection (#42, closed) — the same
  pragmatic call #506 (open: retarget `friends.rs`/`guilds.rs`/
  `connections.rs` reads onto the indexer) documents for reads generally.
  When #506 lands, this endpoint's implementation should move onto the
  indexer, unchanged at the HTTP surface.
  - **Filters**: `q=` does a case-insensitive substring match across
    `name`/`tag`/`description`; `tag=` is an exact case-insensitive match
    (indexed via `crates/server/db/migrations/0021_guild_discovery_index`'s
    `guilds_tag_lower_idx`, alongside #153's existing partial
    `guilds_recruiting_idx`); `integrator=` filters to guilds with a matching row
    in `guild_integrator_associations` (#20's `associate_integrator` — no new
    integrator-association logic invented here).
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
- **Guild integrator affinity view (issue #206, implementing decision #160).**
  `GET /guilds/{id}/integrator-breakdown` (`crates/server/src/guilds.rs::game_breakdown`)
  returns, for a guild, how many current members hold an active
  [`IntegratorBinding`](./bindings.md) (#83) to each integrator they play —
  computed on every read from `guild_members` JOIN `bindings`
  (`ended_at IS NULL`) JOIN `integrators`, grouped by integrator. Same milestone-1
  direct-query stand-in #154's discovery board already established
  (`build_game_breakdown_query`, split out and unit-tested the same way
  `build_discover_query` is), not routed through the indexer (#42, closed;
  #506, open, is what would move reads like this onto it). No
  protocol event and no durable table backs the breakdown itself — it's
  derived/computed data, the same "hot state, not history" tier as
  presence (#57) and the discovery board, never touching
  `SettlementProvider`. No minimum-member threshold: every integrator with at
  least one bound member appears, since this is a display of real counts,
  not a system verdict (#160's rejection of #96-style cohort-size gating
  here). There is no "add" action anywhere in this surface — the only way
  an integrator appears is a member actually holding an active binding to it,
  which supersedes #20's original manual `POST /guilds/{id}/integrations/{integrator_id}`
  (`associate_integrator`) as the honest source of "what integrators is this guild
  connected to"; that endpoint still exists unchanged (removing it is out
  of this ticket's scope) but is no longer the intended way to express a
  guild-integrator connection going forward.
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
    bindings), and `breakdown: GameBreakdownEntry[]` (`integrator_id`,
    `integrator_slug`, `integrator_name`, `member_count`), ordered by `member_count`
    descending. This is deliberately a clean, queryable shape for #207
    (favorites pin, see below) to build on.
  - Hub: `Guild.vue`'s "Integrator affinity" card renders each entry via
    `apps/hub/src/api/guilds.ts::formatGameBreakdownEntry` ("N of M
    members play X", the exact phrasing this ticket's design calls for)
    and shows the public-exposure toggle to a `manage_guild` holder. The
    breakdown is fetched independently of the rest of the guild page
    (`useGuildDetail.ts`'s `integratorBreakdown`/`integratorBreakdownError`) since a
    403 here — not permitted, and the guild hasn't made it public — is an
    expected, common outcome for a non-member, not a page-level error like
    the rest of the guild fetch.
- **Guild favorite integrators: curated top-5 pin list (issue #207, implementing
  decision #160).** Layered directly on #206's affinity breakdown above: a
  `manage_guild` holder may pin up to 5 integrators, in order, as the guild's
  curated "favorites" for public display — but only integrators that already
  show up in the breakdown (at least one currently-actively-bound member).
  There is no way to pin an integrator the guild has no real, live connection to;
  the same invariant #206/#160 already established for the breakdown
  itself now also holds for this curated subset of it.
  - **Storage.** `guild_favorite_games` (`(guild_id, integrator_id, position)`,
    `crates/server/db/migrations/0025_guild_favorite_games`) — a small
    table, not a capped JSONB/array column like #153's `guilds.links`,
    because a pin's validity depends on live data in another table
    (`bindings`, via `guild_members`), not just static per-entry
    validation, and each pin needs its own stable position for reordering.
    Postgres enforces "no duplicate pin per integrator" (composite primary key)
    and "distinct positions per guild" (a unique index on
    `(guild_id, position)`) structurally; the 5-entry cap and the
    live-affinity check are application-level
    (`crates/server/src/guilds.rs::MAX_GUILD_FAVORITE_GAMES`,
    `validate_favorite_game_ids`), same "caps live in code, not the
    schema" posture #153/#206 already document. No durable history table
    beyond the outbox event on each write — like
    `guild_game_breakdown_public` before it, this is current-state-only.
  - **Validation reuses #206's own query.**
    `guilds::guild_bound_integrator_ids` calls the exact same
    `build_game_breakdown_query` #206's `game_breakdown` endpoint queries,
    collecting the set of integrator ids with at least one actively-bound
    member. A pin attempt for any other integrator id is rejected with
    `AppError::FavoriteGameNotBound` (403) — checked against this live
    query at write time, never a cached/stale value, so "pinnable" can
    never drift from "what the breakdown itself would show".
  - **Endpoints and permission gate.** `PUT /guilds/{id}/favorite-integrators`
    (`SetFavoriteGamesRequest { integrator_ids: Vec<Uuid> }`) always sends the
    full desired ordered list — same "resend the whole list, not a
    per-entry patch" convention #153's `links` established — and is gated
    by `has_guild_permission(..., GuildPermission::ManageGuild)`, the exact
    same check `update_guild`/#206's breakdown-visibility toggle already
    use, not a new one. Rejects more than
    `MAX_GUILD_FAVORITE_GAMES` (5) entries
    (`AppError::TooManyFavoriteGames`), a duplicate integrator id in the same
    request (`AppError::DuplicateFavoriteGame`), or any id failing the
    live-affinity check above. On success it replaces the stored rows
    (delete + reinsert under one transaction) and records a
    `guild.favorite_games_updated` outbox event, matching every other
    guild mutation in this module. `GET /guilds/{id}/favorite-integrators`
    returns the same shape read-only, gated only by session
    authentication (no `manage_guild` requirement) — see below for why.
  - **Staleness, not silent removal.** If a pinned integrator's last bound
    member later unbinds, the pin is *not* auto-removed — per #207's
    design, that would churn the guild's public display on a single
    member's binding change. Instead, every read
    (`guilds::fetch_favorite_games`) recomputes `stale: bool` per entry
    against the same live `guild_bound_integrator_ids` set, so a `manage_guild`
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
    dedicated `GET /guilds/{id}/favorite-integrators` endpoint or the profile
    fetch are the intended read paths). This is the guild's own
    deliberate curation choice — the same "always public" treatment
    `motd`/`banner`/`links` already get — distinct from the raw breakdown,
    which a guild may have reasons to keep internal.
  - Hub: `Guild.vue`'s "Favorite integrators" card (below "Integrator affinity") shows
    the pinned list (with staleness rendered inline via
    `apps/hub/src/api/guilds.ts::formatFavoriteGameEntry`) to anyone once
    there's something to show, and adds pin/unpin/reorder controls for a
    `manage_guild` holder. Pin candidates are drawn only from
    `integratorBreakdown.value.breakdown` (`pinnableBreakdownEntries`) — since
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
- **Guild events calendar + RSVP (issue #169).** A guild plans things —
  raid nights, tournament prep, meetups — regardless of which integrator (if
  any) members currently have open; a pinned chat message is a poor
  substitute for a real calendar. `GuildEvent`/`GuildEventRsvp`
  (`crates/protocol/src/guilds.rs`) are served by
  `crates/server/src/guild_events.rs`: `GET`/`POST /guilds/{id}/events`,
  `PATCH`/`DELETE /guilds/{id}/events/{eid}`,
  `PUT /guilds/{id}/events/{eid}/rsvp`, migration
  `crates/server/db/migrations/0028_guild_events`. This is distinct from
  #88's integrator event *result* attestations — durable claims issued after the
  fact about outcomes — this is scheduling something upcoming.
  **Durability call, made explicitly rather than assumed:** unlike guild
  *channels* (#22), whose structure — create/rename/archive — is durable
  history behind `guild.channel_*` outbox events, a guild event gets no
  protocol event kind at all, for either the event row or its RSVPs. A
  scheduled event doesn't have the "durable structure" character a channel
  does: a raid night gets rescheduled or cancelled repeatedly, and that
  churn isn't history worth preserving forever any more than the chat that
  happens in a channel is. So the whole feature — `guild_events` and
  `guild_event_rsvps` alike — gets the same "hot state, not history"
  treatment already given to `guild_messages` and presence, rather than
  channels' "structure is durable, content isn't" split. Deleting an event
  is a real hard delete (nothing here claims to be reconstructable
  history) and cascades to its RSVPs via the `guild_event_rsvps` table's
  `ON DELETE CASCADE` foreign key. Creating, rescheduling, and deleting an
  event are gated on `event_manage`, its own `GuildPermission` (issue
  #250) — originally this reused `manage_channels` rather than adding a
  new permission, since #20's set wasn't meant to be extended casually;
  #250 gave it a real permission once the per-resource override layer
  (see "Per-resource permission overrides" above) made a dedicated one
  worth having, with the migration preserving existing roles' authority
  over events in the switch. RSVPing is self-service and idempotent: `PUT .../rsvp`
  always upserts the caller's own `(event_id, identity_id)` row (the
  table's primary key), replacing any prior status rather than
  accumulating rows; a member can never target another member's RSVP.
  Listing and RSVPing both require current guild membership, same as
  channels/messages. The Rust SDK exposes a read-only
  `Session::guild(id).events()` (`guilds.read`), mirroring `channels()`;
  create/update/delete/RSVP stay identity-authority Hub-only actions, same
  posture channel *management* already has. Hub: a new "Events" card on
  `/guilds/:id` lists upcoming events with title/time/RSVP counts and a
  going/maybe/not-going control, using two new `packages/ui` components
  (`AvalonEventCard`, `AvalonRsvpControl`).
- **Per-member RSVP roster (issue #248).** The event list's `rsvp_counts`
  is aggregate-only — no way to see *who* is going/maybe/can't-go, only
  how many. `GET /guilds/{id}/events/{eid}/rsvps`
  (`crates/server/src/guild_events.rs::list_rsvps`) mirrors `rsvp_counts`'s
  own query against `guild_event_rsvps` but returns every row
  (`identity_id`, `status`, `responded_at`) unaggregated instead. Gated
  the same as `list_events`/`rsvp_counts` — current guild membership only,
  no `manage_channels` or any other management permission, since RSVP
  status is ordinary guild-internal social info rather than a moderation
  concern (same posture the member roster already takes). Strictly
  additive: the aggregate `rsvp_counts` on `GET /guilds/{id}/events` is
  unchanged. Hub resolves the returned identity ids to
  `display_name#discriminator` via the existing batched
  `GET /identities/profiles` (issue #161), the same pattern
  `useGuildChat`'s `resolveAuthorNames` and `listMembersWithPresence`
  already use, and groups them into going/maybe/can't-go buckets
  (`apps/hub/src/api/guildEvents.ts::groupRsvpRoster`,
  `apps/hub/src/composables/useRsvpRoster.ts`). One shared
  `packages/ui` component, `AvalonRsvpRosterPanel` (a thin wrapper around
  `AvalonModal`), renders the roster; it's reachable by clicking an event
  card from both the Events tab and the Calendar tab's selected-day list
  in `Guild.vue`, rather than each tab getting its own roster UI.
- **Guild join requests (issue #242).** `guild_join_requests`
  (`crates/server/db/migrations/0030_guild_join_requests`) is the
  applicant-initiated counterpart to `guild_invites` — a stranger applying
  to a `recruiting` guild from #154's Discover board instead of waiting on
  a manager to invite them. Same projection posture as `guild_invites`: a
  pending/approved/rejected/withdrawn transition is not itself durable
  history, only the resulting membership-add on approval is (through the
  same `add_member` helper `accept_invite`/`join_guild` already use, so
  approval can't drift from what an accepted invite or a direct open-guild
  join produce). `POST /guilds/{id}/join-requests` rejects applying to a
  non-recruiting guild (`GuildNotRecruiting`) and to a guild the caller
  already belongs to; a second apply while one is already pending is
  idempotent, returning the existing pending row rather than erroring or
  duplicating it (`guild_join_requests_pending_idx`, a partial unique index
  on `(guild_id, applicant) WHERE status = 'pending'`, same shape
  `guild_invites_pending_idx` uses). `GET /guilds/{id}/join-requests`
  (pending-only by default, `?status=all` for the full history),
  `POST .../join-requests/{id}/approve`, and `POST .../join-requests/{id}/reject`
  are all `manage_members`-gated via the existing `has_guild_permission`
  helper; `DELETE /guilds/{id}/join-requests/{id}` lets only the applicant
  withdraw their own pending request, same "consent from the other side"
  shape `decline_invite` has from the opposite party. Hub: an "Apply to
  join" action on Discover guild cards (`apps/hub/src/views/Guilds.vue`,
  shown only for recruiting guilds the caller isn't already a member of),
  and an "Applications" section on the guild page
  (`apps/hub/src/views/Guild.vue`, `manage_members`-gated) for managers to
  review pending requests.
- **Withdrawing your own join request (issue #256).** `DELETE
  .../join-requests/{id}` existed from #242, but the Hub had no way to
  discover the request's own id to call it with — `GET
  /guilds/{id}/join-requests` is `manage_members`-gated, so an applicant
  couldn't even see their own pending application. `GET
  /guilds/{id}/join-requests/mine` closes that gap: deliberately *not*
  `manage_members`-gated, since it only ever returns the caller's own
  pending request for the guild (or `null` on a 200, not a 404 for "none"),
  the same nullable-on-200 "single resource belonging to the caller, or
  none" convention `GET /me/recovery/status` already established.
  `Guild.vue`'s Membership card now checks it directly: a pending request
  shows its status and a "Withdraw request" button (wired to the
  previously-unused `withdrawJoinRequest` client call) instead of — or
  alongside, for a recruiting invite-only guild with no pending request —
  an "Apply to join" action. Scoped to the single guild's own page; no
  separate cross-guild "my applications" view, per the ticket's scope.
- **Role name uniqueness, open-guild joining, and role deletion.**
  `guild_roles` had no uniqueness constraint on `name` at all — nothing
  stopped a guild from having several roles all named the same thing.
  Fixed with a case-insensitive unique index (`guild_roles_name_lower_idx`,
  `(guild_id, lower(name))`, same `lower()` functional-index approach
  `guilds.name`/`guilds.tag` already use) plus catching the resulting
  unique-violation in `create_role`/`update_role` as `GuildRoleNameTaken`
  (409). Separately, `join_policy` (`invite_only`/`open`, #21) had a
  fully working direct-join path (`POST /guilds/{id}/join`,
  `can_join_directly`) but no way to actually set a guild to `open` —
  `UpdateGuildRequest` never carried the field. Added it end to end
  (validation via `JoinPolicy::parse`, the `UPDATE` statement, the
  outbox event, and a Settings toggle in the Hub). `update_role` also
  no longer blanket-rejects any edit to the owner role (`name_index`
  `0`) — only a `permissions` change on it is rejected (the owner's
  authority comes from `guilds.owner`, not this row, so changing its
  permissions would be a no-op footgun); name/description/badge are
  cosmetic-only and stay editable. `DELETE /guilds/{id}/roles/{idx}`
  (new) rejects the owner and member roles outright (`is_base_role`,
  structural — member is the hardcoded default role every
  `add_member` call assigns) and relies on `guild_members`'s existing
  foreign key into `guild_roles` to reject deleting any other role
  still held by a member, mapped to `RoleHasMembers` (409) rather than
  a raw DB error. Hub: the Roles tab's permission matrix now has a
  pencil icon that unlocks a row for renaming/permission edits (rather
  than every checkbox always being live to click by accident), and a
  Delete button for any unlocked, non-base role.
- **Role-gated `view`/`view_details` permissions (issue #458, implementing
  #454's decided shape).** `GuildPermission` gained two entries
  (`crates/protocol/src/guilds.rs`), resolved through a dedicated function
  (`crates/server/src/guilds.rs::resolve_view_permission`) rather than the
  generic per-resource fallback (`resolve_resource_permission`) every
  other permission uses, since these two have a genuinely different
  baseline: with no override at all, a member gets `view = true,
  view_details = true` (exactly today's pre-#458 behavior, unchanged),
  while a non-member gets both derived from the resource's own `public`
  flag — never a flat `false`. An explicit `view_details` grant always
  implies `view`, even under an explicit `view` deny (`Some(true)` on
  `view_details` short-circuits before `view`'s own override is even
  consulted) — the ticket's own invariant that the two must never be a
  contradictory pair. `has_view_permission` is the override-fetching,
  membership-aware sibling `has_resource_permission` gives
  `resolve_resource_permission`.
  - **Events** (`guild_events::list_events`): a per-event `can_view` check
    filters the list (denied `view` → not returned at all, matching how a
    genuinely-forbidden resource already behaves elsewhere); a per-event
    `view_details` check controls whether `event_response` returns real
    content or a stripped placeholder. `EventResponse` gained
    `details_visible: bool` — `false` means `channel_id`/`description`/
    `rsvp_counts`/`my_rsvp` are zeroed placeholders, not real data, while
    `id`/`guild_id`/`title`/`starts_at`/`ends_at`/`created_by`/
    `created_at`/`public` stay real (existence visible, content isn't).
    Always `true` for create/update/RSVP responses — those all require
    the actor to already hold `event_manage` or be RSVPing to their own
    record.
  - **Channels** (`channels::list_channels`): channels had *no* non-member
    visibility concept at all before this ticket — every read
    unconditionally required membership. A new `guild_channels.public`
    column (migration 0062, defaulted `false` — no behavior change for an
    existing channel until an owner/manager opts it in via `PATCH
    .../channels/{cid}`) plus the same `has_view_permission` filtering
    `list_events` uses extends the exact "public flag widens exposure to
    non-members" shape events already had. Message content
    (`guild_messages::list_messages`/`list_archive`) is gated on
    `view_details` instead of plain membership now — same baseline for an
    existing channel with no overrides, but now also reachable by a
    non-member of a public channel in a public guild, and deniable
    per-role per-channel like everything else in the override layer.
  - **Hub.** `ChannelPermissionOverrides.vue` (channel-only) generalized
    into `ResourcePermissionOverrides.vue` (`resourceKind`/`resourceId`
    props instead of a hardcoded channel), reused on both the Channels tab
    (the active channel) and the Events tab (while editing an existing
    event — there's no resource id yet while creating one). A new
    "Visible to prospective members" checkbox next to the channel's
    existing announcement-only toggle drives `guild_channels.public`, same
    UX the event creation form already had for `GuildEvent.public`.
    `AvalonEventCard` (`packages/ui`) gained a `detailsVisible` prop
    (defaulting `true` via `withDefaults` — Vue casts an *omitted*
    optional `boolean` prop to `false` at runtime, so every existing
    caller needed an explicit default to keep rendering real content);
    `false` renders a "details hidden" hint instead of the (placeholder)
    description/RSVP summary, and both the Events tab and Calendar tab
    stop offering RSVP/roster-click affordances for a stripped event.
