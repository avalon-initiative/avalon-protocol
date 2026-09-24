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
| Durable event volume | how many protocol events per day, network-wide? | [`./settlement.md`](./settlement.md) |
| Batch size and commitment cadence | how many events per batch; how often is a commitment produced; what latency to "settled"? | [`./settlement.md`](./settlement.md) |
| Query volume | how many profile / friend / guild / registry reads per second? | [`./query-and-indexing.md`](./query-and-indexing.md) |
| Realtime connections | how many identities are connected at once; how does presence fan out to friends and guild rosters? | [`./presence.md`](./presence.md) |
| Historical volume | how large is the log after 5 / 10 / 20 years? | [`./settlement.md`](./settlement.md) |
| Rebuild time | how long to reconstruct every projection from genesis? | [`./disaster-recovery.md`](./disaster-recovery.md) |
| Node specialization | can settlement, indexing, realtime, and gateway scale separately? | [`./nodes.md`](./nodes.md) |
| SDK routing | does discovery and failover stay cheap as node count grows? | [`./sdk.md`](../../sdks/architecture/sdk.md) |

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
  validator set to stall in the first place.

## Scenario L — 1,000 integrators, 100M identities

Does Avalon avoid becoming a gameplay bottleneck? Yes by construction, as long
as the hot/durable line holds. The parts that do scale with population — event
volume, history size, read volume, presence fan-out — each have an independent
lever, which is what the three verticals and the node roles exist to provide.

## Resource limits

Nothing above touches per-process safety under load — a single
`avalon-server` instance still needs floors that stop it from falling over
under a burst, independent of whatever the durable-history scaling story
eventually becomes. Four independently-configurable limits are enforced in
`avalon-server` itself, every one defaulted so an unconfigured node is
exactly as safe as it always was: `AVALON_MAX_DB_CONNECTIONS` (Postgres pool
size), `AVALON_MAX_CONCURRENT_REQUESTS` (`tower::limit::ConcurrencyLimitLayer`
— backpressures, never drops), `AVALON_RATE_LIMIT_PER_MINUTE`
(`tower_governor`, GCRA, a per-IP ceiling keyed by peer address only) with
`AVALON_PRINCIPAL_RATE_LIMIT_PER_MINUTE` as the per-verified-principal limit
underneath it — always `429` + `Retry-After`, never a silent drop or a
generic `500`, and `AVALON_OUTBOX_POLL_INTERVAL_SECS` (drain cadence). See
[`./nodes.md`](./nodes.md) for the exact defaults and where each is wired.

## Current implementation

Nothing is load-tested at real scale. The current deployment shape is one
`avalon-server` process, one Postgres database (with node-role extraction
available for splitting Settlement/Indexer/Realtime/Gateway — see
[`nodes.md`](./nodes.md)). Batching, a Merkle root, and Signed Tree Heads are
real (`crates/chain/src/postgres.rs`'s `ledger_batches`,
`crates/chain/src/merkle.rs`, `crates/protocol/src/sth.rs`) — one ledger row
per event, closed over into batches rather than committed one at a time.
Commit and proof-serving cost is O(log n) in total ledger size, not O(n)
(`crate::incremental_merkle`) — see [`settlement.md`](./settlement.md).
`PostgresIndexer` is a real indexer, and `presence.rs` runs a real WebSocket
realtime service.

The trait boundaries (`SettlementProvider`, `Indexer`) are the load-bearing
scaling affordances.

**Rebuild-time baseline.** `crates/server/tests/rebuild_from_events.rs` logs
the wall-clock cost of its own rebuild each run: on the order of tens of
milliseconds for its small fixture ledger (a handful of identities/friend/
guild actions) on this environment's Postgres. That is a tiny synthetic
fixture, not a real-scale measurement — it exists so a future run at a much
larger ledger size has a first data point to compare against, not a capacity
claim. The whole rebuild runs in one transaction
(`PostgresIndexer::rebuild_from_scratch`), so wall-clock time scales with
total ledger size, not just the new-entries-since-last-rebuild count — see
[`disaster-recovery.md`](./disaster-recovery.md) for why a cheaper
incremental/checkpointed rebuild isn't built yet.
</content>
</invoke>
