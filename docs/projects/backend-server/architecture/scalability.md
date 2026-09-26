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

## Load testing

`scripts/load-tests.sh` (`make load-test`) starts its own isolated
`avalon-server` processes on `127.0.0.1`, each with a private Postgres schema
that is dropped afterwards, and drives them with `crates/loadtest`, a small
Rust load generator. The generator refuses any target that is not a loopback
address, so it cannot reach a shared node. Usage and environment variables are
in [`docs/maintainers/local-development.md`](../../../maintainers/local-development.md);
CI runs the smoke scale on every pull request in a separate `load-smoke` job.

### Methodology

- Every scenario asserts against the limit the script configured the node with
  where one exists, and records measurements where none does. Sessions are
  seeded directly in the node's schema so authenticated scenarios do not spend
  the limits they are measuring on registration.
- The generator is closed-loop: a fixed number of workers each send their next
  request when the previous one completes, optionally after a think time.
  Throughput is therefore bounded by the workers, the think time and the
  server, not offered at a fixed rate.
- A second client address is a bind to `127.0.0.2`, which the per-address limit
  treats as a different client.
- Server CPU and memory come from `/proc/<pid>` of the server the script
  started, sampled every 250 ms (CPU is percent of one core, so 231 means a bit
  over two cores busy; the generator's own CPU is not included).
- Numbers are the range over three consecutive runs with `LOAD_SCALE=full`,
  except where a single value is given because every run was identical.

Commands used for the numbers below:

```bash
LOAD_SCALE=full LOAD_REPEAT=3 scripts/load-tests.sh                  # every scenario, debug build
LOAD_PROFILE=release LOAD_SCALE=full LOAD_REPEAT=3 \
  scripts/load-tests.sh concurrency db-pool-large sustained         # release build
LOAD_POOL=3 scripts/load-tests.sh db-pool-writes                     # pool size probe (smoke scale)
```

Environment (a dev machine shared with other work, so treat every figure as an
indicative dev-machine number, not a capacity claim): Intel i9-10900K, 20
logical cores, 31 GiB RAM, Linux 7.1. Load average when the debug-profile
runs started was 1.9/2.5/8.1 (1/5/15 minutes) and the 1-minute average stayed
between 0.6 and 3.1 across them; the release-profile runs started at
10.4/4.8/4.8 and settled below 1.2. Postgres was a separate host on the LAN
(round trips include a network hop), the server and the generator shared the
machine, and the default profile is the unoptimized debug build, which is why
the release profile is reported separately for the throughput scenarios.

### Limit validation

| Scenario | Configured | Measured (3 runs, identical unless a range is given) | Result |
| --- | --- | --- | --- |
| Single-address flood, public route (`GET /nodes/status`) | per-address ceiling 600/min | 599 of 920 admitted, then `429` with `Retry-After` on every rejection; a spoofed `X-Forwarded-For` did not escape the limit; a client at another address got `200` | limit holds, others unaffected |
| Single-address flood, authenticated route (`GET /me`), per-identity limit | 600/min | 601 admitted, 319 `429`s with `Retry-After`; a second identity from the same address got `200` | limit holds, others unaffected |
| Same, per-address ceiling | ceiling 600/min | 601 admitted, 319 `429`s; the same identity from another address got `200` | limit holds |
| 305 identities from one address, 4 requests each | ceiling 600/min | 601 admitted, 619 `429`s; no identity came near its own limit | the per-address layer bounds it |
| Announce flood, one source, 50 distinct fabricated URLs | 10 new URLs per source per minute | exactly 10 admitted, 40 `429` `rate_limited` with `Retry-After`; refreshing a known URL and a second source address were still accepted | limit holds |
| Announce flood, fabricated URLs, cap | peer table cap 50, source limit off | 200 of 200 announces answered `200` (about 5,000/s on the debug build); table size 50; oldest entries evicted, newest present | cap holds |
| Announce of forbidden addresses | private peers not allowed | private, loopback and metadata addresses `403 base_url_not_allowed`; userinfo and non-http schemes `400 invalid_base_url`; a 300-character host `400 base_url_too_long`; table stays empty | validation holds |
| Three-node mesh, one node flooded with 300 fabricated URLs | cap 60 on each node | flooded node 60 entries (58 fabricated, both real peers kept); the other two 42 each (40 fabricated) after 8 s; every node kept serving | bounded; numbers predate the unverified-pool fix below — a re-run would show the two non-flooded nodes' main tables unaffected, since gossip no longer reaches them |
| Concurrency cap 4, 64 workers on `GET /me` | `AVALON_MAX_CONCURRENT_REQUESTS=4` | 68,800 to 69,300 requests in 20 s, all `200`: no drops, no `5xx`, no `429`; a well-behaved client's p50 rose from 7.2-8.3 ms (idle, 20 ms apart) to 18.4-18.6 ms (2.2x to 2.6x) and p99 stayed near 20 ms | backpressure, not drops |
| DB pool 10, 96 workers, mixed 50% writes / 30% history / 20% search | `AVALON_MAX_DB_CONNECTIONS=10` | 3,170 to 3,540 requests/s, all `200`; pool 10 of 10 in use mid-load and 0 after; p50 26-28 ms, p99 38-49 ms | queues on the pool without errors |
| DB pool 2, reads only | pool 2 | 1,650 to 1,665 requests/s, all `200`; p50 49 ms, p99 75-77 ms | queues without errors |
| DB pool 2, writes | pool 2 | every worker stalled until the pool acquire timeout: 96 `500` responses at about 30 s in each of three runs, 1,050 to 1,630 requests completed before that; the node recovered and served normally afterwards | fails: see the gaps below |

Probing the pool size for writes (smoke scale, 32 workers, one run each): pool
3 completed 2,673 requests with no errors (661 requests/s), pool 4 2,933 (727
requests/s), pool 6 4,085 (1,016 requests/s); only pool 2 stalled.

### Baseline throughput and resources

Sustained mixed profile: 64 workers with 20 ms think time for 60 s against a
node with default limits except that the per-address and per-identity limits
are raised out of the way; 200 identities; mix of 40% `GET /me`, 15% `GET
/nodes/status`, 15% `GET /me/history`, 10% `GET /identities/search`, 10% `PUT
/me/presence`, 10% `PATCH /me`. The rate is bounded by the workers and think
time (about 64 / (0.020 s + service time)), so it is offered load, not a
ceiling.

| Profile | Throughput | p50 | p95 | p99 | Errors | Server CPU (one core = 100) | Server RSS |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Debug build | 2,850 to 2,852 requests/s | 1.51-1.52 ms | 4.13-4.20 ms | 5.53-5.92 ms | 0 of about 171,000 | avg 231-233, peak 463-526 | 61-63 MB peak |
| Release build | 2,923 to 2,928 requests/s | 0.74-0.75 ms | 2.93-3.10 ms | 4.10-4.88 ms | 0 of about 175,500 | avg 49, peak 60-72 | 39-40 MB peak |

Per route on the release build the medians are about 0.1 ms for
`/nodes/status`, 0.4-1.0 ms for authenticated reads and presence, and about 3
ms for `PATCH /me` (a transactional write with an outbox row).

Closed-loop saturation with no think time gives the capacity of the
session-authenticated read path on this machine: about 3,450 requests/s on the
debug build and 6,225 to 6,235 requests/s on the release build (`GET /me`, 64
workers, concurrency cap 4), and 4,330 to 4,750 requests/s for the mixed
read/write profile on the release build (96 workers, pool 10). The server used
about 120% of one core at 6,200 requests/s on the release build, so the
generator, the shared machine and the Postgres round trip are as likely a
limit as the node itself.

### Slow connections and oversized requests

- **Slow connections.** 150 connections each of three kinds were held for 75
  seconds: idle (nothing sent), slow headers (a request line and one header
  byte per second, never terminated), and slow body (complete headers with a
  declared 100,000-byte body, one byte per second). Against default node
  settings, the server closed none of them in any of three runs and returned
  no response to any — the only bound on held connections was the process's
  file descriptor limit. `AVALON_HTTP_HEADER_READ_TIMEOUT_SECS` (default 30)
  now closes the idle and slow-header cases: it's hyper's own HTTP/1
  `header_read_timeout`, which also re-arms between keep-alive requests, so
  it covers "never sends a byte" and "already served a request, now sitting
  idle" the same way. `AVALON_HTTP_REQUEST_TIMEOUT_SECS` (default 30) closes
  the slow-body case with a `408`, bounding request-body read plus handler
  processing; a WebSocket upgrade handler returns almost immediately (the
  socket moves to a spawned task), so live `/ws/*` connections are
  unaffected by either timeout. Well-behaved requests made while slow
  connections are held open were served with a p50 of 2.4-2.5 ms and are
  unaffected by the new timeouts.
- **Body size.** Requests were cut off at axum's implicit default JSON body
  limit of 2 MiB: a 2,047 KiB body reached the handler, a 2,049 KiB body got
  `413`, and 8 MiB and 64 MiB bodies got `413` (public `POST /nodes/announce`
  and authenticated `PATCH /me` alike). That 2 MiB ceiling is now explicit
  and configurable (`AVALON_HTTP_MAX_BODY_BYTES`), and node-coordination
  routes (`/nodes/announce`, `/nodes/probe`, `/nodes/trace`,
  `/mirror/notify`, `/nodes/log-level`) — small, shape-fixed JSON, never
  arbitrary user content — get their own explicit, much smaller default of
  256 KiB (`AVALON_NODE_COORDINATION_MAX_BODY_BYTES`) instead of sharing the
  2 MiB general ceiling. Server RSS stayed at 58-60 MB across all of it.
- **Header size.** A single header value of 64 KiB was accepted (`200`) and 1
  MiB got `431`. A 16 KiB request target was accepted, 64 KiB got `414` and 1
  MiB got `431`. 400 headers got `431`. These are hyper's defaults and stay
  unconfigured — they were not the finding either #894 or #896 acted on.

### Known gaps

- `AVALON_MAX_DB_CONNECTIONS=2` stalls the whole node under concurrent
  writes: all in-flight requests wait for the 30 s pool acquire timeout and
  fail with `500`, then the node recovers. Three connections and above worked
  in every test. The stall points at a write path needing two connections at
  once, or at the outbox drain competing with writers for a pool that small;
  the cause is not isolated.
- ~~Announce gossip carries fabricated entries to neighbors~~ — fixed: a
  gossip-relayed entry no longer lands in a neighbor's main peer table at
  all. It goes into a separate, smaller unverified pool (fixed at 256
  entries, independent of the table's own cap) and is promoted into the main
  table only if it announces itself directly or this node successfully
  contacts it; an entry that never gets promoted ages out of the pool on the
  same expiry rule as the table. In the original mesh test a hostile node's
  40 fabricated entries per neighbor within 8 seconds occupied real table
  slots (bounded, never displacing bootstrap/active peers, but still
  occupying slots until they expired); the same flood now never touches the
  neighbors' main tables, only their unverified pools. See "Peer table
  bounds" in `nodes.md`.
- The request-processing limits are per process. Loopback runs cannot show
  behavior behind a real proxy chain, real network latency, TLS, or a
  multi-host mesh larger than three nodes.

### Tuning guidance for hosters

- Keep `AVALON_MAX_DB_CONNECTIONS` at 4 or more (default 10) and below the
  Postgres server's `max_connections` divided by the number of node processes;
  a request waits for a connection, so a pool smaller than the offered
  concurrency raises latency rather than producing errors, until it is small
  enough to stall writes.
- `AVALON_MAX_CONCURRENT_REQUESTS` (default 256) only queues; it does not
  reject. Lower it to protect Postgres, and expect latency for every client to
  rise roughly in proportion to workers divided by the cap.
- `AVALON_RATE_LIMIT_PER_MINUTE` is the per-address ceiling and the only
  defense against anonymous floods; behind a reverse proxy set
  `AVALON_TRUSTED_PROXIES` so clients are distinguished, otherwise every client
  shares the proxy's bucket. `AVALON_PRINCIPAL_RATE_LIMIT_PER_MINUTE` bounds a
  single identity independently.
- The node now closes idle/slow-header/slow-body connections itself
  (`AVALON_HTTP_HEADER_READ_TIMEOUT_SECS`, `AVALON_HTTP_REQUEST_TIMEOUT_SECS`);
  a reverse proxy's own timeouts are still worth setting as a second layer,
  not a required substitute. Raise the process file descriptor limit to
  match the connection count you accept.
- `AVALON_NODE_MAX_KNOWN_PEERS` bounds memory used by the peer table; the
  per-source announce limit (default 10 new URLs per minute) is what slows
  fabricated announces, and a node that does not need private peers should
  leave `AVALON_ALLOW_PRIVATE_PEERS` off.
- Use a release build: on the same workload it used about a fifth of the CPU
  and two thirds of the memory of the debug build.

## Current implementation

The limits and a single node's behavior under load are measured (see [Load testing](#load-testing)); a real-scale, multi-host deployment has not been. The current deployment shape is one
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
