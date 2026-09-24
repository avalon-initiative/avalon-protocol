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
Gateway/API  — the SDK/API surface integrators and clients talk to
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
| Combined | any subset, including all | the default: one `avalon-server` |

**Config knob**: `AVALON_NODE_ROLES` (comma-separated, e.g.
`settlement,indexer`) — see `crates/server/src/nodes.rs::node_roles`.
Unset defaults to `combined`. This value is genuinely load-bearing, not just
advisory metadata: a roles list excluding `indexer` (and not `combined`) routes
indexer reads/writes to a remote one instead of running a local
`PostgresIndexer`; a roles list excluding `realtime` stops this process from
handling `/ws/presence`/`/ws/messages` locally, proxying those connections
through to a configured remote Realtime node instead; a roles list resolving to
*exactly* `["settlement"]` (`crates/server/src/nodes.rs::is_settlement_only`)
makes `main.rs` skip wiring up every Gateway-facing module entirely and serve a
reduced route table. The `combined` default still behaves as a single process:
full route table, every module wired up locally.

### A node's three configuration axes are independent

Easy to conflate, especially once a single node holds more than one at
once (see the real cross-machine deployment below) — these are three
separate questions, and a node's answer to one says nothing about its
answer to the other two:

| Axis | Question it answers | Values | Config |
|---|---|---|---|
| **Capability** | What services does this process run? | Settlement / Indexer / Realtime / Gateway / Combined (table above) | `AVALON_NODE_ROLES` |
| **Shard role** (per shard) | Does this node hold the real signing key and author this shard's writes, or does it only watch and independently verify another node's? | Authority (self-hosting.md's "shard operator") / Mirror (self-hosting.md's "mirroring the public network") | Authority: `AVALON_SETTLEMENT_SIGNING_KEY` set to that shard's registered key. Mirror: that shard's URL listed in `AVALON_MIRROR_PEERS` |
| **Retention tier** | How much local history does this node keep? | Full/archive (everything) / Hot (recent window only) | `AVALON_RETENTION_TIER` (see below) |

**Shard role is per-shard, not per-node** — a single node can be the
authority for one shard and a mirror of a completely different one at
the same time (nothing shares storage between the two: authored history
lives in `ledger_entries`, mirrored history in `mirrored_entries`, kept
separate by construction). `AVALON_OWN_SHARD_ID` (default `"core"`)
declares which shard, if any, this node authors — every `/ledger/*` read
has to know this to answer correctly once a node holds both roles.

**A node's ledger is its shard.** Events whose issuer is an identity
(identity, social, guild) are routed to the reserved `core` label, but a node
with no `AVALON_SETTLEMENT_REMOTE_URL(S)` entry for that label commits them to
its own local ledger. Whatever a node commits locally is therefore the history
of the shard named by its `AVALON_OWN_SHARD_ID`, signed with its
`AVALON_SETTLEMENT_SIGNING_KEY`.

**`core` is the reserved label of the network's pinned core authority**, the
one node whose settlement key is the `verify_key` pinned for the network in
`docs/trusted-networks.json`. Every other node authors a named, registered
shard (`game:<slug>`, `app:<slug>`, `service:<slug>`) and signs with a
`shard_settlement` key registered for that integrator. A node that leaves
`AVALON_OWN_SHARD_ID` at its `core` default while holding a different key
would become a second author of `core`; clients pinned to the network key
would see its tree heads as a mismatch, indistinguishable from an impostor.

**Startup guard.** When `AVALON_OWN_SHARD_ID` is `core` and the network has a
pinned anchor, the server compares its own settlement verify key with the
pinned one. On a mismatch it refuses to start when the anchor's `environment`
is not `local-dev`, or when `AVALON_BOOTSTRAP_PEERS`/`AVALON_MIRROR_PEERS` is
set (joining an existing network is never a legitimate second `core` author).
A lone `local-dev` node with no peers logs a warning and continues. The result
is reported as the optional `core_author_pinned` field of `GET /nodes/status`
(`true` pinned, `false` tolerated mismatch, absent when the node is not a
`core` author or its network has no anchor). See
[`../for-hosters/choosing-your-shard.md`](../for-hosters/choosing-your-shard.md).

**Sibling shards.** A shard id is `{namespace}:{owner}[/{instance}]`, where
`instance` is 1-64 characters of `[a-z0-9-]` starting with `[a-z0-9]`.
Key and authority resolution use `owner` only, so `game:wow/1` and
`game:wow/2` are both authorized by the `shard_settlement` keys of the
integrator `wow`, while remaining separate ledgers with separate tree heads
and mirrors. `AVALON_OWN_SHARD_ID` is validated at startup (`core` or a valid
extended id). Events issued by the integrator itself (attestations,
revocations) still route to `{namespace}:{owner}` without an instance; a
sibling node receives them only if that node is the configured remote
authority for that exact shard id.

**The core authority is the trust root registrar.** A shard's authority is the
integrator's `shard_settlement` key, recorded as an event in the core
authority's ledger. Registering a new integrator and its shard key is a
request to the core authority; a new shard cannot be trusted by clients until
the core authority has recorded both.

**Real example, live in this sandbox**: a second, physically separate node
runs Combined-capability (same as every node), Full-tier, and holds *both*
shard roles — mirror of the primary's `core` shard, and authority for its
own separate `game:peer-shard-demo-*` shard. See
[`./settlement.md`](./settlement.md)'s "Cross-machine, real end to end"
section for the full write-up.

### Backing-service discovery for a Gateway process

Once a process is configured to *not* run Indexer/Realtime/Settlement locally,
it needs a way to find which backing-service URLs to use. Deliberately
**static config, not a dynamic service registry** — a fixed deployment doesn't
need discovery more elaborate than "an operator wrote the URL down."

| Role | Env var | Shape | Required when role excluded, and no local role either? |
|---|---|---|---|
| Indexer | `AVALON_INDEXER_REMOTE_URL` (+ `AVALON_INTERNAL_ROLE_KEY`) | Single base URL | Yes — hard startup failure if unset or malformed |
| Realtime | `AVALON_REALTIME_URL` | Single base URL | Yes — hard startup failure if unset or malformed |
| Settlement | `AVALON_SETTLEMENT_REMOTE_URL` (singular) / `AVALON_SETTLEMENT_REMOTE_URLS` (per-shard map) | One URL, or a `shard_id=url` map | No — this stays a warning, not a hard failure, since `AppState::chain` is a required field regardless of declared role (a node can always still commit to its own local ledger) |

Indexer and Realtime are each a single, all-or-nothing target: if the role
isn't running locally, there is exactly one place to route that traffic, and
having no valid URL means this process genuinely cannot serve that role at
all — the same "refuse to start rather than silently degrade" posture the
`network_id` genesis-mismatch and DHT config checks already establish.
Settlement is a per-shard *map*, and a node whose declared roles exclude
`settlement` but has no remote authority configured simply keeps committing to
its own local ledger — a real, working, if perhaps unintended, configuration,
not a broken one.

All three vars go through one shared `normalize_and_validate_url` helper (trim
whitespace, strip a trailing slash, reject anything that doesn't parse as a
well-formed URL) in `crates/server/src/backing_services.rs`. What's not
unified is required-vs-optional/hard-vs-soft failure: Indexer and Realtime
each fail hard on a missing/malformed value; the Settlement map skips (with a
log line) any one malformed entry rather than failing the whole process.

Every configured backing-service URL also gets a startup-time reachability
probe (`GET {base_url}/nodes/status`), logged as a warning, never a hard
failure — an unreachable-but-well-formed URL is exactly the shape of problem a
rolling restart produces transiently, and every real request already gets its
own live reachability signal (a `503 RemoteRoleUnreachable`/proxy
failure/remote-submit failure) the moment it actually needs the backing
service.

### Settlement retention tiers

Not every Settlement node is expected to store and serve *all* durable
history. The commitment (the hash-chained/committed log itself) stays
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
minimum archive-replication factor are per-deployment numbers, not fixed
by the protocol.

**Independent of shard role, same as it's independent of capability
(above).** A shard *authority* can be hot or full tier for the shard it
authors; a *mirror* can independently be hot or full tier too (this only
ever governs whether *this node's own* stored copy prunes old payloads —
a mirror's `mirrored_entries` isn't touched by pruning at all).

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
- `AVALON_RETENTION_ARCHIVE_PEERS`/`AVALON_RETENTION_MIN_ARCHIVE_CONFIRMATIONS`
  — a third, independent, off-by-default opt-in that turns "never prune what
  nothing else retains" from operator discipline into something the code
  actually checks. Unset (the default): zero behavior change. Configured:
  before each pruning pass, the node computes the boundary `seq` it's about
  to prune past and asks each listed peer's own `GET
  /ledger/mirror-progress?network_id={id}` (`crate::settlement::mirror_progress`)
  how far it has independently verified and mirrored this node's history —
  pruning only proceeds once at least `AVALON_RETENTION_MIN_ARCHIVE_CONFIRMATIONS`
  (default `1` once peers are configured) distinct peers confirm coverage
  past that boundary; otherwise the pass is skipped and retried next tick.
  Only the background worker performs this check — `avalon prune-ledger`'s
  manual entry point refuses to run a real (non-`--dry-run`) prune at all
  when this is configured, rather than silently skipping the safety gate.
  Live-verified end to end against a real, isolated Postgres database: pruning
  demonstrably blocked while coverage was unconfirmed, then proceeded once it
  was confirmed.
- `AVALON_MIN_MIRROR_CONFIRMATIONS`/`AVALON_MIRROR_GRACE_PERIOD_HOURS`/
  `AVALON_REPLICATION_POLL_INTERVAL_SECS` — extends the mechanism above to a
  different trigger point: not "is it safe for this node to prune its own
  local history," but "does a shard have enough independently-confirmed
  mirrors to accept a brand-new identity registration at all." Always on (no
  opt-in env var; defaults are `1` confirmation, a `24`-hour bootstrap grace
  period, and a 300s poll cadence) — unlike the pruning gate, which only
  matters for an operator who has opted into hot-tier retention, this gate
  protects every identity a shard is about to durably take on, so it applies
  by default rather than requiring configuration to turn on.

  **Mechanism** (`crates/server/src/replication.rs`): a background worker
  polls every known peer's own `GET
  /ledger/mirror-progress?network_id={id}&shard_id={id}` for every known
  shard on a configurable cadence, populating an in-process,
  non-durable count of distinct confirming peers per shard. The gate itself
  (`crate::replication::registration_eligible`, pure, unit-tested) is
  consulted once, at the very start of registration, before the WebAuthn
  ceremony or even uniqueness checks. A shard is eligible for a *new*
  registration when either it's still within its bootstrap grace period, or
  it has at least the configured minimum distinct, currently-fresh confirmed
  mirrors. A shard already past the grace period with too few confirmed
  mirrors gets `AppError::ShardBelowMinimumReplication` (503) — never
  disrupts an identity already registered on the shard, only new
  registrations. A brand-new, legitimately single-operator shard hasn't had
  time to attract a mirror yet, so the grace period exempts it entirely
  while young. A shard whose age can't be determined at all yet (e.g. right
  after this node's own restart) is treated as within the grace period —
  the safe direction to lean. `GET /nodes/status` includes an
  `own_shard_replication` block (confirmed mirror count, minimum required,
  grace-period status, eligibility) so an operator can see this before
  trusting a shard with anything, not just discover it via a failed
  registration attempt.

  One configured minimum applies uniformly to every shard — no per-shard-type
  configuration surface exists yet. Live-verified end to end with multiple
  local `avalon-server` processes against real Postgres: a shard within its
  grace period accepted registration regardless of confirmed-mirror count, a
  shard forced past a near-zero grace period with no confirmed mirrors
  rejected registration, and registration succeeded again once a peer's
  confirmed mirroring was recorded.

Pruning only ever `NULL`s out `ledger_entries.payload` — `seq`, `entry_hash`,
`prev_hash`, `kind`, `issuer`, `subject`, `event_timestamp`, `version`, and
`batch_id` are never touched, which is exactly what keeps a pruned row's
place in both the hash chain and the Merkle tree (built entirely from
`entry_hash`, never `payload`) fully intact. `avalon inspect-ledger` reports,
per entry and in its summary line, whether it's looking at a full node or a
node with some entries' payloads pruned — derived from the data itself
(`payload_pruned_at`), not from the reading process's own config.

**Real cross-machine mirror deployment, live-verified.** A genuine second,
physically-separate node exists in this environment's sandbox and plays a
dual role: it mirrors the primary's core shard (fully converged — its
`mirrored_entries` count matches the primary's real `tree_size` exactly, same
root hash), and independently authors a second, distinct shard with its own
real, registered `shard_settlement` operational key (not a shared
node-operator key). Read availability during a primary outage is live-proven,
not just structural: with the primary process stopped entirely, the mirror's
`GET /ledger/sth/latest` and `GET /ledger/entries` kept answering correctly
from its own independently-verified data. Two things this does not by itself
solve: **write availability** during a primary outage (a mirror-only node
never becomes a new writer/authority on its own — promotion is a manual,
operator-driven runbook ([`authority-promotion.md`](../for-maintainers/authority-promotion.md), which seeds the replacement's ledger from a converged mirror with `avalon promote-mirror`), not automatic failover, deliberately never automatic
election, which would reopen the no-consensus decision) and the harder
multi-writer/consensus question, which remains genuinely open.

**Settlement-state checkpoint.** A periodic durable-state checkpoint lets a
hot-tier node, or any new node, bootstrap from "latest trusted snapshot +
subsequent history" instead of full replay from genesis. For the commitment
layer, this already exists and needed no new storage: the latest
`SignedTreeHead` *is* that checkpoint — `(tree_size, root_hash)` at a known,
signed log height, produced automatically every batch commit, exposed as
`PostgresSettlementProvider::checkpoint()` and over HTTP via `GET
/ledger/sth/latest`. A bootstrapping node trusts this signed head as the root
for everything at or before its `tree_size`, then only needs to hold or replay
`ledger_entries` newer than its own configured window. **Not covered**: an
indexer/projection *read-model* snapshot — `crates/indexer` has real
projections now, but no rebuild-speed work or snapshot format for them exists
yet, so that half stays an open follow-up (see
[`disaster-recovery.md`](./disaster-recovery.md)).

## Node authority

A hosted node is not protocol authority. The concrete guarantees:

- A node **cannot fabricate** "Integrator A issued this achievement". Attestations are
  signed by Integrator A's registered issuer key
  ([`./issuers.md`](./issuers.md)); a node that stores an
  unsigned or wrongly-signed claim has stored something every verifier rejects.
- A node **cannot replace** a signature, alter a settled entry, or drop one
  without detection — the log is hash-chained, signed, and mirrorable.
- A node **cannot act as an identity**. Identity mutations are authorized by the
  identity's own key ([`./identity.md`](./identity.md)).
- Operator actions that do exist (suspending an issuer at the network level) are
  explicit, audited protocol events with their own trail — never silent edits.
  [`./security-model.md`](./security-model.md) covers the full authority map.

What a node *can* do is the ordinary work of infrastructure: accept, validate,
order, store, index, serve, and mirror.

## Mirrors, not federation

Multiple operators is a goal. It is achieved by **mirroring one public,
verifiable log** — the Certificate Transparency pattern — not by
federation. Under federation, whether Integrator B can see an identity would
depend on which servers Integrator B's server peers with; that recreates the walled
gardens Avalon exists to remove. Under mirroring, a client does not pick "which
server to trust": any mirror that misrepresents the log is detectable, because
the log is self-verifying. The log *is* Avalon's own chain, not an anchor into
someone else's, and that does not change this.

Mirroring addresses *read* decentralization — anyone can independently
verify the log without trusting whichever node they happened to ask.
Sharded settlement authority addresses the complementary *write*/control
problem — one operator holding sole settlement authority over the whole
network — by sharding settlement authority per-integrator instead. See
[`settlement.md`](./settlement.md)'s "Cross-shard commitment" section for
how a network with more than one shard still produces one globally
verifiable state with no designated aggregator, mirroring this section's own
"no single trusted party" standard one layer up. An operator running their
own shard (rather than only mirroring) is a third, distinct self-hosting
category — see [`self-hosting.md`](./self-hosting.md)'s "shard operator"
section for exactly how that stays unambiguous from both mirroring and a
disconnected private fork.

## Identity and social actions are not shard-locked

An identity has no "home node" in any operationally ongoing sense, and
this is by construction. Which shard a new event commits into is a property
of *whichever node handles the request* (`AppState::own_shard_id`), never of
the identity acting through it — there is no code path anywhere that requires
an identity's actions to route back through the shard its `identity.created`
event originally landed on. "Home" describes a historical fact (where that
first event is durably committed, forever, by replication) — not a place
an identity depends on continuing to reach.

Concretely: a person can authenticate through any live node via cross-node
login (see below) and their friend/guild/profile actions commit into *that*
node's own shard, exactly as if they'd always used it. If the node/shard
they'd been using disappears mid-session, reconnecting through a different
live node and continuing is the same "reconnect elsewhere" pattern already
proved for realtime chat, extended to authoring durable events generally
rather than only receiving live pushes.

**The real, narrower failure that remains** is a *specific* integrator's own
dedicated settlement shard (their achievements, tournament results —
anything issued under that shard's own authority) going down: that pauses
*that integrator's own* new issuances, a real but contained, per-integrator
blast radius — never a network-wide one, and never something that stops an
identity from acting anywhere else.

Separately, none of this helps if a shard's *data* was never durably copied
anywhere to begin with — a minimum replication guarantee (above) is the
answer for new registrations; extending that guarantee further remains an
open question.

**A pure mirror's "nothing here yet" 404 says so.** Before a mirror node has
backfilled anything for a given shard, `GET /ledger/sth/latest` (and
`/ledger/sth/{tree_size}`) 404 — same as a genuinely empty Settlement
authority that hasn't committed anything either, which would otherwise make
the two indistinguishable to a caller. When the requested shard has a
configured mirror source, the 404 body includes `is_mirror: true` and
`mirror_peers`, naming the peer to ask instead, rather than looking like a
broken/misconfigured node.

## Discovery

A developer should not need to know `postgres://...` or
`http://node-37.example.com`. Every official SDK (Rust, C#, TypeScript) resolves a
node itself, given a target network rather than a URL
(`discover`/`AvalonClient::connect`; Rust shown):

```rust
let avalon = AvalonClient::connect(
    TargetNetwork::NetworkId("avalon-mainnet-1".into()),
    DiscoveryConfig { integrator_credential_key_id, integrator_slug, signing_key, retry },
).await?;
```

Candidates come from `docs/trusted-networks.json`'s `server_url`/`seed_nodes`
for the matching network entry — no separate discovery registry yet, since
that file is already exactly "a published node list" (one of three
plausible discovery sources, alongside a well-known endpoint or DNS). Each candidate is verified the same way an already-known
URL is (`GET /ledger/sth/latest` against the entry's pinned `verify_key`);
the first that verifies wins. Selection is "first that verifies," not yet
ranked by latency, geographic proximity, health, or operator preference —
those remain future refinements once there's more than one anchor node per
network to choose between. Self-hosting stays possible without any central
registry — `AvalonConfig { server_url }` remains for local development and
private deployments, entirely unaffected by `connect()`'s existence. A node
mirroring the public network and a private, disconnected instance both
"self-host" the same code — they are not the same thing; see
[`./self-hosting.md`](./self-hosting.md).

`GET /nodes/discover`
(`crates/server/src/nodes.rs`) is the standalone server-side
discovery endpoint: given one already-verified
node, it returns that node's own `GET /nodes/status` output plus its full
`GET /nodes/peers` table in a single response, so a client that has
reached exactly one node can expand its candidate pool without a second
round trip. Not yet wired into any SDK's `connect()` — each SDK still
only tries `docs/trusted-networks.json`'s static `server_url`/`seed_nodes`
list, one candidate at a time — and still not ranked candidate selection:
`/nodes/discover` hands back a node's raw peer set, not a set ordered by
latency, health, or role. See [`./sdk.md`](../../sdks/architecture/sdk.md)'s
"Known limitations" for the full current-state breakdown.

**Node-to-node announce/bootstrap discovery is real**: `POST
/nodes/announce`/`GET /nodes/peers` (`crates/server/src/nodes.rs`) — a
lightweight, in-memory peer table (`network_id`, roles, protocol version,
last-announced timestamp), keyed by `base_url`, never merged across
`network_id`s. `AVALON_BOOTSTRAP_PEERS` names explicit peers; unset, a node
falls back to its own network's `seed_nodes` in `docs/trusted-networks.json`
(see [`./network-trust-anchors.md`](./network-trust-anchors.md)) — empty for a
network with no anchor node yet, which is the expected state for a network's
first node, not an error. `nodes::run_worker` re-announces on
`AVALON_ANNOUNCE_INTERVAL_SECS` (default 180s) and prunes any peer not
re-announced within a few multiples of that interval. A node's active
announce/exchange peer set grows past its bootstrap list over time —
`run_worker` keeps its own growing `active_peers` list, seeded from the
bootstrap set (never evicted) and extended, capped by `AVALON_NODE_MAX_PEERS`
(default 50), with peers discovered through announce exchanges. Not built:
capability-aware routing — every SDK's zero-URL `connect()` (below)
picks any STH-verified candidate from `docs/trusted-networks.json`, not
the best one by role/latency/health.

**Per-neighbor round-trip latency is measured, per observer.** Each announce
`run_worker` sends to a peer in its active set is timed from request to parsed
response and folded into in-memory rolling stats keyed by `base_url`
(`crates/server/src/neighbors.rs`): `last_ms`, `ewma_ms` (smoothing factor
0.2, seeded by the first sample), `min_ms` and `jitter_ms` (mean absolute
deviation) over the last 20 successful round trips, `samples`, loss as
failed attempts over the last 20 attempts, and `last_success_at`. The value is
an application-level round trip: network plus the peer's request handling, not
ICMP latency. A failed or timed-out attempt (announce requests time out after
15s) counts toward loss and never toward any latency figure. Entries exist only
for peers in the active set and are dropped when a peer leaves it, so memory is
bounded by `AVALON_NODE_MAX_PEERS`; nothing is persisted. The measurement
describes the observer-to-peer path, so it is never gossiped and is not a field
of `PeerInfo`; a peer cannot report its own latency. It is observational only
and never influences admission, pruning, the version floor, or any trust
decision. The announce worker publishes its active set and these stats through
the shared `PeerTable` so read models can serve them.

**Realtime relay and DHT bootstrap consume this peer table.** The realtime
relay (see [`presence.md`](./presence.md)/[`communication.md`](./communication.md))
reads each peer's `roles` to decide who a live presence/chat event gets
forwarded to. A DHT layer (`crate::dht`, `AVALON_DHT_ENABLED`, on by default)
gives a node a libp2p `PeerId` — a fourth independent key domain alongside
player keys, issuer keys, and the settlement log operator's key — and bootstraps
into the DHT swarm using the same peer table (`PeerInfo` carries a peer's
`libp2p_peer_id` and dialable multiaddrs) rather than a second discovery
mechanism.

**Interest registration and lookup over the DHT** (`crate::interest`): a local
guild-channel/conversation subscription holds an interest guard for as long as
it's subscribed, `PutRecord`ing this node's URL under a hash of the
channel/conversation id immediately on a scope's first subscriber and
periodically thereafter for as long as at least one subscriber remains — no
explicit deregister call; a disconnected subscriber's guard drops and the
record lapses. `interest::lookup` is a `GetRecord` against that same key.
`realtime_relay::relay_to_peers` calls this lookup for channel/conversation
events instead of a full peer-table loop, which remains only for presence (no
channel/conversation scope to look up) and as the fallback for any node with
no DHT identity. **A DHT `PutRecord` for a Channel/Conversation scope carries a
signed interest claim** — the same Ed25519 event-signing key session-continuation
tokens use, binding the identity and the destination base URL into the signed
bytes so a claim can't be republished under a different URL to redirect
delivery. The relaying node separately re-checks *current* membership against
its own local, ledger-derived membership tables rather than trusting anything
claimed in the record. The DHT swarm's own protocol id is namespaced by
`network_id`, so a peer configured for a different network can't negotiate a
Kademlia RPC with this swarm at all.

An optional per-hoster Redis fast-path (`AVALON_REDIS_URL`) sits in front of
the interest lookup as a same-fleet shortcut — checked first, never a
replacement for the DHT, and never load-bearing for correctness (an empty or
failed Redis check falls straight through to the DHT).

**Shard-existence gossip rides the same announce mechanism.** `AnnounceRequest`/
`AnnounceResponse` carry a `known_shards` snapshot; a node authoritative for a
shard (real, local, signed settlement history for it) gossips that fact and
every shard it's otherwise learned about to its active peer-exchange partners.
See [`settlement.md`](./settlement.md)'s "Automatic shard discovery" section
for the trust/verification side.

**Push-based mirror sync, on top of polling, not replacing it.** Two tiers:
permissionless (any mirror watches `AVALON_MIRROR_PEERS` with zero
registration, as before) and push-registered (a mirror additionally gets a
low-latency nudge the moment a peer it mirrors commits something new).
Addressing reuses the DHT-backed interest registration above rather than a new
mechanism: the mirror-watcher registers this node's interest in every
`network_id` it successfully verifies an STH for, never an unverified one. The
outbox drain worker, right after a batch it just committed locally, resolves
who's registered for that `network_id` via the same DHT lookup and posts a
small notification to each over plain HTTP (the DHT does the addressing; only
the last hop is HTTP). The notification handler never trusts the body for
anything — it only wakes the mirror-watcher's loop early, which then runs its
exact existing verify/corroborate/backfill pipeline; a missed or dropped push
is never fatal, since the same loop still falls back to its normal poll tick.

**Mirror-watcher verification is per-`network_id`, not one process-wide key.**
Each peer's verify key is resolved from `docs/trusted-networks.json` by its
own claimed `network_id` at verification time — a node can correctly mirror
peers across more than one legitimately pinned network, and a peer claiming
an unpinned `network_id` is refused outright, not silently stored.

**One-hop live realtime relay across nodes**: `POST /nodes/relay`
(`crate::realtime_relay`) — no auth (same posture `GET /nodes/peers` already
takes); single-hop by construction, not by an origin-tracking field. This
reasoning depends on the peer mesh staying small and fully interconnected; a
larger mesh is exactly what the DHT-scoped interest routing above narrows the
targeting to.

**No capability-aware SDK-side routing yet**: the peer table above is a
server-to-server mechanism, not consumed by client-side routing — the
SDKs' `connect()` (above) only pick a verified server to talk to, they
don't route individual calls by role. No export format for the log exists yet either.

## Version rollout

No party can force any operator to upgrade — self-hosting with no central
gatekeeper is deliberate, not a gap — so version handling has to work with
permanent version skew across the network as a constraint. Three separate
axes, conflating them is the actual failure mode: **event-schema version**
(payload shape per event kind, additive-only), **wire/API version** (HTTP
endpoint shapes, mirror-sync DTOs), and **settlement/crypto version** (hash
algorithm, signature scheme, STH/Merkle format — the one axis where mixed
versions on the *same* `network_id` genuinely isn't safe).

**Standing rule for the wire/API axis**: additive-only, forever-compatible.
Never remove, rename, or repurpose a field or endpoint outright; add alongside
and deprecate slowly. This is a code-review discipline expectation, not gated
on any further ticket. One deliberate exception was taken once, before this
repo went public: a terminology generalization renamed request/response field
names and dropped an earlier compatibility route family, made knowingly on the
grounds the rule itself depends on — at the time, every consumer of those
shapes lived in this monorepo and was updated in the same change. Once a repo
is public that argument is gone permanently, and the additive-only rule
applies without exception.

**Cross-cutting invariant**: a version claim is never trusted for anything
*cryptographic* or used to grant elevated trust/capability — it stays a
compatibility/availability signal. The worst case of a forged claim is a
self-inflicted availability change (wrongly excluded, or wrongly not
excluded, from gossip), never elevated trust or bypassed verification — every
actual settlement operation stays independently, cryptographically verified
regardless of what version either side claims.

Concrete node-to-node version awareness (`crates/server/src/version.rs`):

- The mirror-watcher's wire format carries a real version field, additive so
  an older peer's response without it still decodes — an incompatible version
  becomes a clear structured log line and a distinct error, never a raw panic
  or an undifferentiated decode error.
- **The version a node reports is a compile-time constant**
  (`PROTOCOL_VERSION`), never a runtime-settable env var — closes the trivial
  "just set a config value" spoofing path. This is not cryptographic
  non-forgeability — a forked, recompiled binary can still hardcode a fake
  constant; genuine non-forgeability against a deliberately modified binary
  needs a signed release manifest, which doesn't exist yet.
- **A minimum-supported-version floor is baked into the binary**
  (`MIN_SUPPORTED_PEER_VERSION`), not left as an operator-configurable
  default — a peer below it is excluded from this node's peer table/gossip
  entirely. An env var can only raise the effective floor further above that
  baseline, never lower it. Exclusion is reversible: a peer that upgrades
  starts reporting a passing version and is naturally re-admitted on its next
  announce/gossip cycle.
- `GET /nodes/status` surfaces this node's own `protocol_version`, its
  configured `roles` (`AVALON_NODE_ROLES`, `combined` reported as-is), and,
  when known via peer gossip, a `stale` flag — a self-diagnostic "you may
  want to upgrade" signal only; nothing reads `stale` to change behavior.
  `roles` is what a caller that already has this node's URL uses for
  capability negotiation — settlement/indexer/realtime/gateway,
  before routing a request to it. The same response also carries a
  `resources` block — this node's own host-level
  CPU/memory/disk/process metrics plus its DB pool size/in-use — with the
  identical posture: every field is independently optional, a metric this
  process can't read on a given platform is `None` rather than a failed
  request, and nothing here is ever used to gate protocol behavior (peer
  admission, mirroring, consensus) or to signal a privileged node. Reported
  values are always this node's own host only — no cross-node resource
  aggregation happens in the protocol; any topology/dashboard view built on
  this data is a separate client's job.

Opt-in auto-update for self-hosted nodes is real, separate scope gated on a
release-signing mechanism that doesn't exist yet, and stays deliberately open.
Capability-based request forwarding (an outdated node relaying to a peer that
can handle a request) was considered and rejected for now — it adds a real
trust hop with no accountability story yet. A hard-fork escape hatch (a new
`network_id`, old network keeps running unchanged) is reserved, undesigned,
for a genuinely-incompatible-crypto-change case none of the above can cover.

## Current implementation

### Cross-node login

A person can log in through a node they've never registered a passkey on,
without any shared login domain, via a signed, human-approved,
destination-bound assertion — extending the cross-device pairing pattern
([`./identity.md`](./identity.md#cross-device-pairing-for-a-webauthn-incapable-client))
to a genuinely different node rather than a genuinely different device.

- `avalon_protocol::cross_node_login::CrossNodeLoginGrant` is a self-signed
  assertion that a human, shown real requesting-node context, approved
  logging an identity into a specific destination node. `POST
  /auth/cross-node/{start,poll,submit,deny}`
  (`crates/server/src/cross_node_login.rs`) is its server-side lifecycle —
  structurally close to the device-pairing create/poll/approve shape, but
  genuinely cross-node (approval never requires a live session on the
  requesting node itself). Verification checks the grant's signature against
  `identity_signing_keys`, destination-binds the grant's claimed URL against
  this node's own configured URL, and gates replay via a consumed-nonce table.
- **An identity locator over the DHT** resolves which node(s) currently hold
  an identity's signing keys, using the same DHT registration/lookup shape
  interest scopes already use (a new `Identity` scope, same trust model as
  `Network` — a bare advertised base URL, no signed claim, since a consumer
  still verifies whatever it actually fetches independently). A background
  worker periodically registers this node's own locally-known identities;
  `GET /identities/{id}/locations` (deliberately unauthenticated — it has to
  work *before* cross-node login can complete) resolves the full known set,
  never a single "winner."
- **Cross-shard projection resolution with inclusion proof**
  (`crate::cross_shard_fetch::fetch_verified_entries`) — given a shard id and
  base URL (typically from the locator above), fetches every ledger entry for
  a subject from that remote node and verifies each one end-to-end before
  trusting its payload: a signature-checked Signed Tree Head, a real RFC 6962
  inclusion proof checked against that STH's root, and the entry's
  `entry_hash` independently recomputed from its fetched content and compared
  against both the claimed hash and the proof's leaf hash. Without that last
  check, a remote node could hand back a genuine inclusion proof for *some*
  real entry alongside a completely different, forged payload. When a grant's
  signing key isn't found in this node's own local identity-signing-keys
  table, cross-node login verification chains the locator with this
  fetch-and-verify path against each candidate node in turn, matching the
  identity's signing-key history by key id and checking revocation the same
  way the local lookup does. A verified signing key alone isn't sufficient to
  mint a session — the destination node also best-effort provisions a minimal
  local identity/profile stub by cross-shard-fetching the identity's own
  `identity.created` entry, silently skipped (never a login failure) on a
  genuine cross-shard display-name collision, since this node's own
  uniqueness index can't be enforced globally.
- **Approval screens are real in both first-party clients**: the Hub's
  `avalon-hub/apps/hub/src/views/CrossNodeLogin.vue` and hub-app's
  `avalon-hub/apps/hub-app/src/views/CrossNodeLogin.vue` (with a real `avalon://`
  deep-link entry point on desktop; iOS/Android Universal Links are not yet
  set up). Both call `GET /auth/cross-node/lookup?user_code=...`
  (unauthenticated, returns status/requesting-context/expiry, never the
  polling device's own request code) before rendering anything approvable,
  then mint and sign a real grant locally and submit it directly to the
  *requesting* node's own base URL — never the approving client's own
  configured server.
- **No hard allowlist gate on the requesting integrator** — that would block a
  brand-new, not-yet-registered integrator's very first login, exactly the
  case this exists to unlock. Instead, the approval screen always renders,
  but visually distinguishes a verified requester (resolved against the same
  issuer-key/shard-settlement trust mechanism used elsewhere) from an
  unverified one, and the mobile deep-link flow never auto-approves off a
  scan — an explicit confirm step is always required. An owned shard is
  verified when its owner resolves to a real integrator holding an unrevoked
  `shard_settlement` key for exactly that shard; the default, unowned `core`
  shard is verified when the requesting node's URL is one of the network's
  real pinned seed nodes.
- Rate limiting is inherited automatically: the whole router, including every
  `/auth/cross-node/*` endpoint, sits behind the shared per-IP/per-integrator
  rate limiter with no route-group scoping needed.
- A known, honestly-documented race: the outbox pattern writes every event
  durably in the same transaction as the rest of a request, but a separate
  background worker is what actually folds it into the hash-chained ledger —
  there's a real window, on the order of the outbox poll interval, where an
  event is durable but not yet ledger-visible or cross-shard-fetchable. In
  practice this only matters for a login attempted within seconds of an
  identity's very first registration on its owning node.

### Node topology and role extraction

Historically exactly one node type existed: `avalon-server` running Gateway +
Settlement + Indexer + Realtime all in one process, with a "mirror" simply
being that same binary configured with `AVALON_MIRROR_PEERS`. That is still
the default, but each role can now be extracted into its own deployable
process:

- **Operator-internal node-to-node RPC** is the foundation the extraction
  tickets build on: once a Gateway process and its backing Indexer/Realtime/
  Settlement processes are genuinely separate, a Gateway's call sites need a
  way to reach a role that isn't in-process anymore. Deliberately distinct
  from the `/ledger/*` mirror-sync protocol (multi-operator, trust-minimized)
  — this is operator-internal, one deployment's own processes talking to each
  other, no independent verification needed on either side. HTTP+JSON, gated
  on a shared-secret bearer token (`AVALON_INTERNAL_ROLE_KEY`), unset meaning
  every request under `/internal/*` is refused. `avalon_indexer::Indexer`
  (`apply`/`rebuild`) was the first target: `POST /internal/indexer/apply`
  and `POST /internal/indexer/rebuild` are thin server-side twins of the
  in-process calls, and `RemoteIndexer` is a real `Indexer` implementation
  backed by HTTP calls to them — any code holding an `impl Indexer` can't
  tell it apart from a local `PostgresIndexer`. A remote role that's
  unreachable surfaces as a `503`, never conflated with "the data doesn't
  exist" or a generic storage bug.
- **Indexer extraction**: `AppState.indexer` is `IndexerHandle`, an enum over
  `Local(PostgresIndexer)` and `Remote(RemoteIndexer)`, decided at startup off
  the declared roles. A roles list excluding `indexer` (and not `combined`)
  requires `AVALON_INDEXER_REMOTE_URL` (and `AVALON_INTERNAL_ROLE_KEY`) or the
  process refuses to start. Every call site that used to call
  `PostgresIndexer::apply_in_tx` directly now goes through
  `IndexerHandle::apply_in_tx` followed by `apply_after_commit` once its
  transaction has committed — see
  [`query-and-indexing.md`](./query-and-indexing.md) for the consistency
  tradeoff the `Remote` variant accepts. The mirror-watcher keeps its own
  independent local `PostgresIndexer` regardless of role, since mirror-sync
  applies verified peer entries straight to this process's own local
  Postgres.
- **Realtime extraction**: an open WebSocket connection is stateful, so this
  needed a real connection-topology decision — proxy every client WebSocket
  through the Gateway to a remote Realtime node, or have the client connect
  directly. **Decided: proxy-through-Gateway** — keeps the "client always
  talks to one node's URL" invariant every other role extraction preserves.
  A roles list excluding `realtime` requires `AVALON_REALTIME_URL` or the
  process refuses to start. Presence/chat WebSocket handlers still
  authenticate the caller locally first; when a remote URL is configured, the
  upgraded socket is handed to a proxy that dials the remote node's identical
  endpoint and pumps frames bidirectionally until either side closes. The
  realtime relay already posts every locally-originated presence/chat event
  to any same-network peer advertising a `realtime`/`gateway`/`combined`
  role, so a dedicated Realtime node is exactly such a peer and a REST
  mutation handled by a Gateway still reaches it with no new relay logic
  needed.
- **Settlement extraction**: a roles list resolving to *exactly*
  `["settlement"]` is the one predicate that gates this. When true: no
  WebAuthn config is required (a harmless placeholder instance is built
  instead, since no Gateway handler that would touch it is ever mounted); a
  dedicated router replaces the normal one and mounts only `/ledger/*`,
  `/nodes/{announce,peers,status,log-level}`, and `/mirror/notify` — no
  identity/session/social/guild/achievement/integration route at all; and
  Gateway-only background workers (the outbox worker, the guild-message
  archive-expiry worker, the identity locator) are never spawned. Retention
  pruning, the mirror-watcher, the minimum-replication-guarantee worker, and
  node-to-node announce/bootstrap keep running regardless, since those are
  genuinely Settlement-side concerns. The mirror-image case — a Gateway-only
  node with no local Settlement role, pointed at a remote one — needed no new
  mechanism: the existing remote-submit configuration already worked
  standalone.
- **Backing-service discovery** (above) made the set of all three remote-role
  configuration surfaces coherent, including validating `AVALON_INDEXER_REMOTE_URL`
  as a well-formed URL at startup (previously only checked for
  non-emptiness) and adding the reachability probe described above.

Every role-extraction combination above has been live-verified with multiple
real `avalon-server` processes against shared or isolated Postgres, including
the negative cases (a Settlement-only node 404ing cleanly on Gateway routes
while still serving `/ledger/*`; a malformed or missing remote-URL config
refusing to start with a clear error).

### Hoster-configurable resource limits

Every deployment has four independently tunable safety floors, all defaulted
to what this process always hardcoded:

- `AVALON_MAX_DB_CONNECTIONS` (default `10`) — the Postgres pool's max
  connections.
- `AVALON_MAX_CONCURRENT_REQUESTS` (default `256`) — a concurrency-limiting
  middleware layer. Backpressures (bounded wait) past the ceiling, never
  drops a request without a response.
- `AVALON_RATE_LIMIT_PER_MINUTE` (default `3000`) — the outer per-IP flood
  ceiling, applied before authentication. Keyed by the connection's peer
  address only; no request header ever selects the bucket.
- `AVALON_PRINCIPAL_RATE_LIMIT_PER_MINUTE` (default `600`) — the inner
  per-principal limit, applied after a credential is verified. Keyed by the
  verified identity id (user session or continuation token) or the verified
  integrator id (integrator challenge-response credential), so users sharing
  an address each get their own budget and one user cannot consume another's.
  Requests that fail authentication are counted only by the per-IP ceiling.

  Over-limit responses from either layer are always `429` with `Retry-After`.
  Both layers exist in the in-process and Redis-backed limiters. The tracing
  and CORS layers wrap the per-IP limiter, so a `429` or `503` from it still
  carries CORS headers and browsers can read the status; the per-principal
  `429` is produced inside a handler and carries them the same way.
- `AVALON_TRUSTED_PROXIES` (default unset) — comma-separated IPs or CIDR
  ranges of the reverse proxies the node sits behind. Unset trusts no proxy
  and the per-IP key is the peer address. When the direct peer is in the list,
  the per-IP key becomes the right-most `X-Forwarded-For` entry that is not
  itself a trusted proxy, so an address a client prepends is never used. A
  missing or malformed header falls back to the peer address, and a peer not in
  the list has the header ignored entirely. Keys are canonicalized, so an
  IPv4-mapped IPv6 address and its IPv4 form share one bucket; both limiters
  derive the key identically. An unparseable value aborts startup.

  The proxy must overwrite or append `X-Forwarded-For`, never pass a
  client-supplied value through unchanged. For nginx:

  ```nginx
  location / {
      proxy_pass http://127.0.0.1:8080;
      proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
  }
  ```

  with `AVALON_TRUSTED_PROXIES=127.0.0.1` on the node. Without this setting a
  node behind a proxy sees the proxy as the peer address, so the per-IP ceiling
  treats all of its clients as one address.
- `AVALON_OUTBOX_POLL_INTERVAL_SECS` (default `3`) — the outbox worker's
  drain cadence.

**Per-process by default; optionally shared per-hoster.** Both the rate-limit
bucket and the concurrency counter live entirely in one process's memory by
default — an operator running more than one `avalon-server` process gets that
many independent copies of each ceiling unless they opt in.
`AVALON_REDIS_URL` makes both limits Redis-backed instead, **strictly scoped
to that one hoster's own processes** — never a network-wide shared limiter,
which would recreate exactly the single-point-of-control problem sharded
settlement exists to remove; this only lets one operator's own processes
agree with each other, the same way their own `DATABASE_URL` already does.
Both fail open (log a warning, let the request through) if Redis itself
becomes unreachable, rather than taking the whole node down over a rate-limit
backend outage.

### Host resource metrics

`GET /nodes/status`'s `resources` block reports CPU core count/usage %/load
averages, memory and swap used/total, disk used/total for the node's own
storage path and (when configured) a locally-readable Postgres data
directory, process uptime, open-file-descriptor count, and the DB pool's own
tracked size/in-use. Built on the cross-platform `sysinfo` crate rather than
hand-rolled `/proc` parsing, since hosters aren't guaranteed to run Linux.
Every leaf field is independently optional and best-effort.

## Open questions

SDK-side node discovery and capability negotiation (the SDK still takes a
bare `server_url`); TLS requirements before any non-local deployment; a
formal export format for the log.
</content>
</invoke>
