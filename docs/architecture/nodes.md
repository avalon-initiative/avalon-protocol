# Nodes

An Avalon node is infrastructure that transports, indexes, settles, and serves
protocol data. **Nodes are infrastructure providers, not authorities.** **A node
cannot fabricate an issuer's claim or replace an actor's signature.** **Node
capabilities are roles an operator chooses to run, not mandatory separate
binaries.**

## Capabilities

```text
Settlement   — validates and stores durable history (the log)
Indexer      — consumes history, serves query projections
Realtime     — presence and ephemeral connections
Gateway/API  — the SDK/API surface games and clients talk to
```

An operator may run all four in one process, only Indexer + Gateway, only
Settlement, or any other combination. The verticals in
[`./overview.md`](./overview.md) map onto these roles one to one; the point of
keeping the verticals distinct in code is that this specialization is possible
later without a rewrite.

| Node type | Runs | Typical operator |
|---|---|---|
| Settlement / full | the log, verification, mirror sync | a mirror operator, the reference deployment |
| Indexer | projections, registry, historical queries | a hosting provider serving reads |
| Realtime | presence, heartbeat, ephemeral channels | a regional presence service |
| Gateway / API | SDK endpoints, auth, routing | anyone fronting the others |
| Combined | any subset, including all | milestone 1: one `avalon-server` |

### Settlement retention tiers

Not every Settlement node is expected to store and serve *all* durable
history ([#180](https://github.com/LunarVagabond/avalon-protocol/issues/180),
decided; [#208](https://github.com/LunarVagabond/avalon-protocol/issues/208),
implemented). The commitment (the hash-chained/committed log itself) stays
small and permanent on every Settlement node regardless of tier — retention
tiering applies only to the raw signed event *bodies* backing each
commitment:

| Retention tier | Retains | Typical operator |
|---|---|---|
| Full / archive | complete history, no window | a dedicated archive operator, willing to carry long-term storage cost |
| Hot | a configurable recent window only (e.g. "last N months") | a normal Settlement node optimized for current read/serve traffic |

A hot-tier node may discard event bodies older than its configured window
only when the network as a whole still guarantees availability elsewhere
(at least one archive-tier node, or a minimum archive-replication factor) —
never discard something nothing else retains, and never affect the
commitment's own verifiability either way. Exact window defaults and the
minimum archive-replication factor are implementation-ticket-level numbers,
set per-deployment rather than fixed by the protocol.

**Implemented, `crates/chain/src/retention.rs`.** A node declares its tier
via environment configuration, the same pattern `AVALON_NETWORK_ID`/
`AVALON_SETTLEMENT_SIGNING_KEY` already use:

- `AVALON_RETENTION_TIER` — `full` (default) or `hot`.
- `AVALON_RETENTION_HOT_WINDOW_DAYS` — required when `hot`; the window is a
  count of days of `ledger_entries.committed_at` history, not "last N
  entries" — an operator reasoning about "keep 6 months" maps directly to
  this without needing to know the current event rate.
- `AVALON_RETENTION_PRUNING_ENABLED` — a second, independent, off-by-default
  opt-in. Declaring `hot` alone changes nothing by itself; pruning only
  actually runs once this is also explicitly set `true`. `avalon-server`
  prints its resolved tier and pruning state at startup, next to its
  `network_id` line; `avalon prune-ledger [--dry-run]` is the manual/cron
  entry point, and `avalon-server` also runs an in-process hourly worker
  (`crates/server/src/retention.rs`) whenever pruning is enabled.

Pruning only ever `NULL`s out `ledger_entries.payload` (nullable as of the
`0027_ledger_payload_retention` migration) — `seq`, `entry_hash`,
`prev_hash`, `kind`, `issuer`, `subject`, `event_timestamp`, `version`, and
`batch_id` are never touched, which is exactly what keeps a pruned row's
place in both the hash chain and the Merkle tree (which is built entirely
from `entry_hash`, never `payload` — issue #210) fully intact. `avalon
inspect-ledger` reports, per entry and in its summary line, whether it's
looking at a full node or a node with some entries' payloads pruned —
derived from the data itself (`payload_pruned_at`), not from the reading
process's own config, so it stays accurate against any database it's
pointed at.

**Milestone-1 honesty**: per "Today in the repo" below, there is currently
exactly one Settlement node/database. #180's availability invariant — never
prune what nothing else retains — has no real archive-tier mirror to be
checked against yet. `AVALON_RETENTION_PRUNING_ENABLED=true` today means
real, permanent data loss for anything outside the configured window, not
"safely available elsewhere" — `.env.example` and `crates/chain/src/retention.rs`'s
module doc comment both say this plainly rather than letting the mechanism
imply a safety guarantee the network doesn't yet provide. The mechanism
itself (config, pruning query, what never gets touched) is built to be
correct once an archive-tier mirror actually exists; only the network-wide
guarantee it should ultimately be gated on is still missing.

**Settlement-state checkpoint.** #180 also asked for a periodic
durable-state checkpoint so a hot-tier node, or any new node, can bootstrap
from "latest trusted snapshot + subsequent history" instead of full replay
from genesis. For the commitment layer, this already exists and needed no
new storage: the latest `SignedTreeHead` (`signed_tree_heads`, issue #210)
*is* that checkpoint — `(tree_size, root_hash)` at a known, signed log
height, produced automatically every batch commit, exposed as
`PostgresSettlementProvider::checkpoint()` (an explicitly-named alias over
`latest_signed_tree_head`) and over HTTP via `GET /ledger/sth/latest`
(issue #211). A bootstrapping node trusts this signed head as the root for
everything at or before its `tree_size`, then only needs to hold or replay
`ledger_entries` newer than its own configured window. **Not covered**: an
indexer/projection *read-model* snapshot — `crates/indexer` has real
projections now (issue #42), but no rebuild-speed work or snapshot format
for them exists yet, so that half of #180's ask remains an open follow-up
(issue #43, `docs/architecture/disaster-recovery.md`), not silently
resolved here.

## Node authority

A hosted node is not protocol authority. The concrete guarantees:

- A node **cannot fabricate** "Game A issued this achievement". Attestations are
  signed by Game A's registered issuer key
  ([`./games-and-issuers.md`](./games-and-issuers.md)); a node that stores an
  unsigned or wrongly-signed claim has stored something every verifier rejects.
- A node **cannot replace** a signature, alter a settled entry, or drop one
  without detection — the log is hash-chained and, once
  [#40](https://github.com/LunarVagabond/avalon-protocol/issues/40) lands,
  signed and mirrorable.
- A node **cannot act as a player**. Identity mutations are authorized by the
  player's own key ([`./identity.md`](./identity.md),
  [#73](https://github.com/LunarVagabond/avalon-protocol/issues/73)).
- Operator actions that do exist (suspending an issuer at the network level) are
  explicit, audited protocol events with their own trail — never silent edits.
  [`./security-model.md`](./security-model.md) covers the full authority map.

What a node *can* do is the ordinary work of infrastructure: accept, validate,
order, store, index, serve, and mirror.

## Mirrors, not federation

Multiple operators is a goal. It is achieved by **mirroring one public,
verifiable log** — the Certificate Transparency pattern
([#70](https://github.com/LunarVagabond/avalon-protocol/issues/70)) — not by
federation. Under federation, whether Game B can see a player's identity would
depend on which servers Game B's server peers with; that recreates the walled
gardens Avalon exists to remove. Under mirroring, a client does not pick "which
server to trust": any mirror that misrepresents the log is detectable, because
the log is self-verifying. The log *is* Avalon's own chain, not an anchor into someone else's
([#79](https://github.com/LunarVagabond/avalon-protocol/issues/79), closed;
[ADR #93](https://github.com/LunarVagabond/avalon-protocol/issues/93)), and
that does not change this.

## Discovery

A game developer should not need to know `postgres://...` or
`http://node-37.example.com`. The SDK should eventually resolve a node itself:

```rust
let avalon = Avalon::connect().await?;
```

Selection criteria over time: latency, geographic proximity, availability,
protocol version, advertised capabilities, health, settlement support, operator
preference, and, later, operator reputation. Version and capability negotiation
happen on connect; failover and retry live inside the SDK
([`./sdk.md`](./sdk.md)). Self-hosting stays possible without any central
registry — `connect_to(url)` remains for local development and private
deployments. A node mirroring the public network and a private, disconnected
instance both "self-host" the same code — they are not the same thing; see
[`./self-hosting.md`](./self-hosting.md).

**Scenario K — a node disappears.** The SDK routes to another node advertising
the needed capabilities. Durable history is unaffected (it is mirrored);
presence for players on that node lapses until their next heartbeat
([`./presence.md`](./presence.md)); nothing a game had already verified becomes
unverifiable.

## Today in the repo

- Exactly one node type exists: `avalon-server` (`crates/server/src/main.rs`)
  running Gateway + Settlement (via `PostgresSettlementProvider`) in one
  process. No indexer implementation, no realtime service, no mirror.
  **This is also why the retention-tier mechanism above cannot yet
  deliver #180's actual availability guarantee** — there is only one
  database for a hot-tier node's pruning to be gated against, not a
  second archive-tier node — see this section's own honesty note.
- No discovery: `AvalonConfig { server_url, .. }` in `crates/sdk/src/lib.rs`
  takes a URL.
- No node-to-node protocol, no export format for the log, no capability
  advertisement endpoint.
- No distinct "archive" node *type*/binary exists, and #208 deliberately
  didn't invent one: retention tier is operational configuration on the
  one existing Settlement role (`AVALON_RETENTION_TIER=full`), not a fifth
  capability alongside Settlement/Indexer/Realtime/Gateway in the table
  above. An "archive operator" today is simply an operator running
  `avalon-server` with `AVALON_RETENTION_TIER=full` (the default) and
  `AVALON_RETENTION_PRUNING_ENABLED` left unset — nothing about the
  capability table changes; only the retention-tier config differs between
  a full and a hot deployment of the same Settlement role.

## Decisions and tickets

- #70 mirrors of a public log, not federation
- #79 long-term settlement backend; #186 decided no blockchain/validator
  consensus (transparency log on Postgres instead, superseding part of #93);
  #40 log design and mirror sync (validator/consensus design dropped)
- #180 (decided) / [#208](https://github.com/LunarVagabond/avalon-protocol/issues/208)
  (implemented) — node-tiered durable history retention: retention-tier
  config, payload pruning, the settlement-state checkpoint. The
  indexer-projection-snapshot half of #180's ask stays open, tracked under
  #43.
- [#91](https://github.com/LunarVagabond/avalon-protocol/issues/91) SDK node
  discovery and capability negotiation
- [#72](https://github.com/LunarVagabond/avalon-protocol/issues/72) TLS before
  any non-local deployment
- [#78](https://github.com/LunarVagabond/avalon-protocol/issues/78) realtime is
  its own vertical and can become its own node
