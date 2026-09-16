# Scalability

The stress model is deliberately large. **1,000 integrators × 100,000 identities each =
100,000,000 identities.** That is **not** 100 million gameplay events per second
flowing through Avalon — gameplay stays integrator-side, always. The question is how
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
| Realtime connections | how many identities are connected at once; how does presence fan out to friends and guild rosters? | [`./presence.md`](./presence.md) |
| Historical volume | how large is the log after 5 / 10 / 20 years? | #40 |
| Rebuild time | how long to reconstruct every projection from genesis? | [`./disaster-recovery.md`](./disaster-recovery.md), #43 |
| Node specialization | can settlement, indexing, realtime, and gateway scale separately? | [`./nodes.md`](./nodes.md) |
| SDK routing | does discovery and failover stay cheap as node count grows? | [`./sdk.md`](./sdk.md), #91 |

## Back-of-envelope (assumptions, not measurements)

Every number below is an assumption to be replaced by data. The point is the
shape, not the digits.

- **Events.** Suppose an active identity generates 1 durable event a day on
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
- **Presence.** 10M concurrently online identities is a fan-out problem for the
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

## Scenario L — 1,000 integrators, 100M identities

Does Avalon avoid becoming a gameplay bottleneck? Yes by construction, as long
as the hot/durable line holds. The parts that do scale with population — event
volume, history size, read volume, presence fan-out — each have an independent
lever, which is what the three verticals and the node roles exist to provide.

## Resource limits (#287/#363)

Nothing above touches per-process safety under load — a single
`avalon-server` instance still needs floors that stop it from falling over
under a burst, independent of whatever the durable-history scaling story
eventually becomes. #287 decided the shape (four independently-configurable
limits, all enforced in `avalon-server` itself, every one defaulted so an
unconfigured node is exactly as safe as it always was); #363 is the
implementation, real today: `AVALON_MAX_DB_CONNECTIONS` (Postgres pool
size), `AVALON_MAX_CONCURRENT_REQUESTS` (`tower::limit::ConcurrencyLimitLayer`
— backpressures, never drops), `AVALON_RATE_LIMIT_PER_MINUTE`
(`tower_governor`, GCRA, keyed by integrator key id with an IP fallback for
pre-auth endpoints — always `429` + `Retry-After`, never a silent drop or a
generic `500`), and `AVALON_OUTBOX_POLL_INTERVAL_SECS` (drain cadence). See
[`./nodes.md`](./nodes.md)'s own "Today in the repo" for the exact defaults
and where each is wired.

## Today in the repo

- Nothing is load-tested. Milestone 1 is one `avalon-server` process, one
  Postgres database. Batching (#38), a Merkle root, and Signed Tree Heads
  are real (`crates/chain/src/postgres.rs`'s `ledger_batches`,
  `crates/chain/src/merkle.rs`, `sth.rs`) — one ledger row per event, closed
  over into batches rather than committed one at a time. Commit and
  proof-serving cost is O(log n) in total ledger size, not O(n)
  (`crate::incremental_merkle`, #349) — see `settlement.md`'s "Today in the
  repo". `PostgresIndexer`
  (#42) is a real indexer, and `presence.rs` runs a real WebSocket realtime
  service — both still in-process with settlement, not split onto their own
  nodes yet.
- The trait boundaries (`SettlementProvider`, `Indexer`) are the only scaling
  affordances that exist; they are the right ones.
- **Rebuild-time baseline (#43).** `crates/server/tests/rebuild_from_events.rs`
  logs the wall-clock cost of its own rebuild each run: ~21ms for its
  16-ledger-entry fixture (a handful of identities/friend/guild actions) on
  this environment's Postgres. That is a tiny synthetic fixture, not a
  real-scale measurement — it exists so a future session re-running this
  test at a much larger ledger size has a first data point to compare
  against, not a capacity claim. The whole rebuild runs in one transaction
  (`PostgresIndexer::rebuild_from_scratch`), so wall-clock time will scale
  with total ledger size, not just the new-entries-since-last-rebuild
  count — see "What breaks today" in `disaster-recovery.md` for why a
  cheaper incremental/checkpointed rebuild isn't built yet.

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
- [#287](https://github.com/LunarVagabond/avalon-protocol/issues/287)
  decided (hoster-configurable resource limits — see the section above),
  implemented by
  [#363](https://github.com/LunarVagabond/avalon-protocol/issues/363)
