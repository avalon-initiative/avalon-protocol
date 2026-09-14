# Hub: Implementation Log

Full build-by-build detail behind [`./hub.md`](./hub.md) — which views exist,
what each one actually renders, and where its data comes from. Split into its
own file so `hub.md` itself can stay focused on the Hub's role in the wider
network; this is the file to open when you need the real detail behind one
line of that doc's own summary.

Same discipline as everywhere else in `docs/architecture/`: if this doc and
the actual code ever disagree, the code is right and this doc is stale.

- `apps/hub/` — Vue 3 + Vite + TypeScript, routed with `vue-router`. Identity
  creation (`CreateIdentity.vue`) and login (`Login.vue`) are their own
  pre-authenticated pages — see [identity](./identity.md)'s "Today in the
  repo" for the crypto/API layer underneath registration/login (#55). All
  server traffic goes through `apps/hub/src/api/`; nothing calls `fetch`
  directly outside it.
- **The logged-in Hub is a persistent shell, not separate pages.**
  `HubShell.vue` renders a fixed left sidebar on desktop (wordmark,
  `AvalonSidebarNav`, the caller's own presence at the bottom) that
  collapses to an `AvalonBottomNav` bar on mobile (≤768px), plus a top
  header (a disabled search field — identity discovery is #129, undecided — a
  notifications placeholder, and an `AvalonUserChip` with the caller's real
  avatar/name/handle linking to Profile), with `<RouterView />` swapping
  the page in place. Pages are nested child routes under a shared parent
  (`/home`, `/profile`, `/friends`, `/activity` in
  `apps/hub/src/router/index.ts`) — URL-addressable and bookmarkable;
  `requiresAuth` is set once on the parent route and inherited by every
  child via `vue-router`'s meta-merging. Nav entries for features that
  don't exist yet (Discover) render disabled with a
  "Soon" tag rather than being hidden. Integrators (#270), Chat (#105), and
  Achievements (#35) are no longer among them, same as Guilds (#24) before
  them. The shell also heartbeats `PUT /me/presence` (Online, every 60s,
  inside the server's 120s TTL) so the user actually reads as online to
  their friends while the Hub is open. Visual language: one dark theme via
  CSS custom properties in `packages/ui/src/styles/tokens.css` (+
  `global.css`), imported once in `apps/hub/src/main.ts`; component styles
  reference tokens only.
- **`Achievements.vue` (#35, landed)** — reads `GET /me/achievements` (#34)
  and merges in two things that endpoint doesn't itself carry: the
  achievement/milestone's display name (`GET /integrations/{slug}/achievements` or
  `GET /integrations/{slug}/milestones`, keyed by the claim's own GlobalId
  ref) and the issuer's display name (`GET /integrations/{slug}`) — see
  `apps/hub/src/api/achievements.ts`'s module doc comment. Each claim
  renders via a new `packages/ui` component, `AvalonAchievementCard`:
  achievement name, a clickable issuer chip (routes to the integrator's profile
  page, #90), issued date, a `valid`/`invalid` badge with its reason
  (never a score or star rating — ADR #77), and an expandable history list
  that shows a revocation's date and reason alongside the original
  issuance (#81/#85) rather than replacing it. Filter (by achievement/integrator
  name, and by a specific issuer via a dropdown scoped to issuers the
  caller actually has claims from) and sort (date/name/integrator) are both
  client-side and unit-tested (`apps/hub/src/api/achievements.test.ts`).
  **Not built in this pass**: feature/hide (blocked on #87's visibility
  store — no protocol event or preference row exists yet to persist
  either), and `PUT /integrations/{slug}/recognition`-backed recognition display
  (trust-model.md's own tracked gap — this view shows authenticity/validity
  only, per ADR #76).
- `Home.vue` (#148, all six mock sections landed for #312) — the landing page
  after login: welcome header, "Recently Connected" (the most recently
  connected app/integrator — `IntegratorBindingResponse` has no last-played/session data,
  so that's the only honest ordering available, not a curated pick), "Your
  Apps & Integrators" (a name/icon grid of every connected binding), Quick Actions
  (add a friend, set up this device, edit profile), Friends Online, Guilds (a
  compact panel of the caller's guilds with member counts), Latest Messages
  (each guild's most recent message from its first non-archived channel —
  not a full cross-channel merge, which `Guild.vue`'s own channel list
  already covers), and the six most recent entries from `GET /me/history`
  with a "View all" link to `/activity`. All four of the newer sections
  reuse `apps/hub/src/api/integrations.ts`/`guilds.ts`/`guildChat.ts` as they
  already existed for `IntegrationDirectory.vue`/`Guilds.vue`/guild chat — no new
  endpoints. Every section has its own empty state with a next action
  (Explore Integrators / Find a Guild / Find friends), and
  `summarizeActivityEntry` covers all twelve `guild.*` event kinds the
  server emits so Recent Activity doesn't fall back to a raw event-kind
  string for guild events. Friends + live presence loading is shared with
  `Friends.vue` through `apps/hub/src/composables/useFriendsPresence.ts`;
  Latest Messages' per-guild lookup lives in
  `apps/hub/src/composables/useLatestGuildMessages.ts`.
- **Read-only until Edit.** Nothing on a logged-in page is an open input by
  default: a value renders as a styled read-only display
  (`AvalonEditableField`) and only becomes editable when the user
  presses Edit — Enter, Save, or leaving the field commits (only if it
  changed), Escape/Cancel reverts. Actions that need input (add a friend,
  recover with a phrase) sit behind a button that reveals the form. Login
  and Create Identity are the exception, since entering an id/name is the
  whole screen.
- `Profile.vue` — avatar/name/handle hero with log out, the profile fields
  (display name, avatar URL — each saves on its own edit, one change per
  `profile.updated`), and the device setup / recovery / pending-approval /
  device-list cards (#134/#135/#145); device names are editable fields too. `Login.vue`/`CreateIdentity.vue` sit in
  `AuthLayout.vue` (wordmark above a centered `AvalonAuthCard`); auth is
  identity-id + passkey only.
- `Friends.vue` (#18) keeps each friend's presence live via
  `apps/hub/src/api/client.ts::openPresenceSocket` (`GET /ws/presence`,
  #136), re-subscribed with the current friend-id set on every refresh.
  `GET /friends` + `GET /friends/requests` still poll every 5 minutes
  (membership changes — accept/decline/remove — aren't pushed, so they
  still need it) and merge with a one-shot presence catch-up client-side in
  `apps/hub/src/api/friends.ts` — `GET /friends` does not embed presence
  server-side (see
  [social-graph](./social-graph.md)/[presence](./presence.md)), the same
  merge the Rust SDK's `Session::friends()` does (#17), ported to
  TypeScript since the Hub doesn't consume the Rust SDK directly. "Add
  friend" accepts either a raw identity id or a `display_name#1234` handle
  (#128) — a handle (anything containing `#`) is resolved to an identity id
  client-side via `GET /friends/handle/:handle` before the request is sent,
  since `createFriendRequest` always targets an identity id on the wire; the
  identity header in `HubShell.vue` shows the caller's own handle as the
  shareable form. One remaining scope cut, documented not silent: there is
  no "Playing &lt;integrator&gt;" label (no integrator registry exists, `Presence.playing`
  is always `null` in practice today).
- `Messages.vue` (#105) — direct/small-group conversations
  ([communication.md](./communication.md)): a conversation-list sidebar next
  to the active thread, swapped in place on selection rather than remounted
  — the same shape #241 set for Guild.vue's Channels tab, routed the same
  way (`/messages` and `/messages/:id` render the same component, the param
  just pre-selects). `useConversations`/`useConversationThread`
  (`apps/hub/src/composables/`) mirror `useGuildChat.ts`'s
  load-once-then-poll/cursor-pagination shape. Renders messages via
  `AvalonChatMessage`/`AvalonChatComposer` unmodified — both were already
  wire-shape-agnostic (no guild/channel field in their props), so no new
  `packages/ui` component was needed. Starting a conversation is
  `AvalonFriendRow`'s new `message` emit (`Friends.vue`), not a separate
  "new message" flow — `POST /conversations`'s idempotent-on-participant-set
  behavior means "start" and "open the existing one" are the same call.
- `Activity.vue` (#121) — "what does the network know about me": the
  caller's own protocol events from `GET /me/history`
  (`crates/server/src/handlers.rs::my_history`), which reads the ledger
  directly rather than through the indexer (see
  [settlement](./settlement.md)'s "Today in the repo" for why). Not a
  chain/block explorer — that's a separate, unscoped idea. Each entry
  renders as a human-readable summary line, not the raw `kind` string
  (`apps/hub/src/api/activityFeed.ts::summarizeActivityEntry`, #146) — an
  event kind this build doesn't recognize falls back to the raw kind
  rather than erroring, since the kind catalogue (#82) keeps growing.
  Timestamps render relative ("3 hours ago") via
  `formatActivityTimestamp`. An empty list (a fresh identity, only
  `identity.created` pending in the outbox) renders a sensible message
  rather than a blank page.
- `Guilds.vue` / `Guild.vue` / `GuildChannel.vue` (#24) — `/guilds` (my
  guilds via `GET /me/guilds` + a button-first create form),
  `/guilds/:id` (overview, roster grouped by role, roles, channels,
  management), `/guilds/:id/channels/:cid` (chat), all nested under
  `HubShell` like every other page. Roster presence is merged client-side
  in `apps/hub/src/api/guilds.ts::listMembersWithPresence` — `GET
  /guilds/{id}/members` doesn't embed presence server-side either, the
  same gap and the same fix as `Friends.vue`. No poll-vs-push split here:
  everything guild-related (roster, roles, channels, membership) polls
  every 5 minutes, matching #20/#21/#22's own design note that milestone 1
  doesn't need a WebSocket for guild data; chat messages poll faster
  (`useGuildChat.ts`, 15s) since a channel is closer to a live
  conversation. Chat is newest-at-bottom with load-older-on-scroll via
  #22's `before`/`limit` cursor; the composer enforces the server's
  4000-character body cap (`MESSAGE_BODY_MAX_CHARS`) with a live counter.
  Every management action is gated client-side on the caller's own
  resolved permission list (owner is always authorized, structurally,
  matching `crates/server/src/guilds.rs::has_guild_permission`) but the
  server re-checks independently — a 403 on a still-reachable action shows
  a plain error rather than crashing. See
  [guilds](./guilds.md)'s "Today in the repo" for the two real gaps this
  surfaced (the `manage_channels` permission stub, and no endpoint to list
  the caller's own pending guild invites) rather than working around them
  with a Hub-only endpoint. **Issue #57** completed this view rather than
  starting it over: a "Currently playing" card on `Guild.vue` groups the
  roster's merged presence by integrator (`apps/hub/src/api/guilds.ts`'s
  `groupMembersPlayingByIntegrator`/`formatPlayingSummary`, "N members playing
  X" per #74), and a "History" card states plainly that guild event
  history isn't available yet rather than fabricating one from the
  current roster (no `GET /guilds/{id}/history` endpoint or indexer
  projection exists — #82). Roster/chat access itself is already exactly
  what #87 would additionally scope down: current guild membership gates
  `GET /guilds/{id}/members` and the channel/message endpoints outright,
  with no separate public/members-only/hidden mode to render — #87 stays
  open and undecided, and nothing here simulates scopes it hasn't defined.
- `apps/mobile-hub/` — the same scaffold in a Tauri shell; `src-tauri/` is its
  own Cargo package, not a workspace member. Not wired to the identity flow
  yet (#60). `src-tauri/icons/` is generated from `src-tauri/app-icon.svg`
  (issue #311, `tauri icon apps/mobile-hub/src-tauri/app-icon.svg -o
  apps/mobile-hub/src-tauri/icons`) — the real Avalon mark (a six-facet gem
  in the brand palette, matching `.vscode/mocks/IconMock.png`'s "Mark
  (Standalone)"), not the solid-color placeholder the Tauri build-fix
  commit generated before any branding existed. `apps/hub/public/favicon.svg`
  is the same mark, linked from `apps/hub/index.html`. `AvalonIcon`'s
  `logo` glyph (`packages/ui/src/components/AvalonIcon.vue`, used at 22px
  for `HubShell.vue`'s sidebar brand mark) is a solid diamond silhouette
  matching the mock's "Monochrome" app-icon variant — filled rather than
  stroked, unlike every other glyph in that component, since it's a brand
  mark, not a line icon. Not yet done, and not this ticket's job: achievement
  /rank badges (star, rare, epic, legendary, …) get their own component per
  #311's design rather than being forced through `AvalonIcon`'s
  single-color `currentColor` model — tracked on the concurrent #332.
- `IntegrationDirectory.vue` / `IntegrationProfile.vue` (#270, first buildable slice of
  #90) — `/integrations` (a search box + name/newest sort over `GET /integrations`,
  `apps/hub/src/composables/useDiscoverIntegrations.ts`, same server-side
  cursor-pagination-on-filter-change pattern `useDiscoverGuilds.ts`
  established for #154) and `/integrations/:slug` (the profile page: `GET
  /integrations/{slug}`'s public fields plus `GET /integrations/{slug}/registry`'s five
  metrics, each rendered through `AvalonMetricTile` with its definition
  and class label — never a bare number). `/integrations` and `/integrations/:slug`
  redirect to the routes above (`apps/hub/src/router/index.ts`), so
  existing deep links don't 404. The nav entry (`HubShell.vue`) reads
  "Connected Apps", not "Games" — per #282/#275, `category` tabs
  (Integrators/Apps/Services) filter the fetched list client-side, though only
  Integrators has real registrants today. Both public, unauthenticated reads —
  no session token required, matching the endpoints' own visibility (#273).
  `status` renders through a badge that's visibly distinct whenever it
  isn't `"active"`. No ranking, no score, no "recommended" ordering
  anywhere in either view, per #89's invariant —
  `apps/hub/src/views/IntegrationDirectory.test.ts` and `IntegrationProfile.test.ts`
  assert every metric's label renders and that no score/ranking element
  exists. `IntegrationProfile.vue` also renders `GET /integrations/{slug}/keys`'s
  full issuer key history (#90, #80/#84 now decided/closed) — every key
  ever registered, root or operational, with its revoked status — and,
  only when the viewer is logged in, a "Your access" section reusing
  `AvalonConnectionCard`/`revokeGrant`/`disconnectIntegrator` exactly as
  `Connections.vue` does, scoped to just this integrator's binding. Recognition
  relationships (which other issuers recognize this one) remain unbuilt —
  no server-side concept of that exists yet; recognition today is scoped
  per-attestation (#33), not an integrator-level relationship graph — tracked as
  the one open item left on #90.
- `Connections.vue` (`/connections`, #27/#83) — lists the caller's own
  `IntegratorBinding`s with a revoke action. `ConnectIntegration.vue` (`/connect/:slug`,
  #27) — the capability-consent flow a user lands on to authorize an integrator,
  posting to `POST /integrations/{slug}/connect`.
- `RecoverIdentity.vue` (`/recover-identity`, #201) — the guardian-based
  recovery flow's entry point for a device with no registered passkey; see
  [identity](./identity.md)'s "Today in the repo" for the server-side
  mechanics.
- `packages/ui/` — `@avalon/ui`: `AvalonButton`, `AvalonTextField`,
  `AvalonForm`, `AvalonAuthCard`, `AvalonPresenceBadge`, `AvalonFriendRow`,
  `AvalonFriendRequestRow`, `AvalonAvatar`, `AvalonCard`, `AvalonIcon` (a
  small inline-SVG set, no icon library), `AvalonSidebarNav`,
  `AvalonBottomNav`, `AvalonUserChip`, `AvalonGuildCard`,
  `AvalonGuildMemberRow`, `AvalonRoleBadge`, `AvalonChannelList`,
  `AvalonChatMessage`, `AvalonChatComposer` (#24), `AvalonIntegratorCard`,
  `AvalonMetricTile` (#270) — all presentational (props in,
  events out; the nav components take `{ label, to, icon, active, disabled }[]`
  and emit which entry was picked, never emitting for a disabled one) in
  the `components/` / `styles/` / `stories/` split, plus `styles/tokens.css`
  and `styles/global.css`. No `<style>` blocks in `.vue` files; styling
  lives in `.module.scss` and references tokens only. Has no test runner or
  working Storybook config of its own yet — components are tested from
  `apps/hub/src/ui-components.test.ts`, which already has a working vitest
  setup and consumes them the same way the app does.
- **Responsive is CSS-only, in the component's own files — never a
  mobile-only fork** (#61's invariant, generalized library-wide by #310).
  `HubShell.vue` already collapses sidebar → `AvalonBottomNav` and hides the
  header search/footer at `768px` (`apps/hub/src/views/HubShell.module.scss`);
  `Home.vue`'s grid and `IntegrationProfile.vue`'s metrics grid have their own
  breakpoints. #310's audit covered every `packages/ui` component the mock's
  Home/Login/CreateIdentity/Profile/Friends/`NetworkStatus` screens actually
  use (`AvalonAuthCard`, `AvalonForm`, `AvalonTextField`, `AvalonButton`,
  `AvalonCard`, `AvalonEditableField`, `AvalonFriendRow`,
  `AvalonFriendRequestRow`, `AvalonSuggestionRow`, `AvalonAvatar`,
  `AvalonPresenceBadge`, `AvalonWarningBanner`, `AvalonModal`, `AvalonIcon`):
  every one already holds up at phone width without a breakpoint of its own,
  because the library was already built on `flex-wrap`, `min-width: 0` on
  flex children, `text-overflow: ellipsis` on names/labels, `flex-shrink: 0`
  on icons, and relative (`rem`/`%`) sizing throughout rather than fixed
  pixel widths — so no component in that set needed a media query, a new
  prop/variant, or a compact-viewport story (the design's "only then add an
  explicit variant" case never triggered). Verified by reading every
  audited component's `.module.scss`, not by screenshot — this sandbox has
  no headless browser to render one. `AvalonSidebarNav`/`AvalonBottomNav`
  themselves are desktop/mobile counterparts by design (`HubShell.vue`
  swaps between them), not something either needs to also flex into the
  other's role.

  A second #310 pass audited the guild (`AvalonGuildCard`,
  `AvalonGuildMemberRow`, `AvalonChannelList`, `AvalonChatMessage`,
  `AvalonChatComposer`), event (`AvalonEventCard`, `AvalonRsvpControl`,
  `AvalonRsvpRosterPanel`), and integrator-directory (`AvalonIntegratorCard`,
  `AvalonMetricTile`, `AvalonFilterBar`, `AvalonConnectionCard`) components
  the same way, and this time found real gaps, all the same shape: a
  truncating `.name` (`AvalonGuildCard`, `AvalonIntegratorCard`,
  `AvalonConnectionCard`) sitting in a `min-width: 0` flex row next to a
  `flex-shrink: 0` sibling (a status badge, a slug) but missing `min-width:
  0` on itself — without it a flex item's default `min-width: auto` floors
  it at its own content width, so `text-overflow: ellipsis` never actually
  triggers and a long guild/integrator name pushes the badge/slug out and
  overflows the card at phone width. Fixed by adding `min-width: 0` to each
  `.name`. Two more real ones: `AvalonGuildMemberRow`'s row (avatar + name +
  role badge + presence badge + up to two action buttons) had no
  `flex-wrap`, so at phone width there's more fixed-size content than fits
  one line even after the name collapses — now wraps instead of
  overflowing. `AvalonFilterBar`'s two `flex: 1` fields (search + sort) had
  no floor, so a narrow container could squeeze a `<select>`/`<input>`
  below its own usable width before ever triggering the intrinsic-content
  overflow browsers apply to form controls — now `flex-wrap` on the bar
  plus `min-width: 8rem` per field lets the sort field wrap to its own line
  rather than both becoming illegibly narrow. Everything else in this pass
  (`AvalonChannelList`, `AvalonChatMessage`, `AvalonChatComposer`,
  `AvalonEventCard`, `AvalonRsvpControl`, `AvalonRsvpRosterPanel`,
  `AvalonMetricTile`) already held up, same as the first pass.

  A third pass closed out the rest of the library:
  `AvalonCapabilityConsentRow`, `AvalonCalendarMonth`, `AvalonSuggestionRow`,
  `AvalonUserChip`, `AvalonRoleBadge`, `AvalonModal` (already fine, no
  change), and `AvalonDateTimeField` — which had one more real bug of its
  own kind: its date/time picker `.popover` was a fixed `26rem` (416px),
  `position: absolute`, with no ceiling, so on any viewport narrower than
  that (plus the field's own left offset) it ran off the edge of the
  screen with no way to reach the rest of it. Capped it with `max-width:
  calc(100vw - 2 * var(--av-space-4))` so it shrinks to fit instead.
  That's every `packages/ui` component now audited except
  `AvalonAchievementCard` (deliberately skipped — concurrent work on its
  icons, #332).
- The server surface the Hub calls today: `POST /identities/register/start`
  + `/finish`, `POST /sessions/start` + `/finish`, `GET|PATCH /me`,
  `GET /friends`, `GET|POST /friends/requests`,
  `POST /friends/requests/:id/accept`, `DELETE /friends/requests/:id`,
  `DELETE /friends/:id`, `GET /presence` — a real WebAuthn + Ed25519 flow
  for identity, not the plain-credential shape earlier drafts of this doc
  set implied. Since #24: `GET|POST /guilds`, `GET|PATCH /guilds/:id`,
  `GET|POST /guilds/:id/roles`, `PATCH /guilds/:id/roles/:idx`,
  `POST /guilds/:id/transfer-ownership`, `POST /guilds/:id/integrations/:integrator_id`,
  `POST /guilds/:id/invites`, `POST /guilds/:id/invites/:invite_id/accept`
  + `/decline`, `POST /guilds/:id/join` + `/leave`,
  `GET /guilds/:id/members`, `PATCH|DELETE /guilds/:id/members/:identity_id`,
  `GET /me/guilds`, `GET|POST /guilds/:id/channels`,
  `PATCH /guilds/:id/channels/:cid`, `POST /guilds/:id/channels/:cid/archive`,
  `GET|POST /guilds/:id/channels/:cid/messages`,
  `DELETE /guilds/:id/channels/:cid/messages/:mid`. Since #270:
  `GET /integrations`, `GET /integrations/:slug/registry`. Also called, not yet listed
  above: `POST|DELETE /integrations/:slug/connect` (bind/unbind), `GET
  /me/connections`, `GET /me/grants`, and `DELETE
  /integrations/:slug/grants/:capability` (#27/#83), used by
  `Connections.vue`/`ConnectIntegration.vue`.

