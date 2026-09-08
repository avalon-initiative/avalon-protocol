# The Hub

**The Avalon Hub is a client of the network. It is not the network.** It talks
to `avalon-server` through the same API, authentication, and capability model as
any game or third-party client, has no backend of its own, and holds no
privilege another authorized client couldn't have. Decided in
[#77](https://github.com/LunarVagabond/avalon-protocol/issues/77); narrative in
[Proposal §6](../stakeholders/Proposal.md#6-avalon-hub).

## The world outside the games

The Hub is where a player interacts with the network itself, with no game open:

- create and manage a persistent identity; connect keys or a wallet where that
  applies ([identity](./identity.md))
- profile and player-controlled metadata
- friends and presence
- guilds and guild chat
- achievements, attestations, and history
- game discovery and per-game profiles
- permissions: which games have which capabilities, and revoking them
- connected games (bindings)
- recognition preferences (which issuers' claims to feature)
- game event history
- later: asset ownership

It is not a launcher, a store, a distribution platform, or a game authority. It
does not own the games it lists. Steam + Roblox + universal launcher is the shape
to avoid ([Proposal §28](../stakeholders/Proposal.md#28-difference-from-roblox)).

## One client among several

Because the Hub uses only the public API, other clients are possible by
construction: web, mobile, desktop, a Discord integration, a game's native UI, a
third-party application. `apps/mobile-hub` is the first proof — the same UI in a
Tauri shell for desktop and mobile, for guild chat and presence without a game
running ([Proposal §22](../stakeholders/Proposal.md#22-companion-apps-presence-beyond-the-game)).
A player who never installs any Hub loses nothing at the protocol level.

Rules that keep this true:

- no Hub-only endpoints, read paths, or credentials
- anything the Hub needs that the server lacks is a server ticket first
- no server-side rendering that reaches into Postgres; no Hub-scoped tables
- `packages/ui` is a component library for clients and knows nothing about
  storage or settlement

## Hub and achievements

The Hub shows every authentic, valid claim with its provenance, regardless of
whether any game recognizes it:

```text
Your History

🏆 Dragon Slayer
   Ashen Realms · Verified

🏆 Dragon Slayer
   WorldZero · Verified

🏆 Dragon Slayer
   Community MMO #47 · Verified
```

All three are authentic. The player can feature, hide, filter, and sort. The Hub
never implies Avalon has judged one more prestigious than another, and a revoked
claim shows as revoked with its history, not as a gap. See the
[trust model](./trust-model.md) and [revocation](./revocation.md).

## Hub and guilds

Guilds are network primitives, so the Hub is their natural client: create, join,
leave, manage roles, chat, see members and their presence, see which games
members are playing, view history, coordinate across games. A guild is unaffected
by any game shutting down. See [guilds](./guilds.md).

## Hub and game discovery

A directory of games connected to Avalon and a profile page per game, rendering
[registry](./game-registry.md) facts with their definitions and class labels
(durable-derived / realtime / self-reported), issuer status and key history, and
recognition relationships. Sort options are explicit and neutral; there is no
score and no trust-derived "recommended" ordering. A suspended or revoked issuer
is visibly marked.

## Today in the repo

- `apps/hub/` — Vue 3 + Vite + TypeScript, routed with `vue-router`. Identity
  creation (`CreateIdentity.vue`) and login (`Login.vue`) are their own
  pre-authenticated pages — see [identity](./identity.md)'s "Today in the
  repo" for the crypto/API layer underneath registration/login (#55). All
  server traffic goes through `apps/hub/src/api/`; nothing calls `fetch`
  directly outside it.
- **The logged-in Hub is a persistent shell with tabs, not separate pages.**
  `HubShell.vue` renders an always-visible identity header (display name,
  handle, identity id, log out) plus tab navigation (`AvalonTabs`,
  `packages/ui`), with `<RouterView />` swapping tab content in place. Tabs
  are real nested routes under a shared parent (`/profile`, `/friends`,
  `/activity` as children of `HubShell.vue` in
  `apps/hub/src/router/index.ts`) — URL-addressable and bookmarkable, not
  client-side-only state; `requiresAuth` is set once on the parent route
  and inherited by every child via `vue-router`'s meta-merging across
  matched records. Today's three real tabs are Profile (`Profile.vue` —
  display-name/avatar-url edit form; the identity header/logout lives in
  the shell instead), Friends (`Friends.vue`, #18), and Activity
  (`Activity.vue`, #121 — see below). **Every future Hub UI ticket should
  add a new tab (a child route + a `tabs` entry in `HubShell.vue`) instead
  of a new standalone route** — this is what #24 (guild view + chat), #35
  (achievements), and #105 (DMs) should build against.
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
  no "Playing &lt;game&gt;" label (no game registry exists, `Presence.playing`
  is always `null` in practice today).
- `Activity.vue` (#121) — "what does the network know about me": a plain
  list of the caller's own protocol events from `GET /me/history`
  (`crates/server/src/handlers.rs::my_history`), which reads the ledger
  directly rather than through the indexer (see
  [settlement](./settlement.md)'s "Today in the repo" for why). Not a
  chain/block explorer — that's a separate, unscoped idea; just the
  caller's own events, kind + subject + timestamp + raw payload, newest
  first. An empty list (a fresh identity, only `identity.created` pending in
  the outbox) renders a sensible message rather than a blank page.
- `apps/mobile-hub/` — the same scaffold in a Tauri shell; `src-tauri/` is its
  own Cargo package, not a workspace member. Not wired to the identity flow
  yet (#60).
- `packages/ui/` — `@avalon/ui`: `AvalonButton`, `AvalonTextField`,
  `AvalonForm`, `AvalonAuthCard`, `AvalonPresenceBadge`, `AvalonFriendRow`,
  `AvalonFriendRequestRow`, `AvalonTabs` (#130 — takes a plain
  `{ label, to, active }[]` and emits which one was picked; no route
  awareness inside the component itself, same invariant as the rest of this
  package) in the `components/` / `styles/` / `stories/`
  split. No `<style>` blocks in `.vue` files; styling lives in
  `.module.scss`. Has no test runner or working Storybook config of its own
  yet (pre-existing gaps, noted in #55's PR) — the three friends components
  are instead tested from `apps/hub/src/ui-components.test.ts`, which
  already has a working vitest setup and consumes them the same way the app
  does.
- The server surface the Hub calls today: `POST /identities/register/start`
  + `/finish`, `POST /sessions/start` + `/finish`, `GET|PATCH /me`,
  `GET /friends`, `GET|POST /friends/requests`,
  `POST /friends/requests/:id/accept`, `DELETE /friends/requests/:id`,
  `DELETE /friends/:id`, `GET /presence` — a real WebAuthn + Ed25519 flow
  for identity, not the plain-credential shape earlier drafts of this doc
  set implied.

## Decisions and tickets

- [#77](https://github.com/LunarVagabond/avalon-protocol/issues/77) — ADR: the Hub
  is a client of the network, not the network.
- [#54](https://github.com/LunarVagabond/avalon-protocol/issues/54) — Epic: Avalon
  Hub (Web): [#55](https://github.com/LunarVagabond/avalon-protocol/issues/55)
  identity/login, [#56](https://github.com/LunarVagabond/avalon-protocol/issues/56)
  friends/presence, [#57](https://github.com/LunarVagabond/avalon-protocol/issues/57)
  guilds + chat, [#58](https://github.com/LunarVagabond/avalon-protocol/issues/58)
  achievements + connected games/permissions,
  [#90](https://github.com/LunarVagabond/avalon-protocol/issues/90) game discovery.
- [#59](https://github.com/LunarVagabond/avalon-protocol/issues/59) — Epic: Avalon
  Mobile-Hub (Tauri): [#60](https://github.com/LunarVagabond/avalon-protocol/issues/60),
  [#61](https://github.com/LunarVagabond/avalon-protocol/issues/61),
  [#62](https://github.com/LunarVagabond/avalon-protocol/issues/62).
- [#76](https://github.com/LunarVagabond/avalon-protocol/issues/76) — ADR: trust
  model (what the Hub may and may not imply about a claim).
