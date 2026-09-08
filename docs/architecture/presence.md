# Presence and the Realtime Vertical

**Realtime presence is ephemeral state. It never enters durable protocol history.**
It is the third logical vertical of the network alongside settlement and
query/indexing, and it is the one whose loss costs nothing: if presence storage
disappears, players look offline until their next heartbeat. Decided in
[#78](https://github.com/LunarVagabond/avalon-protocol/issues/78).

## Two kinds of fact

```text
"Player X is currently online in Ashen Realms."      ephemeral
"Player X defeated the Dragon Lord."                 durable history
```

The first changes every few seconds, matters only now, and is worthless in an
append-only log. The second is the kind of fact Avalon exists to preserve. The
architecture keeps them apart at every layer — see
[overview](./overview.md) for the three verticals.

## What presence covers

| Field | Example |
|---|---|
| status | online / away / offline |
| current game | Ashen Realms |
| current server / region | NA-East |
| activity | "in a raid", free-form, game-supplied |
| heartbeat | last seen at |
| connection state | which realtime session, transient |

```text
Player X
    Online
    Playing Ashen Realms
    Server: NA-East
```

Anything a player would want a friend or guildmate to know *right now*, and
nothing anyone needs to prove later.

## Rules

- Presence is not a `ProtocolEvent`. It is never written to the settlement store
  and is never in [rebuild](./disaster-recovery.md) scope.
- Presence may live in process memory, a cache, or a dedicated realtime service.
  It is not required to be in Postgres.
- Presence is permissioned. Publishing it to a game requires the game to hold
  `presence.read` under an active [binding](./game-bindings.md); who else can see
  it is a [visibility](./privacy.md) setting (friends, guild, nobody).
- A game publishes presence for its own players (it knows they're connected);
  it cannot publish presence for players who are not bound to it.
- Realtime population numbers ("players online in Game A") are labeled realtime
  wherever shown and never stored as durable metrics in the
  [game registry](./game-registry.md).
- What *is* durable about presence-adjacent activity — a binding being
  established, an achievement being earned — is its own protocol event, never
  inferred from heartbeats.

## What presence powers

- friends lists with online/where
- guild rosters with "members currently playing: 42 — Ashen Realms"
- the Hub's activity view and the mobile-hub companion app
  ([Proposal §22](../Proposal.md#22-companion-apps-presence-beyond-the-game))
- cross-game "join me" flows and matchmaking integrations a game chooses to build
- community tools

## Deployment

Milestone 1 runs presence inside `avalon-server`. Because presence shares no
storage with settlement or indexing and emits no events, it can move to its own
process or node role later ([nodes](./nodes.md)) without touching either. Scaling
realtime connections is a separate axis from scaling history or queries
([scalability](./scalability.md)).

## Today in the repo

- `crates/protocol/src/social.rs` — `PresenceStatus { Online, Away, Offline }` and
  `Presence { identity_id, status, playing: Option<GameId>, updated_at }`.
- No presence endpoint, storage, or realtime transport exists. No server/region
  or activity fields yet.
- `avalon-server` has no websocket or push path; how presence is delivered to
  clients is undesigned.

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
