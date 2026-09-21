# The Hub

**The Avalon Hub is a client of the network. It is not the network.** It talks
to `avalon-server` through the same API, authentication, and capability model as
any integrator or third-party client, has no backend of its own, and holds no
privilege another authorized client couldn't have. Decided in
[#77](https://github.com/LunarVagabond/avalon-protocol/issues/77); narrative in
[Proposal §6](../../../stakeholders/Proposal.md#6-avalon-hub).

## The world outside the integrators

The Hub is where a user interacts with the network itself, with no integrator open:

- create and manage a persistent identity; connect keys or a wallet where that
  applies ([identity](../../backend-server/architecture/identity.md))
- profile and identity-controlled metadata
- friends and presence
- guilds and guild chat
- achievements, attestations, and history
- integrator discovery and per-integrator profiles
- permissions: which integrators have which capabilities, and revoking them
- connected integrators (bindings)
- recognition preferences (which issuers' claims to feature)
- integrator event history
- later: asset ownership

It is not a launcher, a store, a distribution platform, or an integrator authority. It
does not own the integrators it lists. Steam + Roblox + universal launcher is the shape
to avoid ([Proposal §28](../../../stakeholders/Proposal.md#28-difference-from-roblox)).

## One client among several

Because the Hub uses only the public API, other clients are possible by
construction: web, mobile, desktop, a Discord integration, an integrator's native UI, a
third-party application. `apps/mobile-hub` is the first proof — the same UI in a
Tauri shell for desktop and mobile, for guild chat and presence without an integrator
running ([Proposal §22](../../../stakeholders/Proposal.md#22-companion-apps-presence-beyond-the-game)).
A user who never installs any Hub loses nothing at the protocol level.

Rules that keep this true:

- no Hub-only endpoints, read paths, or credentials
- anything the Hub needs that the server lacks is a server ticket first
- no server-side rendering that reaches into Postgres; no Hub-scoped tables
- `packages/ui` is a component library for clients and knows nothing about
  storage or settlement

## Hub and achievements

The Hub shows every authentic, valid claim with its provenance, regardless of
whether any integrator recognizes it:

```text
Your History

🏆 Dragon Slayer
   Ashen Realms · Verified

🏆 Dragon Slayer
   WorldZero · Verified

🏆 Dragon Slayer
   Community MMO #47 · Verified
```

All three are authentic. The user can filter and sort (landed, #35); feature and
hide are deferred (see "Today in the repo" — they depend on a visibility/
preference store, #87, that doesn't exist yet). The Hub never implies Avalon has
judged one more prestigious than another, and a revoked claim shows as revoked
with its history, not as a gap. See the [trust model](../../backend-server/architecture/trust-model.md) and
[revocation](../../backend-server/architecture/revocation.md).

## Hub and guilds

Guilds are network primitives, so the Hub is their natural client: create, join,
leave, manage roles, chat, see members and their presence, see which integrators
members are playing, view history, coordinate across integrators. A guild is unaffected
by any integrator shutting down. See [guilds](../../backend-server/architecture/guilds.md).

## Hub and integrator discovery

A directory of integrators connected to Avalon and a profile page per integrator, rendering
[registry](../../backend-server/architecture/registry.md) facts with their definitions and class labels
(durable-derived / realtime / self-reported), issuer status and key history, and
recognition relationships. Sort options are explicit and neutral; there is no
score and no trust-derived "recommended" ordering. A suspended or revoked issuer
is visibly marked.

## Today in the repo

The full build-by-build detail — which views exist, what each renders, where
its data comes from — lives in its own file:
[`./hub-implementation-log.md`](./hub-implementation-log.md). Everything
below is real and implemented unless noted otherwise.

- **`apps/hub/`** — Vue 3 + Vite + TypeScript, routed with `vue-router`. The
  fetch client, session store, and WebAuthn/signing-key auth ceremony live in
  `packages/api-client` (`@avalon/api-client`, #60) — extracted out of
  `apps/hub/src/api/` so nothing calls `fetch` directly outside it and
  `apps/mobile-hub` imports the exact same code rather than a copy;
  `apps/hub/src/api/` keeps its own domain-specific modules
  (`guilds.ts`, `friends.ts`, ...) that build on top of it.
- **`apps/mobile-hub/`** is the same identity/login UI and Vue3 components as
  the web Hub, in a Tauri shell for desktop and mobile (#60), its own
  standalone Cargo package (`src-tauri/`) outside the root workspace. The
  session token is the one thing that differs per platform: the web Hub
  keeps it in `localStorage`, mobile-hub stores it through the OS keychain
  (macOS Keychain / Windows Credential Manager / Linux Secret Service) via
  the `keyring` crate, behind a single native command in `src-tauri/src/lib.rs`
  — everything else (the API client, the auth ceremony, session state) stays
  in TypeScript, shared with the web Hub through `@avalon/api-client`'s
  pluggable storage adapter. The avalon-server URL is configurable at
  runtime from an in-app settings screen (default `http://127.0.0.1:8080`),
  and the Tauri window's CSP is scoped to that origin rather than left
  unset. Guild chat/friends/presence views are separate, later work under
  epic #59 — #60's scope was wiring the shared client and auth flow only.
- **The logged-in Hub is a persistent shell, not separate pages** —
  `HubShell.vue`'s sidebar (desktop) / bottom nav (mobile) around a
  `<RouterView />`, nested child routes that stay URL-addressable. Nav
  entries for features that don't exist yet render disabled with a "Soon"
  tag rather than being hidden.
- **Achievements** (`Achievements.vue`, #35) — reads `GET /me/achievements`,
  merges in display names, and renders authenticity/validity only (never a
  score, per ADR #77). Feature/hide and recognition display are explicit
  known gaps, blocked on #87 and the trust-model's own tracked gap
  respectively.
- **Home, Profile, Friends, Messages, Activity, Guilds** — each a real,
  built view (`Home.vue` #148/#312, `Profile.vue`, `Friends.vue` #18,
  `Messages.vue` #105, `Activity.vue` #121, `Guilds.vue`/`Guild.vue` #24).
  Nothing on a logged-in page is an open input by default — read-only until
  an explicit edit action.
- **Integrator discovery** (`IntegrationDirectory.vue`/`IntegrationProfile.vue`, #270),
  **connections** (`Connections.vue`, #27/#83), and **guardian-based
  recovery** (`RecoverIdentity.vue`, #201) round out the unauthenticated and
  cross-integrator surfaces.
- **`packages/ui/`** (`@avalon/ui`) — the shared component library
  (`AvalonButton`, `AvalonTextField`, and the rest); responsive layout is
  CSS-only, living in each component's own files, never a separate
  mobile-specific component.

See [`./hub-implementation-log.md`](./hub-implementation-log.md) for exact
components, endpoints, and gaps behind every item above.

## Decisions and tickets

- [#77](https://github.com/LunarVagabond/avalon-protocol/issues/77) — ADR: the Hub
  is a client of the network, not the network.
- [#54](https://github.com/LunarVagabond/avalon-protocol/issues/54) — Epic: Avalon
  Hub (Web): [#55](https://github.com/LunarVagabond/avalon-protocol/issues/55)
  identity/login, [#56](https://github.com/LunarVagabond/avalon-protocol/issues/56)
  friends/presence, [#57](https://github.com/LunarVagabond/avalon-protocol/issues/57)
  guilds + chat, [#58](https://github.com/LunarVagabond/avalon-protocol/issues/58)
  achievements + connected integrators/permissions,
  [#90](https://github.com/LunarVagabond/avalon-protocol/issues/90) integrator discovery
  ([#270](https://github.com/LunarVagabond/avalon-protocol/issues/270) is its
  first buildable slice: `GET /integrations`, the integrator directory, and the
  per-integrator profile page; [#273](https://github.com/LunarVagabond/avalon-protocol/issues/273)
  made the directory and profile page render without auth;
  [#275](https://github.com/LunarVagabond/avalon-protocol/issues/275)/[#282](https://github.com/LunarVagabond/avalon-protocol/issues/282)
  generalized the nav/routes to "Connected Apps"/`/integrations` with
  category tabs, described above).
- [#59](https://github.com/LunarVagabond/avalon-protocol/issues/59) — Epic: Avalon
  Mobile-Hub (Tauri): [#60](https://github.com/LunarVagabond/avalon-protocol/issues/60),
  [#61](https://github.com/LunarVagabond/avalon-protocol/issues/61),
  [#62](https://github.com/LunarVagabond/avalon-protocol/issues/62).
- [#76](https://github.com/LunarVagabond/avalon-protocol/issues/76) — ADR: trust
  model (what the Hub may and may not imply about a claim).
