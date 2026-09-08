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
  creation (`CreateIdentity.vue`), login (`Login.vue`), and a minimal
  profile view (`Profile.vue`) are real, wired against a live `avalon-server`
  — see [identity](./identity.md)'s "Today in the repo" for the crypto/API
  layer underneath them (#55). All server traffic goes through
  `apps/hub/src/api/`; nothing calls `fetch` directly outside it.
- `apps/mobile-hub/` — the same scaffold in a Tauri shell; `src-tauri/` is its
  own Cargo package, not a workspace member. Not wired to the identity flow
  yet (#60).
- `packages/ui/` — `@avalon/ui`: `AvalonButton`, `AvalonTextField`,
  `AvalonForm`, `AvalonAuthCard` in the `components/` / `styles/` / `stories/`
  split. No `<style>` blocks in `.vue` files; styling lives in `.module.scss`.
- The server surface the Hub calls today: `POST /identities/register/start`
  + `/finish`, `POST /sessions/start` + `/finish`, `GET|PATCH /me` — a real
  WebAuthn + Ed25519 flow, not the plain-credential shape earlier drafts of
  this doc set implied.
- `npm install` at the repo root has been run and verified in this
  environment.

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
