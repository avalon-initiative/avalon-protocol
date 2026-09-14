# Presence and the Realtime Vertical

**Realtime presence is ephemeral state. It never enters durable protocol history.**
It is the third logical vertical of the network alongside settlement and
query/indexing, and it is the one whose loss costs nothing: if presence storage
disappears, identities look offline until their next heartbeat. Decided in
[#78](https://github.com/LunarVagabond/avalon-protocol/issues/78).

## Two kinds of fact

```text
"User X is currently online in Ashen Realms."      ephemeral
"User X defeated the Dragon Lord."                 durable history
```

The first changes every few seconds, matters only now, and is worthless in an
append-only log. The second is the kind of fact Avalon exists to preserve. The
architecture keeps them apart at every layer — see
[overview](./overview.md) for the three verticals.

## What presence covers

| Field | Example |
|---|---|
| status | online / away / do not disturb / offline |
| current game | Ashen Realms |
| current server / region | NA-East |
| activity | "in a raid", free-form, game-supplied |
| heartbeat | last seen at |
| connection state | which realtime session, transient |

```text
User X
    Online
    Playing Ashen Realms
    Server: NA-East
```

Anything a user would want a friend or guildmate to know *right now*, and
nothing anyone needs to prove later.

## Rules

- Presence is not a `ProtocolEvent`. It is never written to the settlement store
  and is never in [rebuild](./disaster-recovery.md) scope.
- Presence may live in process memory, a cache, or a dedicated realtime service.
  It is not required to be in Postgres.
- Presence is permissioned. A game publishing presence on an identity's behalf
  requires the game to hold `presence.publish` under an active
  [binding](./game-bindings.md); who else can see it is a
  [visibility](./privacy.md) setting (friends, guild, nobody) — friends-only
  by default.
- A game publishes presence for identities bound to it (it knows they're
  connected); it cannot publish presence for identities that are not bound to it.
- Realtime population numbers ("players online in Game A") are labeled realtime
  wherever shown and never stored as durable metrics in the
  [game registry](./game-registry.md).
- What *is* durable about presence-adjacent activity — a binding being
  established, an achievement being earned — is its own protocol event, never
  inferred from heartbeats.
- `status` has four values: `online`, `away`, `do_not_disturb`, `offline`.
  `online` is the only one tracked automatically — a live entry within the
  heartbeat TTL reads `online`, an entry that's gone stale reads `offline`.
  The other three are **sticky manual overrides**, Discord-style: once a
  caller explicitly publishes `away`, `do_not_disturb`, or `offline` via
  `PUT /me/presence`, that status is reported on every subsequent read
  regardless of TTL expiry or continued heartbeat activity — it does not
  silently flip back to `online` on its own, and it survives a reconnect
  (though not a server restart, since presence is entirely in-memory; see
  "Deployment" below). The only way to clear an override is to explicitly
  publish `online` again, which immediately resumes automatic TTL tracking.

## What presence powers

- friends lists with online/where
- guild rosters with "members currently playing: 42 — Ashen Realms"
- the Hub's activity view and the mobile-hub companion app
  ([Proposal §22](../stakeholders/Proposal.md#22-companion-apps-presence-beyond-the-game))
- cross-game "join me" flows and matchmaking integrations a game chooses to build
- community tools

## Deployment

Milestone 1 runs presence inside `avalon-server`. Because presence shares no
storage with settlement or indexing and emits no events, it can move to its own
process or node role later ([nodes](./nodes.md)) without touching either. Scaling
realtime connections is a separate axis from scaling history or queries
([scalability](./scalability.md)).

## Today in the repo

- `crates/protocol/src/social.rs` — `PresenceStatus { Online, Away, DoNotDisturb, Offline }` and
  `Presence { identity_id, status, playing: Option<GameId>, updated_at }`.
- `crates/server/src/presence.rs` — an in-process `PresenceStore` (`Arc<RwLock<HashMap<...>>>`
  keyed by identity), never a migrated table, never touching the outbox or
  `avalon-chain`. `PUT /me/presence` lets the caller publish their own
  `status`; they can never set `playing`. `PUT /presence/:identity_id` lets
  a game publish presence on behalf of an identity it's bound to —
  authenticated via `crate::authz`'s `Caller`/`require_capability` (#28):
  the caller must resolve to `Caller::Game`, hold an active
  `presence.publish` grant under an active [binding](./game-bindings.md)
  to that identity (`crates/server/src/connections.rs`, #26/#83), and
  `playing`, if set at all, must equal the game's own id — a game claiming
  to be a *different* game's `playing` value is rejected
  (`AppError::PresencePlayingMismatch`) even with a valid grant. `GET
  /presence?ids=…` and `GET /ws/presence` default to friends-only
  visibility: the caller's own entry is always visible; anyone else's is
  visible only if they're currently friends
  (`crates/server/src/friends.rs`'s `friend_partners`) and there's no
  block between them (issue #97, checked first). This is a literal
  implementation of `docs/architecture/privacy.md`'s proposed default for
  this one resource, **not** the full per-resource visibility-scope model
  #87 still owns (guild visibility, a private setting, etc.) — see that
  file's own "Today in the repo" note. An identity can independently opt
  `playing` out of ever being shown, regardless of any game's grant
  (`presence_preferences.hide_playing`, set via `PUT /me/presence`,
  `crates/server/db/migrations/0017_presence_preferences`) — deliberately
  a durable Postgres row, not part of the ephemeral store, since it's a
  standing preference rather than a realtime fact. An entry not refreshed
  within the TTL (120s by default, `AVALON_PRESENCE_TTL_SECS` overrides it
  for testing) reads as `Offline`, never a guess — **unless** its last
  explicitly-published status was `Away`, `DoNotDisturb`, or `Offline`
  itself, in which case it's a sticky manual override and keeps reading as
  that status past the TTL, until explicitly set back to `Online`
  (`PresenceStore::get`'s own doc comment has the full mechanism). Sticky
  overrides live in the same in-memory `PresenceStore` as everything else
  here, not `presence_preferences` — a reconnect never touches this store
  (only a server restart clears it), so in-memory already satisfies
  "survives reconnect"; the accepted tradeoff is that, like all presence
  state, an override is lost on server restart.
- **Deferred, documented, not silently missing**: the full per-resource
  visibility-scope model (friends/guild/private, per resource, identity- and
  guild-configurable) is #87's open decision — presence's friends-only
  default above is one literal instance of it, not the general mechanism.
- `GET /ws/presence?token=…` (#136, transport chosen in #119) — a live push
  transport, additive to `GET /presence`, not a replacement. Auth is a
  `?token=` query parameter,
  not the usual `Authorization` header — a browser `WebSocket` handshake
  can't set custom headers (`handlers::authenticate_token`). A connected
  client sends `{"type":"subscribe","ids":[...]}` (additive — sending it
  again with more ids grows the subscription, doesn't replace it); the
  server immediately replies with a catch-up snapshot for each newly
  subscribed id, then pushes every subsequent `PresenceStore::set()` for a
  subscribed id as it happens. Fan-out is a bounded, lossy
  `tokio::sync::broadcast` channel (`PresenceStore::subscribe`) — a slow
  consumer drops interim ticks rather than backing up the publisher, an
  acceptable tradeoff for ephemeral presence, unlike the outbox's durable
  delivery guarantee for real protocol events. Same "no visibility
  filtering yet" cut as `GET /presence` above (deferred to #87) — any valid
  session may subscribe to any ids it names.
- `crates/sdk/src/social.rs::Session::subscribe_presence` — the Rust SDK's
  client for the above, additive to `presence()`/`presence_of()`. Returns a
  `tokio::sync::mpsc::UnboundedReceiver<Presence>`; a background task
  forwards every pushed update onto it, and dropping the receiver ends that
  task on its next send attempt (no separate unsubscribe call).
- `apps/hub/src/api/client.ts::openPresenceSocket` — the Hub's client for
  the same endpoint (a plain browser `WebSocket`, not the Rust SDK, which
  the Hub doesn't consume directly). `Friends.vue` (#18) uses it to keep
  each friend's presence live; the friend-*list* poll (membership changes —
  a request accepted/declined, a friend removed) still runs, just much
  slower now that presence itself doesn't depend on it for liveness.

## Decisions and tickets

- [#78](https://github.com/LunarVagabond/avalon-protocol/issues/78) — ADR:
  realtime presence is ephemeral and never enters durable history.
- [#16](https://github.com/LunarVagabond/avalon-protocol/issues/16) — presence
  tracking (status + playing game) + update endpoint.
- [#87](https://github.com/LunarVagabond/avalon-protocol/issues/87) — visibility
  scopes.
- [#89](https://github.com/LunarVagabond/avalon-protocol/issues/89) — registry read
  model, where realtime counts are labeled as such.
- [#14](https://github.com/LunarVagabond/avalon-protocol/issues/14) — Epic: Social
  Graph (Friends & Presence).
