# Scalability

The stress model is deliberately large. **1,000 games × 100,000 players each =
100,000,000 players.** That is **not** 100 million gameplay events per second
flowing through Avalon — gameplay stays game-side, always. The question is how
many *durable* facts that population produces, how much history it accumulates,
how many reads and connections it generates, and whether every part of the
system scales independently of the others.

## What scale is not

Not transaction throughput on a chain. Movement, combat, HP, XP ticks, NPC
state, and ordinary chat never reach Avalon
([`./protocol-events.md`](./protocol-events.md)). If a design puts hot gameplay
on infrastructure that cannot scale with gameplay, the design is wrong, not the
infrastructure.

## Dimensions

Each is evaluated on its own. A system that handles event volume but takes a
week to rebuild an index is not scalable.

| Dimension | The question | Where it lands |
|---|---|---|
| Durable event volume | how many protocol events per day, network-wide? | [`./settlement.md`](./settlement.md), #38 |
| Batch size and commitment cadence | how many events per batch; how often is a commitment produced; what latency to "settled"? | #38, #40 |
| Query volume | how many profile / friend / guild / registry reads per second? | [`./query-and-indexing.md`](./query-and-indexing.md) |
| Realtime connections | how many players are connected at once; how does presence fan out to friends and guild rosters? | [`./presence.md`](./presence.md) |
| Historical volume | how large is the log after 5 / 10 / 20 years? | #40 |
| Rebuild time | how long to reconstruct every projection from genesis? | [`./disaster-recovery.md`](./disaster-recovery.md), #43 |
| Node specialization | can settlement, indexing, realtime, and gateway scale separately? | [`./nodes.md`](./nodes.md) |
| SDK routing | does discovery and failover stay cheap as node count grows? | [`./sdk.md`](./sdk.md), #91 |

## Back-of-envelope (assumptions, not measurements)

Every number below is an assumption to be replaced by data. The point is the
shape, not the digits.

- **Events.** Suppose an active player generates 1 durable event a day on
  average (an achievement, a guild action, a binding) and 10% of the 100M are
  active daily: ~10M events/day, ~115/s. Bursty (a raid completes, a tournament
  ends) but nowhere near gameplay rates. A batch of 10,000 events committed every
  minute would be ~1,000 commitments/day regardless of backend.
- **History.** At ~1 KB per event, 10M events/day is ~10 GB/day, ~3.6 TB/year,
  ~36 TB at ten years before compaction or pruning of payloads that a schema
  later marks derivable. This is what makes rebuild time and mirror cost
  first-class concerns and why not every convenience field goes in the log.
- **Reads.** Reads dwarf writes by orders of magnitude and are entirely the
  indexer's problem; they never touch settlement. Indexers partition by domain
  (profiles, guilds, registry) or by shard before settlement ever needs to.
- **Presence.** 10M concurrently online players is a fan-out problem for the
  realtime vertical alone; nothing about it touches history. Losing a realtime
  node costs nothing durable.
- **Rebuild.** If replay sustains 50,000 events/s on one indexer, ten years of
  history (~36B events at the rate above) is roughly eight days. Snapshots plus
  incremental replay, and per-domain projections rebuilt in parallel, are the
  obvious levers — and the reason `Indexer::rebuild` must be parallelizable by
  domain, not a single serial loop, before the log is large.

## Failure questions

Asked of every design, with the intended answer:

- **A node disappears** — the SDK routes elsewhere; nothing durable is lost
  (scenario K, [`./nodes.md`](./nodes.md)).
- **A region disappears** — realtime presence in that region lapses; reads are
  served by indexers elsewhere; settlement is mirrored.
- **A database is lost** — projections rebuild from the log (scenario J,
  [`./disaster-recovery.md`](./disaster-recovery.md)).
- **The settlement log stalls** — events queue in the buffer; projections
  keep serving; commitments resume once the settlement operator does. No
  validator set to stall in the first place ([ADR #186](https://github.com/LunarVagabond/avalon-protocol/issues/186)).

## Scenario L — 1,000 games, 100M players

Does Avalon avoid becoming a gameplay bottleneck? Yes by construction, as long
as the hot/durable line holds. The parts that do scale with population — event
volume, history size, read volume, presence fan-out — each have an independent
lever, which is what the three verticals and the node roles exist to provide.

## Today in the repo

- Nothing is load-tested. Milestone 1 is one `avalon-server` process, one
  Postgres database, one ledger row per event, no batching
  (`crates/chain/src/postgres.rs`), no indexer implementation, no realtime
  service.
- The trait boundaries (`SettlementProvider`, `Indexer`) are the only scaling
  affordances that exist; they are the right ones.

## Decisions and tickets

- [#38](https://github.com/LunarVagabond/avalon-protocol/issues/38) batching
- [#40](https://github.com/LunarVagabond/avalon-protocol/issues/40) log
  structure and mirror sync (history size, verification cost)
- [#79](https://github.com/LunarVagabond/avalon-protocol/issues/79) /
  [ADR #93](https://github.com/LunarVagabond/avalon-protocol/issues/93) backend
  decided (Avalon's own chain); throughput, cost over decades, and stall
  behavior are now #40's consensus-design criteria
- [#43](https://github.com/LunarVagabond/avalon-protocol/issues/43) rebuild
- [#91](https://github.com/LunarVagabond/avalon-protocol/issues/91) discovery
  and failover
- [#78](https://github.com/LunarVagabond/avalon-protocol/issues/78) presence is
  its own vertical
