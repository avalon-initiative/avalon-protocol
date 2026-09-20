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
| Combined | any subset, including all | milestone 1: one `avalon-server` |

**Config knob**: `AVALON_NODE_ROLES` (comma-separated, e.g.
`settlement,indexer`) — see `crates/server/src/nodes.rs::node_roles`.
Unset defaults to `combined`. This value used to be purely advisory/
self-reported (peer-table bookkeeping and #539's realtime relay routing)
and never gated which endpoints a node actually serves. Three of #291's
role-extraction tickets have since made it genuinely load-bearing, each
for its own role: **#662** — a roles list excluding `indexer` (and not
`combined`) makes this process route indexer reads/writes to a remote one
instead of running a local `PostgresIndexer`; **#663** — a roles list
excluding `realtime` stops this process from handling `/ws/presence`/
`/ws/messages` locally, proxying those connections through to a configured
remote Realtime node instead; **#664** — a roles list resolving to
*exactly* `["settlement"]` (`crates/server/src/nodes.rs::is_settlement_only`)
makes `main.rs` skip wiring up every Gateway-facing module entirely and
serve a reduced route table. See each ticket's own "Today in the repo"
entry below. Every other roles value, including the `combined` default,
still behaves exactly as before any of these tickets: full route table,
every module wired up locally regardless of what's declared.

### A node's three configuration axes are independent

Easy to conflate, especially once a single node holds more than one at
once (see `avalon-peer`'s real deployment below) — these are three
separate questions, and a node's answer to one says nothing about its
answer to the other two:

| Axis | Question it answers | Values | Config |
|---|---|---|---|
| **Capability** | What services does this process run? | Settlement / Indexer / Realtime / Gateway / Combined (table above) | `AVALON_NODE_ROLES` |
| **Shard role** (per shard) | Does this node hold the real signing key and author this shard's writes, or does it only watch and independently verify another node's? | Authority (self-hosting.md's "shard operator") / Mirror (self-hosting.md's "mirroring the public network") | Authority: `AVALON_SETTLEMENT_SIGNING_KEY` set to that shard's registered key. Mirror: that shard's URL listed in `AVALON_MIRROR_PEERS` |
| **Retention tier** | How much local history does this node keep? | Full/archive (everything) / Hot (recent window only, issue #569-gated on confirmed archive coverage) | `AVALON_RETENTION_TIER` (see below) |

**Shard role is per-shard, not per-node** — a single node can be the
authority for one shard and a mirror of a completely different one at
the same time (nothing shares storage between the two: authored history
lives in `ledger_entries`, mirrored history in `mirrored_entries`, kept
separate by construction). `AVALON_OWN_SHARD_ID` (default `"core"`)
declares which shard, if any, this node authors — see
`crate::settlement`'s module doc comment (issue #573) for why every
`/ledger/*` read has to know this to answer correctly once a node holds
both roles.

**Real example, live in this environment**: `avalon-peer` is
Combined-capability (same as every node today), Full-tier, and holds
*both* shard roles — mirror of the primary's `core` shard, and authority
for its own separate `game:peer-shard-demo-*` shard. See this file's
"Today in the repo" section and
[`./settlement.md`](./settlement.md)'s "Cross-machine, real end to end"
section for the full write-up.

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

**Independent of shard role, same as it's independent of capability
(above).** A shard *authority* can be hot or full tier for the shard it
authors; a *mirror* can independently be hot or full tier too (this only
ever governs whether *this node's own* stored copy prunes old payloads —
a mirror's `mirrored_entries` isn't touched by pruning at all, see
`crates/chain/src/retention.rs`'s own doc comment). These three axes really are
orthogonal — see "A node's three configuration axes are independent"
above.

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
  (issue #569) — a third, independent, off-by-default opt-in that turns
  "never prune what nothing else retains" from operator discipline into
  something the code actually checks. Unset (the default): zero behavior
  change from before this existed. Configured: before each pruning pass,
  the node computes the boundary `seq` it's about to prune past and asks
  each listed peer's own `GET /ledger/mirror-progress?network_id={id}`
  (`crate::settlement::mirror_progress`) how far it has independently
  verified and mirrored this node's history — pruning only proceeds once
  at least `AVALON_RETENTION_MIN_ARCHIVE_CONFIRMATIONS` (default `1` once
  peers are configured) distinct peers confirm coverage past that
  boundary; otherwise the pass is skipped and retried next tick. Only the
  background worker performs this check — `avalon prune-ledger`'s manual
  entry point has no `reqwest` dependency in a `--no-default-features`
  build and refuses to run a real (non-`--dry-run`) prune at all when this
  is configured, rather than silently skipping the safety gate.
- `AVALON_MIN_MIRROR_CONFIRMATIONS`/`AVALON_MIRROR_GRACE_PERIOD_HOURS`/
  `AVALON_REPLICATION_POLL_INTERVAL_SECS` (issue #629, implementing
  #622's decision) — extends the #569 mechanism above to a different
  trigger point: not "is it safe for this node to prune its own local
  history," but "does a shard have enough independently-confirmed
  mirrors to accept a brand-new identity registration at all." Always
  on (no opt-in env var; defaults are `1` confirmation, a `24`-hour
  bootstrap grace period, and a 300s poll cadence) — unlike #569's
  pruning gate, which only matters for an operator who has opted into
  hot-tier retention, #629's gate protects every identity a shard is
  about to durably take on, so it applies by default rather than
  requiring configuration to turn on. See "Minimum replication guarantee
  for new registrations" in this file's "Today in the repo" section
  below for the full mechanism.

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

**Milestone-1 honesty, updated (2026-09-17)**: a second, physically-separate
node now genuinely exists in this environment — see "Today in the repo"
below for `avalon-peer`'s real, live-verified mirror deployment (its own
Postgres container on a distinct host, independently verified against the
pinned trust-anchor key, fully converged with the primary's real history).
That closes the "no archive-tier mirror exists at all" gap this note used
to describe. **Archive-confirmation gating (#569) closes the second half**:
"never prune what nothing else retains" is no longer pure operator
discipline — a hot node configured with `AVALON_RETENTION_ARCHIVE_PEERS`
now genuinely refuses to prune past what its configured archive peers
haven't yet confirmed, live-verified end to end against a real, isolated
Postgres database (a real `PostgresSettlementProvider`, real committed
entries, real fake-peer HTTP servers reporting coverage): pruning
demonstrably blocked while coverage was unconfirmed, then proceeded once
it was, with `prunable_entry_count` going from nonzero to zero exactly
when expected. This is opt-in (`AVALON_RETENTION_ARCHIVE_PEERS` unset
means zero behavior change from before), so it doesn't retroactively make
every past pruning decision safe — it means a node that turns it on now
gets a real, enforced guarantee instead of a documented risk.
Two things this second node/mechanism does *not* by itself solve: **write
availability** during a primary outage (a mirror-only node never becomes
a new writer/authority on its own) and the harder multi-writer/consensus
question #40 still owns. The first is now decided, not open — see
[#609](https://github.com/LunarVagabond/avalon-protocol/issues/609)
(decided: a manual, operator-driven promotion runbook, prioritized for
the `core` shard specifically; tracked as
[#630](https://github.com/LunarVagabond/avalon-protocol/issues/630)) and
[#622](https://github.com/LunarVagabond/avalon-protocol/issues/622)
(decided and implemented: a minimum confirmed-mirror count before a
shard is trusted with new identity registrations at all, extending this
same archive-confirmation mechanism to a new trigger point —
[#629](https://github.com/LunarVagabond/avalon-protocol/issues/629), see
below) — deliberately never automatic election/failover, which would
reopen #186.
What this second node/mechanism already solves today: a genuine second,
independently-verifiable copy of Settlement history no longer depends on
one physical database being up (reads against `avalon-peer` succeed today
even with the primary down, served from its own independently-verified
mirrored data), and a hot node's pruning decision is no longer a
documented risk an operator has to manage by hand — it's an enforced
check against real, confirmed coverage.

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

- A node **cannot fabricate** "Integrator A issued this achievement". Attestations are
  signed by Integrator A's registered issuer key
  ([`./issuers.md`](./issuers.md)); a node that stores an
  unsigned or wrongly-signed claim has stored something every verifier rejects.
- A node **cannot replace** a signature, alter a settled entry, or drop one
  without detection — the log is hash-chained and, once
  [#40](https://github.com/LunarVagabond/avalon-protocol/issues/40) lands,
  signed and mirrorable.
- A node **cannot act as an identity**. Identity mutations are authorized by the
  identity's own key ([`./identity.md`](./identity.md),
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
federation. Under federation, whether Integrator B can see an identity would
depend on which servers Integrator B's server peers with; that recreates the walled
gardens Avalon exists to remove. Under mirroring, a client does not pick "which
server to trust": any mirror that misrepresents the log is detectable, because
the log is self-verifying. The log *is* Avalon's own chain, not an anchor into someone else's
([#79](https://github.com/LunarVagabond/avalon-protocol/issues/79), closed;
[ADR #93](https://github.com/LunarVagabond/avalon-protocol/issues/93)), and
that does not change this.

Mirroring addresses *read* decentralization — anyone can independently
verify the log without trusting whichever node they happened to ask.
[#527](https://github.com/LunarVagabond/avalon-protocol/issues/527)
(decided) addresses the complementary *write*/control problem — one
operator holding sole settlement authority over the whole network — by
sharding settlement authority per-integrator instead. See
[`settlement.md`](./settlement.md)'s "Cross-shard commitment" section
(#529) for how a network with more than one shard still produces one
globally verifiable state with no designated aggregator, mirroring
this section's own "no single trusted party" standard one layer up. An
operator running their own shard (rather than only mirroring) is a third,
distinct self-hosting category — see
[`self-hosting.md`](./self-hosting.md)'s "shard operator" section (#530)
for exactly how that stays unambiguous from both mirroring and a
disconnected private fork.

## Identity and social actions are not shard-locked

An identity has no "home node" in any operationally ongoing sense, and
this is by construction, not an accident worth losing track of. Which
shard a new event commits into is a property of *whichever node handles
the request* (`AppState::own_shard_id`), never of the identity acting
through it — there is no code path anywhere that requires an identity's
actions to route back through the shard its `identity.created` event
originally landed on. "Home" describes a historical fact (where that
first event is durably committed, forever, by replication) — not a place
an identity depends on continuing to reach.

Concretely: a person can authenticate through any live node (once
[#620](https://github.com/LunarVagabond/avalon-protocol/issues/620)'s
cross-node login exists — see that decision for the actual mechanism) and
their friend/guild/profile actions commit into *that* node's own shard,
exactly as if they'd always used it. If the node/shard they'd been using
disappears mid-session, reconnecting through a different live node and
continuing is the same "reconnect elsewhere" pattern
[#541](https://github.com/LunarVagabond/avalon-protocol/issues/541)
already proved for realtime chat, just extended to authoring durable
events generally rather than only receiving live pushes.

**The real, narrower failure that remains** (tracked as
[#609](https://github.com/LunarVagabond/avalon-protocol/issues/609)) is a
*specific* integrator's own dedicated settlement shard (their achievements,
tournament results — anything issued under that shard's own authority,
per [`settlement.md`](./settlement.md)'s per-shard trust model) going
down: that pauses *that integrator's own* new issuances, a real but
contained, per-integrator blast radius — never a network-wide one, and
never something that stops an identity from acting anywhere else.

Separately, none of this helps if a shard's *data* was never durably
copied anywhere to begin with — see
[#622](https://github.com/LunarVagabond/avalon-protocol/issues/622)
(open) for the distinct, more foundational question of whether any
minimum replication guarantee should exist at all, versus today's purely
opt-in mirroring.

**A pure mirror's "nothing here yet" 404 says so (#519).** Before a mirror
node has backfilled anything for a given shard, `GET /ledger/sth/latest`
(and `/ledger/sth/{tree_size}`) 404 — same as a genuinely empty Settlement
authority that hasn't committed anything either, which used to make the
two indistinguishable to a caller. When the requested shard has a
configured mirror source (`AVALON_MIRROR_PEERS`), the 404 body now
includes `is_mirror: true` and `mirror_peers`, naming the peer to ask
instead, rather than looking like a broken/misconfigured node. A genuine
empty authority's 404 is unchanged — this never fabricates or synthesizes
an STH, only enriches the error body explaining *why* nothing was found.

## Discovery

A developer should not need to know `postgres://...` or
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

**Discovery feeding mirror bring-up directly (#511).** #362's announce/
bootstrap discovery (`GET /nodes/peers`, this section) and mirroring
(`AVALON_MIRROR_PEERS`, the previous section) used to be entirely
disconnected in practice — a hoster who successfully discovered peers still
had to hand-copy URLs into `AVALON_MIRROR_PEERS` themselves. `make
stack-up`'s first-run bring-up now closes that gap: `avalon
discover-mirror-peers` (run via the `discover-mirror-peers` Compose
service, so it needs no host Rust toolchain) queries the resolved bootstrap
peer's address book and prompts interactively for which peer(s) to mirror,
writing the selection straight into the generated `.env`. This never makes
discovery *imply* mirroring — every skip condition (already configured, no
bootstrap peer resolved, unreachable, empty address book, no TTY) exits
silently with no behavior change, and "mirror every discovered peer" is
never the default even when offered; an explicit `ALL` or a blank/skipped
prompt are the only two ways to leave with zero or every peer, never a
silent choice made on the hoster's behalf.

**DHT identity and bootstrap (#582, part of epic #580).** #542 decided that
realtime relay at real network scale routes over a libp2p Kademlia DHT
instead of #539's full-mesh broadcast (see [`communication.md`](./communication.md)'s
own note on that decision) — #582 is the identity/bootstrap piece of that
work, not the routing logic itself (that's #583). A node running the DHT
(`AVALON_DHT_ENABLED`, on by default as of ADR #593 — an opt-*out* escape
hatch, not an opt-in gate; see `crate::dht`'s module doc for the full
rationale) holds a libp2p `PeerId`, a *fourth* independent key
domain alongside player keys (#73), issuer keys (#80/#84), and the
settlement log operator's key (#39) — none of those fit a peer-transport
identity, so this is genuinely new rather than reused. Bootstrap reuses
this section's own peer table rather than inventing a second discovery
mechanism: `PeerInfo`/`POST /nodes/announce` now also carry a peer's
`libp2p_peer_id` and dialable multiaddrs (`#[serde(default)]`, so an older
peer's announce without these fields still deserializes fine), and
`crate::dht`'s worker watches that same table for identities it hasn't
dialed into the DHT yet.

**Peer-set growth past the bootstrap list, and shard-existence gossip
(#599).** Before this, `nodes::run_worker` only ever re-announced to
whatever `AVALON_BOOTSTRAP_PEERS` (or a network's `seed_nodes`) resolved
to at startup — a peer discovered one hop further out was recorded in the
passive `PeerTable` but never itself became an ongoing announce target, so
propagation stopped one hop past the bootstrap set. Now the worker keeps
its own growing `active_peers` list, seeded from the bootstrap set (still
never evicted — it's still how a brand-new node reaches the mesh at all on
a cold start) and extended, bounded by `AVALON_NODE_MAX_PEERS` (default
50), with peers discovered through those announce exchanges — the same
bounded-fan-out/full-eventual-reach property Kademlia's own k-bucket
routing-table maintenance and gossip-membership protocols (SWIM,
HyParView) rely on. Riding on the exact same mechanism, `AnnounceRequest`/
`AnnounceResponse` now also carry a `known_shards` snapshot — a node
authoritative for a shard (real, local, signed settlement history for it)
gossips that fact, and every shard it's otherwise learned about, to its
active peer-exchange partners; a node's `crate::nodes::ShardRegistry`
accumulates this into a full picture of "every shard that exists on this
network" without any operator manually listing them (see
[`./settlement.md`](./settlement.md)'s own "Automatic shard discovery"
section for the trust/verification side, which is completely unchanged
by any of this).

**Scenario K — a node disappears.** The SDK routes to another node advertising
the needed capabilities. Durable history is unaffected (it is mirrored);
presence for identities *publishing through* that node lapses until they
themselves reconnect and republish elsewhere
([`./presence.md`](./presence.md)); nothing an integrator had already verified becomes
unverifiable. A client that was only *subscribed* (reading presence/chat)
through the disappeared node, not authoring anything through it, resumes
live delivery immediately on reconnecting anywhere else — #539's
cross-node relay means every node already receives the same events, so
there's no session hand-off to perform, only a fresh connection to make.
Live-verified as its own ticket (#541,
[`crates/server/tests/realtime_reconnect.rs`](../../crates/server/tests/realtime_reconnect.rs)):
reconnecting to a different node with the same session token resumes
delivery, and the gap is bounded to the disconnect window itself, never
ongoing afterward.

## Version rollout

Decided in #308. No party can force any operator to upgrade — self-hosting
with no central gatekeeper is deliberate, not a gap — so this has to work
with permanent version skew across the network as a constraint, not design
around it away. Three separate axes, conflating them is the actual failure
mode: **event-schema version** (payload shape per event kind — #82's
already-decided additive-only policy), **wire/API version** (HTTP endpoint
shapes, mirror-sync DTOs), and **settlement/crypto version** (hash
algorithm, signature scheme, STH/Merkle format — the one axis where mixed
versions on the *same* `network_id` genuinely isn't safe).

**Standing rule for the wire/API axis, effective now**: additive-only,
forever-compatible — generalizing #82's payload policy and what #293
(`/games` → `/integrations`) and #297 (additive `register-integrator`
alias) already practiced without naming it. Never remove, rename, or
repurpose a field or endpoint outright; add alongside and deprecate slowly.
This is a code-review discipline expectation starting now, not gated on
any further ticket.

**One deliberate exception, taken once, before the repo went public: #290.**
The terminology generalization renamed the request/response field names, and
dropped the `/games` route family and the `x-avalon-game-*` headers that #293
had kept as compatibility paths. That is squarely a break of the rule above,
made knowingly on the grounds the rule itself depends on: the rule exists
because operators who cannot be forced to upgrade are running independent
nodes, and at the time of #290 there were none — the repo was private with
zero external integrators, and every consumer of these shapes lived in this
monorepo and was updated in the same change. Once the repo is public that
argument is gone permanently, and the additive-only rule applies without
exception. Any future proposal to rename a wire field or endpoint needs its
own decision issue; it does not get to cite #290 as precedent.

**Cross-cutting invariant, applies to every future piece of this**: a
version claim is never trusted for anything *cryptographic* or used to
grant elevated trust/capability — it stays a compatibility/availability
signal. This is why it's still safe to *gate network membership* on a
version claim (below): the worst case of a forged claim is a
self-inflicted availability change (wrongly excluded, or wrongly not
excluded, from gossip), never elevated trust or bypassed verification —
every actual settlement operation stays independently, cryptographically
verified regardless of what version either side claims. The same
reasoning #40/#299 already apply to a single signed STH (never trusted
without corroboration) applies here too.

Concrete node-to-node version awareness, implemented by #368
(`crates/server/src/version.rs`):

- The mirror-watcher's wire format carries a real version field
  (`SignedTreeHeadDto.protocol_version`, additive/`#[serde(default)]` so
  an older peer's response without it still decodes) — an incompatible
  version becomes a clear `event = "incompatible_peer_version"` structured
  log line and a distinct `MirrorWatcherError::IncompatiblePeerVersion`,
  never a raw panic or an undifferentiated decode error.
- **The version a node reports is a compile-time constant**
  (`PROTOCOL_VERSION`), never a runtime-settable env var — closes the
  trivial "just set a config value" spoofing path. Stated honestly: this
  is not cryptographic non-forgeability — a forked, recompiled binary can
  still hardcode a fake constant. Genuine non-forgeability against a
  deliberately modified binary needs #369's signed release manifest; this
  only removes the no-recompile-required spoofing path.
- **A minimum-supported-version floor is baked into the binary as the
  real baseline** (`MIN_SUPPORTED_PEER_VERSION`, released alongside
  `PROTOCOL_VERSION`), not left as an operator-configurable default — a
  peer below it is excluded from this node's peer table/gossip entirely.
  An `AVALON_MIN_PEER_VERSION` env var can only raise the effective floor
  further above that baseline, never lower it — otherwise an operator (or
  a compromised `.env`) could disable enforcement entirely by setting a
  permissive value. Exclusion is reversible: a peer that upgrades starts
  reporting a passing version and is naturally re-admitted on its next
  announce/gossip cycle, no manual unban step. Raising the binary's own
  baked-in floor over time happens via a real, changelog-visible release,
  never a silent default bump. `nodes::PeerTable::admit_if_supported` is
  the one enforcement point, shared by `POST /nodes/announce`'s handler
  and `nodes::run_worker`'s gossip merge — a below-floor peer is dropped
  (logged, never upserted), not rejected outright the way a `network_id`
  mismatch is.
- `GET /nodes/status` surfaces this node's own `protocol_version` and,
  when known via peer gossip (#362's peer table), a `stale` flag — `true`
  when some known peer reports a *newer* version than this node's own. A
  self-diagnostic "you may want to upgrade" signal only; nothing reads it
  to change behavior. Since #517, the same response also carries a
  `resources` block — this node's own host-level CPU/memory/disk/process
  metrics plus its DB pool size/in-use — with the identical posture: every
  field is independently optional, a metric this process can't read on a
  given platform is `None` rather than a failed request, and nothing here
  is ever used to gate protocol behavior (peer admission, mirroring,
  consensus) or to signal a privileged node, per #292's "no orchestrator
  node" decision. Reported values are always this node's own host only —
  no cross-node resource aggregation happens in the protocol; any
  topology/dashboard view built on this data is a separate client's job.

Opt-in auto-update
for self-hosted nodes — the further step of a node acting on a newer
version's existence, not just reporting it — is real, separate scope
gated on a release-signing mechanism that doesn't exist yet, and is
deliberately left open as #369 rather than decided here. Capability-based
request forwarding (an outdated node relaying to a peer that can handle a
request) was considered and rejected for now — it adds a real trust hop
with no accountability story yet; revisit once real operational experience
with version skew exists. A hard-fork escape hatch (a new `network_id`,
old network keeps running unchanged) is reserved, undesigned, for the
genuinely-incompatible-crypto-change case none of the above can cover.

## Today in the repo

- **Cross-node login (epic #623) has landed its foundation, not the full
  epic**: `avalon_protocol::cross_node_login::CrossNodeLoginGrant` (#633) —
  a self-signed assertion, structurally close to #525's
  `ContinuationToken` and #610's `InterestClaim`, that a human, shown real
  requesting-node context, approved logging an identity into a specific
  destination node — and its server-side lifecycle (#634):
  `POST /auth/cross-node/{start,poll,submit,deny}`
  (`crates/server/src/cross_node_login.rs`), structurally close to #307's
  `device_pairing` create/poll/approve shape but genuinely cross-node
  (approval never requires a live session on the requesting node itself).
  Verification checks the grant's signature against
  `identity_signing_keys`, destination-binds `destination_base_url` against
  this node's own `AVALON_NODE_URL` (#610's destination-binding lesson,
  applied here), and gates replay via `consumed_cross_node_login_nonces`
  (migration `0072_cross_node_login`), same pattern #525 already
  established. Live-verified against real Postgres and a real server
  process: same-device fast-path submit, cross-device start/submit/poll
  round-trip, destination-mismatch rejection, replay rejection, expiry/TTL
  rejection, wrong-key rejection, and deny-then-blocked-submit
  (`crates/server/tests/cross_node_login.rs`, `--ignored`). **The identity
  locator over the DHT has also landed (#635)**: `crate::identity_locator`
  reuses `crate::interest`'s existing DHT registration/lookup shape (a new
  `InterestScope::Identity` variant, same trust model as `Network` — a bare
  advertised `own_base_url`, no signed claim, since a consumer still
  verifies whatever it actually fetches from a resolved location
  independently) rather than a second DHT mechanism.
  `identity_locator::run_worker` periodically scans this node's own
  `identity_signing_keys` for identities not yet registered and holds one
  standing `InterestGuard` per identity for the life of the process, the
  same durable-registration shape #596's mirror-sync `Network` scope
  already established; `GET /identities/{id}/locations` (deliberately
  unauthenticated — it has to work *before* cross-node login can complete)
  resolves the full known set, never a single "winner," per the epic's own
  two-layer-identity scope note. Live-verified: the DHT-level `Identity`
  scope put/get round-trip between two real swarms
  (`crates/server/tests/interest_dht.rs`), and the full worker+HTTP path
  against real Postgres and a real server process
  (`crates/server/tests/identity_locator.rs`, both `--ignored`). Live
  testing surfaced, and a same-day follow-up fixed, a real startup-noise
  issue: a node with a large backlog of already-known local identities was
  registering all of them at once on the worker's very first scan tick,
  firing a burst of simultaneous immediate `PutRecord`s well ahead of the
  DHT swarm's own bootstrap — harmless (each registration's own refresh
  loop retries regardless) but real, unnecessary `put_record failed: the
  quorum failed` log noise, live-observed, not just theoretical.
  `identity_locator::run_worker` now paces registrations found within one
  scan tick `REGISTRATION_STAGGER` (100ms) apart instead of firing them all
  in the same instant — confirmed live (the failure burst's own log
  timestamps went from identical to evenly spaced) and unit-tested
  (`identity_locator::tests`, using `tokio::time::pause` rather than real
  sleeps). The module's other known simplification — a full rescan every
  tick, not an incremental one — is unchanged.
- **Cross-shard projection resolution with inclusion proof has also landed
  (#636)**: `crate::cross_shard_fetch::fetch_verified_entries`
  (`crates/server/src/cross_shard_fetch.rs`) — given a `shard_id` +
  `base_url` (typically from #635's locator), fetches every ledger entry
  for a `subject` from that remote node and verifies each one end-to-end
  before trusting its payload: a signature-checked Signed Tree Head
  (against `crate::cross_shard::resolve_shard_verify_keys_from_db`'s same
  #543 trust-anchor mechanism `crate::cross_shard`'s own cross-shard-root
  aggregation already uses — deliberately not a second trust mechanism), a
  real RFC 6962 inclusion proof checked against that STH's root, and —
  the real gap this closes — the entry's `entry_hash` **independently
  recomputed** from its fetched content and compared against both the
  claimed `entry_hash` and the proof's `leaf_hash`. Without that last
  check, a remote node could hand back a genuine inclusion proof for *some*
  real entry alongside a completely different, forged payload, and
  signature/proof verification alone would still pass — `avalon_chain`'s
  previously-private `hash_entry`/`EntryContent` (`crates/chain/src/postgres.rs`)
  are now `pub`, exposed for exactly this, and `InclusionProofResponse`
  (`GET /ledger/proof/inclusion`) now also returns `leaf_index`, which a
  one-off cross-shard verifier has no other way to learn (unlike a
  continuously-backfilling mirror, which already tracks its own next
  `leaf_index` from its own backfill progress). Live-verified against a
  real server and real Postgres: a freshly registered identity's real
  `identity.signing_key_added` entry fetched and fully verified, an unknown
  subject resolving to no entries, and two fail-closed trust-anchor cases
  (no verify key resolves; a real-but-wrong verify key) — all
  `crates/server/tests/cross_shard_fetch.rs`, `--ignored`. The one
  adversarial case a real server would never itself produce — a forged
  payload paired with a genuinely valid proof for the real content — is
  exercised separately against a mocked remote node
  (`crates/server/tests/cross_shard_fetch_tamper_detection.rs`, `wiremock`,
  not gated `--ignored`, no live infra needed). **Deliberately generic**:
  returns every verified entry for a subject rather than picking "the
  one" — #636's own scope is the fetch-and-verify primitive, not
  identity-signing-key-specific business logic (which key is still active,
  revocation) that a caller like #634's verification path would own for
  itself; that actual wiring — having #634 call this when a grant's
  `signing_key_id` isn't found locally — is **still not built**, tracked
  as the concrete remaining gap in #634's own verification path.
- **Rust SDK support has landed (#637)**: `AvalonClient::cross_node_login`/
  `CrossNodeLogin::wait` (`crates/sdk/src/cross_node_login.rs`) mirror
  `device_login`'s own start/poll shape — see
  [`sdk.md`](./sdk.md)'s own "Today in the repo" entry for the full
  detail, including the same-device fast path
  (`submit_cross_node_login_grant`) and why this SDK's usual
  integrator-backend callers rarely use it (they never hold a *player's*
  own signing key).
- **The phishing-context decision is now decided (#642, closed)**: no hard
  allowlist gate on the requesting integrator (would block a brand-new,
  not-yet-registered integrator's very first login — the exact case this
  epic exists to unlock) — instead, the approval screen always renders,
  but must visually distinguish a verified requester (resolved against the
  same #543 issuer-key/integrator registry shard-settlement trust already
  uses) from an unverified one, and mobile-hub's QR/deep-link flow must
  never auto-approve off a scan, always routing into an explicit confirm
  step. Actually resolving verified-vs-unverified status server-side is
  its own new piece of work (#649, sub-issue of this epic) — #639/#640
  aren't blocked on it landing first, they can ship the visual-distinction
  UI against a stubbed value and wire #649 in once it exists.
- **C# SDK support has also landed (#638)**: `CrossNodeLogin.cs` ports
  #637's Rust shape directly — `AvalonClient.CrossNodeLoginAsync`/
  `CrossNodeLogin.WaitAsync` and the same-device fast path
  `SubmitCrossNodeLoginGrantAsync`, the first place this SDK signs with a
  *player's* own Ed25519 identity key rather than an integrator's issuer
  key — see [`sdk.md`](./sdk.md)'s own "Today in the repo" entry for the
  full detail.
- **Hub approval screen has landed (#639)**: `apps/hub/src/views/CrossNodeLogin.vue`
  (route `cross-node-login`, a `?node=...&user_code=...` deep link) — real,
  not stubbed, and it's what surfaced a genuine missing server piece:
  `submit`/`deny` only ever took a `user_code` with no read path to go with
  it, so an approval screen had nothing to show before a human decided.
  Added `GET /auth/cross-node/lookup?user_code=...`
  (`crates/server/src/cross_node_login.rs::lookup`) to close that gap —
  unauthenticated, returns `status`/`requesting_context`/`expires_in`,
  deliberately never the polling device's own `request_code`. The Hub
  screen calls `lookup` before rendering anything approvable (#642's
  decided requirement), then — on approve — mints and signs a real grant
  locally (`packages/api-client/src/crypto/crossNodeLogin.ts::mintCrossNodeLoginGrant`,
  reading the identity's signing key the same way `continuation.ts`/
  `interestClaim.ts` already do) and submits it directly to the
  *requesting* node's own `base_url`, never this Hub's own configured
  server — a genuinely new client-side capability (`client.ts`'s new
  `crossNodeLoginRequest` helper takes an explicit `baseUrl` per call,
  unlike every other function in that file, which always targets this
  Hub's own fixed `BASE_URL`). Live-verified: `crates/server/tests/cross_node_login.rs`'s
  three new `lookup` tests (pending, denied, unknown-code-404), plus
  `packages/api-client/src/crypto/crossNodeLogin.test.ts` proving the
  signed bytes match the Rust side exactly and a destination swap breaks
  the signature, same as `interestClaim.test.ts` already proves for #610.
  **#639's own session also found and fixed a real, unrelated bug**: the
  root `npm install` had never been rerun since `packages/api-client` was
  added as a workspace, so `node_modules/@avalon/api-client` was simply
  missing — the "known pre-existing issue" earlier sessions documented
  (`vue-tsc -b apps/hub`/`npm run -w apps/hub test` both failing with a
  pile of `Cannot find module '@avalon/api-client'` errors) was never a
  real code problem, just a stale `node_modules`. A plain `npm install` at
  the repo root fixed it completely — `vue-tsc -b apps/hub` is now clean,
  and `npm run -w apps/hub test` passes all 472 tests.
- **mobile-hub's own approval screen has landed (#640)**:
  `apps/mobile-hub/src/views/CrossNodeLogin.vue` is the same approval logic
  #639's Hub screen established (same `@avalon/api-client` functions —
  `lookupCrossNodeLogin` before rendering anything approvable per #642,
  `mintCrossNodeLoginGrant`/`submitCrossNodeLoginGrant`/`denyCrossNodeLogin`
  — neither app imports the other's `.vue`, only the shared package), with
  its own real deep-link entry point: `tauri_plugin_deep_link` registered
  in `src-tauri/src/lib.rs`, an `avalon://` custom scheme declared in
  `tauri.conf.json`'s `plugins.deep-link.desktop.schemes`, and
  `deepLink.ts` parsing an incoming `avalon://cross-node-login?node=...&user_code=...`
  URL and routing into the screen — `getCurrent()` for a cold launch,
  `onOpenUrl()` for one that arrives while already running. #642's
  no-auto-approve requirement holds regardless of entry point: a deep link
  only ever prefills the form, `lookup`+an explicit tap on Approve is still
  required. **Two real, explicitly-not-closed gaps, not silently assumed
  done**: desktop `onOpenUrl` needs `tauri-plugin-single-instance` to route
  a second-launch URL into the already-running app on Windows/Linux (macOS
  gets it natively) — not added; and true iOS Universal Links / Android App
  Links need this repo's native mobile projects initialized at all
  (`tauri ios/android init`, neither run — `src-tauri/gen/` only has
  desktop/linux capability schemas) plus a real hosted domain's
  `.well-known` association files, both out of scope here. Manual
  node/code entry (typed by hand) always works regardless of any of this.
- **Rate limiting (#641) turned out to already be covered, verified live
  rather than assumed** — closed with no new code. `crate::router`
  (`crates/server/src/lib.rs`) wraps the *entire* router in the #545/#363
  mechanism this issue asked to reuse (`GovernorLayer`/`ConcurrencyLimitLayer`,
  or their Redis-backed equivalents when `AVALON_REDIS_URL` is set, keyed by
  peer IP for any unauthenticated route) — there's no route-group scoping
  in how the router is built, so every `/auth/cross-node/*` endpoint
  inherited it the moment #634 added those routes, same as any other
  endpoint in the file. Confirmed live: with
  `AVALON_RATE_LIMIT_PER_MINUTE=5`, 5 rapid `POST /auth/cross-node/start`
  calls from one IP returned `200`, the next 5 returned `429`, and a
  `POST /auth/cross-node/submit` right after also `429`'d from the same
  exhausted per-IP bucket. A *tighter, dedicated* ceiling specifically for
  cross-node-login, separate from the shared per-IP budget every other
  unauthenticated route also draws from, would be new scope beyond what
  #641 asked for — a real future option, not assumed needed here.
- **#649's verification-status resolution has landed — epic #623's last
  open sub-issue, now closed.** `GET /auth/cross-node/lookup`'s response
  gained `integrator_verified`/`display_name`
  (`crate::cross_node_login::resolve_requester_verification`), reusing
  #543's existing shard-trust mechanism rather than a second registry —
  two paths: an owned shard (`"{namespace}:{owner}"`, `game`/`app`/`service`)
  is verified iff `owner` resolves to a real integrator currently holding
  an unrevoked `shard_settlement`-purpose issuer key for exactly that shard
  (the identical join `crate::cross_shard::resolve_shard_verify_keys_from_db`
  already trusts for STH verification); the default, unowned `"core"` shard
  has no integrator to check, so it's verified iff `own_base_url` is one of
  the network's real seed nodes
  (`docs/trusted-networks.json`/`avalon_sdk::network::bundled_trust_anchors`).
  The anchor-matching logic (`is_verified_seed_node`) is pulled out as a
  pure function and directly unit-tested with a controlled anchor list —
  this sandbox's own trusted-networks.json entry has an empty `seed_nodes`,
  so a live test against the real bundled file could only ever prove the
  "not verified" branch. Live-verified: the owned-shard path flips from
  unverified to verified, live, on the same running node, the moment the
  matching integrator registers its `shard_settlement` key (no restart, no
  snapshot staleness); the default-shard path confirmed unverified against
  this sandbox's own real config
  (`crates/server/tests/cross_node_login_verification.rs`, `--ignored`,
  the owned-shard test needing a `AVALON_OWN_SHARD_ID=game:...`-configured
  server per its own module doc comment). #639/#640 both now render a real
  verified/unverified distinction (a green `AvalonWarningBanner`-adjacent
  badge with the registered name, vs. an explicit `AvalonWarningBanner`
  warning) instead of the raw `requesting_context` string alone — never a
  hard gate, per #642's own decision.
- **#656 closes the real gap #636's own doc comment flagged as still
  open: `verify_grant` now actually calls the cross-shard fetch-and-verify
  path when `signing_key_id` isn't found in this node's own
  `identity_signing_keys`.** `resolve_signing_key_cross_shard`
  (`crate::cross_node_login`) chains #635's locator with #636's
  `fetch_verified_entries` against each candidate `base_url` in turn,
  matching the identity's `identity.signing_key_added`/
  `identity.signing_key_revoked` history by `signing_key_id` and checking
  revocation before trusting the fetched key — the same
  `revoked_at IS NULL` semantics the local lookup already has, just against
  a remote node's own verified history instead of a local column. Identity
  signing keys don't carry their own shard — they're resolved against the
  fixed `"core"` shard (`IDENTITY_SIGNING_KEY_SHARD_ID`), a documented
  simplification rather than a silent one: #543's own issuer-key trust
  mechanism has no owner to resolve for `"core"`, so
  `core_shard_verify_keys` supplies the same pinned-network-anchor trust
  `resolve_requester_verification` (#649) already uses for exactly the same
  shard. **A verified signing key alone isn't sufficient to mint a session
  here** — `sessions.identity_id` has a real foreign key into `identities`,
  and `GET /me` needs a `profiles` row to return anything — so the
  cross-shard branch also calls `provision_local_identity_stub`, which
  cross-shard-fetches the identity's own `identity.created` entry the same
  way and best-effort upserts a minimal local `identities`/`profiles` row
  (silently skipped, never surfaced as a login failure, on a genuine
  cross-shard `display_name` collision — this node's own uniqueness index
  can't be enforced globally). Live-verified against two genuinely separate
  `avalon-server` processes with isolated Postgres schemas, real libp2p DHT
  bootstrap between them, and a real WebAuthn registration on one node
  followed by a same-device-fast-path grant submitted directly to the
  other, which had never seen the identity before
  (`crates/server/tests/cross_node_login_cross_shard.rs`, `--ignored`).
  **A second real gap surfaced and got fixed along the way, not papered
  over**: the outbox pattern (`crate::outbox`) writes every event durably
  in the same transaction as the rest of a request, but a separate
  background worker (`AVALON_OUTBOX_POLL_INTERVAL_SECS`, 3s by default) is
  what actually folds it into the hash-chained `ledger_entries`/Merkle tree
  — there's a real window, on the order of that poll interval, where an
  event is durable but not yet ledger-visible or cross-shard-fetchable.
  #635's locator propagates independently of the outbox and can resolve
  before the outbox worker has caught up, so a cross-node login attempted
  in that window can still spuriously fail — not a bug in the verification
  logic itself, an inherent, honestly-documented property of the outbox
  pattern this epic didn't introduce. In practice this only matters for a
  login attempted within seconds of the identity's *very first*
  registration on its owning node; the test's own
  `wait_for_signing_key_ledger_entry` polls the real ledger endpoint rather
  than sleeping a fixed amount, to make this race visible rather than
  flaky.
- Exactly one node type exists: `avalon-server` (`crates/server/src/main.rs`)
  running Gateway + Settlement (via `PostgresSettlementProvider`) + Indexer
  (`PostgresIndexer`) + Realtime (`presence.rs`'s WebSocket service) all in
  one process. There is still no distinct "mirror" binary/deployment
  shape — a mirror is simply an `avalon-server` deployment with
  `AVALON_MIRROR_PEERS` set, watching another deployment
  ([#299](https://github.com/LunarVagabond/avalon-protocol/issues/299),
  see [`settlement.md`](./settlement.md)'s "Mirror-watcher" section for the
  actual observation/equivocation-detection/backfill mechanism).
  **This is also why the retention-tier mechanism above cannot yet
  deliver #180's actual availability guarantee** — there is only one
  database for a hot-tier node's pruning to be gated against, not a
  second archive-tier node — see this section's own honesty note.
- **Node-to-node announce/bootstrap discovery is real** (#362, implementing
  #292's decided design): `POST /nodes/announce`/`GET /nodes/peers`
  (`crates/server/src/nodes.rs`) — a lightweight, in-memory peer table
  (`network_id`, roles, protocol version, last-announced timestamp),
  keyed by `base_url`, never merged across `network_id`s.
  `AVALON_BOOTSTRAP_PEERS` names explicit peers; unset, a node falls back
  to its own network's `seed_nodes` in `docs/trusted-networks.json` (see
  [`./network-trust-anchors.md`](./network-trust-anchors.md)) — empty for
  a network with no anchor node yet, which is the expected state for a
  network's first node, not an error. `nodes::run_worker` re-announces on
  `AVALON_ANNOUNCE_INTERVAL_SECS` (default 180s) and prunes any peer not
  re-announced within a few multiples of that interval. Still not built:
  capability-aware routing (the SDK's own discovery, below, still takes a
  bare `server_url`) and any use of the peer table by settlement/mirror
  sync itself — this is purely peer discovery, independent of #40/#299's
  trust model. **One real consumer now exists**: #539's realtime relay
  (`crate::realtime_relay`, see [`presence.md`](./presence.md)/
  [`communication.md`](./communication.md)) reads each peer's `roles` to
  decide who a live presence/chat event gets forwarded to — the first
  place this peer table's `roles` field is actually read for anything
  beyond bookkeeping.
- **A second real consumer of the peer table: DHT bootstrap (#582, part
  of epic #580)**: `crate::dht`, gated behind `AVALON_DHT_ENABLED` (off by
  default at the time #582 landed; **on by default since ADR #593** — see
  this section's own note above). `PeerInfo` now also carries `libp2p_peer_id`/
  `libp2p_listen_addrs`; `dht::run_worker` scans the peer table every 30s
  for identities it hasn't dialed into its `rust-libp2p` `kad` swarm yet.
  Live-verified across the two-node LAN sandbox (`avalon-peer`): a peer
  announced over plain HTTP is picked up and successfully dialed into the
  DHT with no separate bootstrap step, completing a real noise handshake
  with the correct verified identity on both sides. A bootstrap-only
  connection like this idles and closes after ~10s (libp2p's default
  keep-alive timeout) since nothing yet asks anything of the DHT over
  it — expected, not a defect; #583's actual interest queries are what
  should keep a connection alive going forward. Also found live: a
  Docker-deployed node needs `AVALON_LIBP2P_EXTERNAL_ADDR` set (see
  `crate::dht`'s module doc) since it can never safely self-detect its
  own LAN-reachable address the way a native process can.
- **Interest registration and lookup over the DHT (#583, part of epic
  #580)**: `crate::interest` — a local guild-channel/conversation
  subscription (`crate::chat::handle_chat_socket`) now holds an
  `InterestGuard` for as long as it's subscribed. `interest::run_worker`
  `PutRecord`s this node's `AVALON_NODE_URL` under a hash of that
  channel/conversation id *immediately* on a scope's first subscriber
  (a real bug caught live: waiting for the first 45s refresh tick left a
  fresh subscription invisible to a lookup for far too long), then again
  every 45s (a 135s TTL, the same `PRUNE_INTERVAL_MULTIPLE`-style margin
  #362's peer table already uses) for as long as at least one subscriber
  remains — no explicit deregister call exists; a disconnected
  subscriber's guard drops, the refcount hits zero, and the record
  simply lapses. `interest::lookup` is a `GetRecord` against that same
  key, deduplicated (`get_record` was live-observed reporting the same
  value more than once — one per DHT replica that answered).
- **DHT-scoped relay delivery (#584, part of epic #580, closed)**:
  `crate::realtime_relay::relay_to_peers` now calls `interest::lookup`
  for `ChannelMessage`/`ChannelMessageDeleted`/`ConversationMessage`
  events instead of #539's original full peer-table loop — that loop
  still runs unconditionally for `Presence` (no channel/conversation
  scope exists to look up) and as the fallback for any node with no DHT
  identity (`AVALON_DHT_ENABLED` unset), so every pre-#584 deployment's
  behavior is unchanged. This node's own `base_url` is filtered out of a
  DHT lookup's results (a node with a local subscriber for the same
  scope it's relaying for would otherwise relay-POST to itself).
  Live-verified against two real, separately-running processes sharing
  one Postgres (`crates/server/tests/realtime_relay.rs`, `--ignored`),
  run both with `AVALON_DHT_ENABLED` unset (regression: unchanged
  behavior) and set (the new path actually exercised and delivering).
  **Cross-network reachability closed (issue #608)**: the DHT swarm's own
  Kademlia protocol id (and the informational `identify` exchange) are
  now namespaced by `network_id` (`crate::dht::kad_protocol_name`/
  `identify_protocol_version`) — a peer configured for a different network
  can no longer negotiate a single Kademlia RPC with this swarm at all,
  closing what used to be a purely incidental protection ("this doesn't
  leak across networks today only because #582's bootstrap can itself
  only ever reach peers already admitted into this same network's peer
  table") into a real, structural one. That closed the *network-boundary*
  question only — it didn't add any authorization for *which specific
  scope* an already-admitted, same-network node could register interest in.
  **Per-scope interest authorization (issue #610, closed):** a DHT
  `PutRecord` for a `Channel`/`Conversation` scope now has to carry a
  signed `avalon_protocol::interest_claim::InterestClaim` — the same
  Ed25519 event-signing key #525's session-continuation tokens already
  use, binding `identity_id` and, critically, the destination `base_url`
  itself into the signed bytes (so a claim observed in the DHT can't be
  republished under a different `base_url` to redirect delivery). Both the
  registering node (`crate::chat::handle_chat_socket`, before ever calling
  `InterestRegistry::register_with_claim`) and the relaying node
  (`crate::interest::lookup_claimed`, `crate::realtime_relay`'s only
  caller for these two scope kinds now) verify the signature; the relaying
  node separately re-checks *current* membership against its own local,
  ledger-derived membership tables (`crate::channels::is_member_of_channel`
  / `crate::conversations::require_unblocked_participant`) rather than
  trusting anything claimed in the record — a same-network node can no
  longer get real chat content relayed to it just by forging a raw
  `PutRecord` for a scope it isn't actually in. The browser side mints the
  claim with `packages/api-client/src/crypto/interestClaim.ts`, learning
  which `base_url` to bind against from a new `node_info` hello
  (`crate::chat::ChatServerMessage::NodeInfo`) sent once right after
  upgrade, since a browser has no other way to know its own node's
  announced address. A connection with no local signing key (or one that
  hasn't yet heard `node_info`) still subscribes and gets this node's own
  local `ChatBus` delivery — it just never registers DHT interest, so it
  won't receive delivery relayed *from* another node for that scope, a
  narrower version of the same tradeoff #525 already accepts for
  cross-node session continuation. Issue #596's mirror-sync reuse of this
  same DHT mechanism (`InterestScope::Network`) is unaffected — it's
  node-to-node, not identity-scoped, and lower severity to begin with
  (`mirror_push`'s own use never trusts DHT-discovered content directly,
  only triggers a re-poll `mirror_watcher` independently verifies). Live-
  verified: a claim naming a channel its own identity never joined is
  rejected by `lookup_claimed` even though its signature is perfectly
  valid, and a real member's claim is accepted
  (`crates/server/src/interest.rs`'s `claim_verification_live` tests,
  `--ignored`, against real Postgres); the full subscribe -> claim ->
  cross-node relay path re-verified against two real, separately-running
  processes (`crates/server/tests/realtime_relay.rs`,
  `realtime_reconnect.rs`, both `--ignored`).
- **Local Redis fast-path in front of the interest lookup (#585, part of
  epic #580)**: `crate::interest::RedisFastPath`, reusing #545's
  already-decided optional per-hoster `AVALON_REDIS_URL` — checked first
  on both registration and lookup, a same-fleet `SADD`/`SMEMBERS` shortcut
  in front of the real DHT `PutRecord`/`GetRecord`, never a replacement
  for it. Registration always writes to the DHT regardless of whether
  Redis is configured; a lookup only skips the DHT hop when Redis
  actually has a non-empty answer, falling straight through otherwise —
  unset, unreachable, and empty are all handled identically. Live-
  verified against a real Redis instance
  (`crates/server/tests/interest_dht.rs`'s
  `redis_fast_path_answers_a_lookup_even_when_the_dht_channel_is_dead`,
  `--ignored`, requires `AVALON_REDIS_URL`): the lookup is handed a
  deliberately-dead DHT command channel, so the only way it can find the
  right answer is Redis alone. **Known simplification, accepted rather
  than solved**: a scope's Redis entry has one whole-key TTL, not a
  per-member one, so a departed fleet member's `base_url` can keep
  appearing in a fast-path answer for as long as any other fleet member
  keeps refreshing that same scope — worst case one extra harmless relay
  POST, never a missed delivery, since the DHT remains the correctness
  backstop.
- **Automatic peer-set growth and shard discovery (#599)**:
  `nodes::run_worker` no longer announces only to `AVALON_BOOTSTRAP_PEERS`
  forever — it keeps a growing `active_peers` list (bootstrap peers plus
  peers promoted from discovery, capped by `AVALON_NODE_MAX_PEERS`,
  default 50), and `AnnounceRequest`/`AnnounceResponse` now also gossip a
  `ShardRegistry` snapshot bidirectionally. `crate::cross_shard`'s
  aggregation and `crate::mirror_watcher`'s opt-in
  `AVALON_MIRROR_ALL_DISCOVERED_SHARDS` both consume the same registry —
  see [`settlement.md`](./settlement.md)'s "Automatic shard discovery"
  section for the full design and live-verification results (a genuine
  three-machine topology: this sandbox, `avalon-peer`, and
  `avalon-peer-two`).
- **One-hop live realtime relay across nodes (#539, implementing #535's
  decision)**: `POST /nodes/relay` (`crate::realtime_relay`) — see
  [`presence.md`](./presence.md) and [`communication.md`](./communication.md)'s
  own "Today in the repo" entries for the full mechanics. No auth (same
  posture `GET /nodes/peers` already takes); single-hop by construction,
  not by an origin-tracking field — see #542 for what changes once the
  peer mesh stops being small and fully interconnected.
- **Mirror-watcher verification is per-`network_id`, not one process-wide
  key** (#513/#515, closing a real gap the two-node LAN sandbox surfaced:
  `AVALON_MIRROR_PEERS` used to be verified against a single globally
  loaded key, so a peer claiming any `network_id` other than the one that
  key happened to match either silently mirrored under its own name or
  failed for the wrong reason — there was no actual check that the peer's
  claimed network was one this node was meant to trust at all).
  `crates/server/src/mirror_watcher.rs::verify_key_for_network` now
  resolves each peer's verify key from `docs/trusted-networks.json` by its
  *own* claimed `network_id` at verification time — a node can correctly
  mirror peers across more than one legitimately pinned network, and a
  peer claiming an unpinned `network_id` is refused outright
  (`MirrorWatcherError::UnpinnedNetwork`), not silently stored. Adding a
  trust anchor for a private/local network requires an entry in
  `docs/trusted-networks.json`, not just an env var.
- **Push-based mirror sync (#596), on top of #299's polling, not
  replacing it.** Two deliberate tiers now exist, not a legacy fallback
  bolted onto a new feature: *(1) permissionless* — any mirror can watch
  `AVALON_MIRROR_PEERS` with zero registration, exactly as before this
  ticket, forever; *(2) push-registered* — a mirror additionally gets a
  low-latency nudge the moment a peer it mirrors commits something new.
  Addressing reuses #583's DHT-backed interest registration rather than a
  new mechanism: `mirror_watcher::run_worker` registers this node's
  interest in every `network_id` it successfully verifies an STH for
  (`InterestScope::for_network`, `crates/server/src/interest.rs`) — never
  an unverified one, so a hostile or unpinned network_id claim is never
  registered. `crates/server/src/outbox.rs`'s drain worker, right after a
  batch it just committed *locally* (never after a remote-submit — that
  authority's own outbox pushes when it commits), resolves who's
  registered for that `network_id` via the same DHT lookup and POSTs a
  small `{network_id, tree_size}` body to each
  (`crate::mirror_push::notify_peers`). **Delivery transport is plain
  HTTP to the peer's known base URL**, not a libp2p stream — chosen
  because a `reqwest::Client`, the peer's reachable `base_url` (from
  #362's peer table via interest registration), and an axum route to
  receive it all already exist for this exact shape of problem, while a
  stream would need a new request-response protocol added to
  `crate::dht`'s swarm (today only Kademlia put/get plus `identify`) for
  a one-shot notification that never needs a persistent connection. The
  DHT still does the actual addressing — only the last hop is HTTP.
  `POST /mirror/notify`'s handler never trusts the body for anything: it
  only wakes `mirror_watcher::run_worker`'s loop early
  (`AppState::mirror_wake`, a `tokio::sync::Notify`), which then runs its
  exact existing verify/corroborate/backfill pipeline — #300/#316's
  multi-peer corroboration gate applies completely unchanged, since a
  push only changes *when* a tick runs, never what it trusts once it
  does. A missed or dropped push is never fatal: the same loop still
  falls back to its normal poll tick regardless. Because push now covers
  the common case, `AVALON_MIRROR_POLL_INTERVAL_SECS`'s default rose from
  30s to 120s — poll's job for a push-registered peer shrinks to
  "bound worst-case staleness if a push is ever missed," which doesn't
  need a 30s cadence, and the wider default also cuts every deployment's
  at-idle polling overhead 4x regardless of whether push is reaching it.
  `crates/server/tests/mirror_push.rs` (`--ignored`) is written against
  two/three real, separately-running `avalon-server` processes sharing one
  Postgres — see that file's own module doc for the exact setup. **Now
  fully live-verified end to end (issue #605)**, against an isolated
  Postgres schema (not the shared sandbox database, whose accumulated
  history and stale equivocation state made cold convergence slow and
  unreliable to test against) and with node startup deliberately
  sequenced — the authority given one real committed event before either
  mirror starts, since a mirror's first poll tick has to succeed to
  register DHT interest at all. Both headline assertions
  (`a_push_notification_measurably_reduces_observed_mirror_sync_latency`,
  `mirroring_still_converges_via_poll_alone_when_push_is_unavailable`)
  pass. The test's own `enqueue_real_event` helper had a real bug found
  during this verification: it hand-built the `protocol_outbox` row's
  JSON instead of serializing an actual `ProtocolEvent`, using an RFC3339
  string for `timestamp` where `OffsetDateTime`'s derived (non-human-
  readable) wire format was expected — every event that helper enqueued
  was silently dropped by the outbox worker as unparseable, so neither
  node ever saw a second commit to converge on. Fixed by building a real
  `ProtocolEvent` and serializing it with `serde_json::to_value`, exactly
  as `crate::outbox::enqueue` does.
- No SDK-side discovery yet: `AvalonConfig { server_url, .. }` in
  `crates/sdk/src/lib.rs` still takes a bare URL — #362's peer table is a
  server-to-server mechanism, not yet consumed by client-side routing.
- No export format for the log (that's #40).
- **A real, physically-separate second node exists, is live-verified, and
  now plays a dual role** (`avalon-peer`, a distinct host on the same LAN,
  `~/avalon-protocol` there, always reachable via `ssh avalon-peer` in this
  sandbox — see `.claude/CLAUDE.md`). Deployed with `make stack-up-no-redis`
  (its own Docker-Compose Postgres container, genuinely separate storage
  from the primary's), `AVALON_NETWORK_ID=avalon-dev-local` (matching the
  primary, required for its STHs to verify against the same pinned trust
  anchor — see `docs/trusted-networks.json`).
  - **Mirrors the primary's core shard** (`AVALON_MIRROR_PEERS=http://<primary-LAN-IP>:8080`).
    Fully converged (its `mirrored_entries` count matches the primary's
    real `tree_size` exactly, same root hash) via the existing
    mirror-watcher/backfill mechanism, no new code. **Read availability
    during an outage is live-proven, not just structural**: with the
    primary process stopped entirely (`make stop`, confirmed
    connection-refused), `avalon-peer`'s `GET /ledger/sth/latest` and
    `GET /ledger/entries` kept answering correctly from its own
    independently-verified data — see the retention section's updated
    honesty note above for exactly what this does and doesn't close
    (write availability during an outage is now decided — #609/#630 — as
    a manual promotion runbook, not automatic failover).
  - **Also independently authors a second, distinct shard** (2026-09-17):
    `AVALON_SETTLEMENT_SIGNING_KEY` set to a real, registered
    `shard_settlement` operational key (not a shared node-operator key —
    see `docs/architecture/settlement.md`'s "Cross-machine, real end to
    end" section for the full proof). Nothing here needed new code —
    mirroring and authoring are independent capabilities a node can hold
    simultaneously, and #532's routing + #543's DB-resolved trust already
    composed correctly the first time they were pointed at two genuinely
    separate machines.
- **Archive-confirmation gating (#569) is real, implemented, and
  live-verified.** `GET /ledger/mirror-progress?network_id={id}`
  (`crate::settlement::mirror_progress`) plus
  `avalon_chain::retention::RetentionConfig`'s `archive_peers`/
  `min_archive_confirmations` plus `avalon_server::retention::confirm_archive_coverage`
  (the actual HTTP check, run before every pruning pass in
  `crate::retention::prune_once`) — see the retention section above for
  the full mechanism and what it closes. Unit-tested with an in-process
  fake-peer HTTP server (`crates/server/src/retention.rs`'s own tests);
  live-verified end to end against a real, isolated Postgres database
  (`crates/server/tests/retention_archive_confirmation.rs`, `--ignored`):
  real committed entries, pruning genuinely blocked while a configured
  peer hadn't mirrored far enough, then genuinely proceeded once it had.
  `avalon prune-ledger`'s manual entry point deliberately does not perform
  this check itself (no `reqwest` in a `--no-default-features` build) and
  refuses to run a real prune when the loaded config requires it, rather
  than silently bypassing the gate the background worker enforces.
- **Minimum replication guarantee for new registrations (#629,
  implementing #622's decision) is real, implemented, and
  live-verified.** #569's mechanism above answers "is it safe for *this*
  node to stop keeping a full local copy of its own history"; #629
  answers a different question at a different trigger point — "does a
  shard have enough independently-confirmed mirrors to be trusted with a
  *brand-new* identity registration at all." A single-operator shard has
  zero durability guarantee beyond that operator's own uptime, and a
  locator that resolves an identity to a shard (epic #623, closed) is
  only as useful as the guarantee that shard's data actually persists
  somewhere.
  - **Mechanism, `crates/server/src/replication.rs`.** A background
    worker (`replication::run_worker`, spawned unconditionally at
    startup, same posture the #362 announce worker takes) polls every
    known peer's (`crate::nodes::PeerTable`) own
    `GET /ledger/mirror-progress?network_id={id}&shard_id={id}` for every
    known shard (this node's own `own_shard_id` plus whatever
    `crate::nodes::ShardRegistry` has gossiped in) on a
    `AVALON_REPLICATION_POLL_INTERVAL_SECS`-second cadence (default
    300s). Unlike #569's boundary-`seq` question, this only asks whether
    a peer has mirrored *anything at all* for that shard (`last_seq >
    0`) — populating `replication::MirrorConfirmationRegistry`, an
    in-process, non-durable count of distinct confirming peers per
    shard, same posture every other piece of `crate::nodes` gossip state
    already takes.
  - **The gate itself**, `crate::replication::registration_eligible`
    (pure, unit-tested), is consulted once, at the very start of
    `crate::handlers::register_start` — before the WebAuthn ceremony or
    even the display-name/identity-id uniqueness checks, since neither
    matters if the shard itself isn't eligible. A shard is eligible for
    a *new* registration when either it's still within its bootstrap
    grace period, or it has at least `AVALON_MIN_MIRROR_CONFIRMATIONS`
    (default `1`) distinct, currently-fresh (within 15 minutes)
    confirmed mirrors. A shard already past the grace period with too
    few confirmed mirrors gets a new `AppError::ShardBelowMinimumReplication`
    (HTTP 503) — the request is otherwise well-formed, this is a
    temporary, shard-wide condition, not a rejection of the caller.
    **Never disrupts an identity already registered on the shard** —
    this is only ever consulted at the start of a brand-new
    registration.
  - **The bootstrap decision**: a brand-new, legitimately
    single-operator shard hasn't had time to attract a mirror yet, so
    `AVALON_MIRROR_GRACE_PERIOD_HOURS` (default `24`) exempts a shard
    from the gate entirely while it's younger than that window. "How old
    is this shard" comes from `crate::nodes::ShardRegistry::first_seen_at`
    — the *earliest* `last_seen_at` this node has ever recorded for that
    shard, across both its own `record_own` claims and peer gossip
    (deliberately tracked separately from the registry's existing
    per-URL `last_seen_at`, which only ever tracks the newest
    observation). A shard whose age can't be determined at all yet
    (`None` — e.g. right after this node's own restart, before either
    worker has run a first tick) is treated as within the grace period:
    the safe direction for this specific unknown to lean is "briefly,
    harmlessly exempt," never "wrongly gate a shard this node simply
    hasn't observed yet."
  - **Visible before the fact, not just as a rejection reason**:
    `GET /nodes/status` now includes an `own_shard_replication` block
    (`confirmed_mirror_count`, `min_confirmations_required`,
    `first_seen_at`, `within_grace_period`,
    `eligible_for_new_registrations`) — issue #599's shard registry is
    where operators already look for shard-existence facts, and this is
    the same idea applied to a shard's durability, so an operator (and
    eventually an end user choosing where to register) can see it before
    trusting a shard with anything, not just discover it via a failed
    registration attempt.
  - **Deliberate v1 simplifications**, called out explicitly rather than
    left implicit: one configured minimum applies uniformly to every
    shard regardless of whether it's a `core`-like, identity-bearing
    shard or an individual integrator's own dedicated one — no
    per-shard-type configuration surface exists yet. And because
    `ShardRegistry`/`MirrorConfirmationRegistry` are both in-process and
    non-durable (same posture every other piece of `crate::nodes` state
    already takes), a shard's apparent age resets to "unknown" (treated
    as within grace period) on this node's own restart — a restart can
    only ever briefly re-extend a shard's grace period, never wrongly
    lock it out, which is the safe direction for that specific gap to
    lean.
  - **Tested**: pure grace-period/count logic and the
    `MirrorConfirmationRegistry`/`ShardRegistry::first_seen_at`
    bookkeeping are unit-tested (`crates/server/src/replication.rs`,
    `crates/server/src/nodes.rs`) with an in-process fake-peer HTTP
    server for the actual `GET /ledger/mirror-progress` polling call;
    live-verified end to end with multiple local `avalon-server`
    processes against real Postgres (distinct schemas, one primary node
    plus peer nodes acting as mirrors) — a shard within its grace period
    accepted registration regardless of confirmed-mirror count, a shard
    forced past a near-zero grace period with no confirmed mirrors
    rejected registration with `SHARD_BELOW_MINIMUM_REPLICATION`, and
    registration succeeded again once a peer's confirmed mirroring was
    recorded.
- No distinct "archive" node *type*/binary exists, and #208 deliberately
  didn't invent one: retention tier is operational configuration on the
  one existing Settlement role (`AVALON_RETENTION_TIER=full`), not a fifth
  capability alongside Settlement/Indexer/Realtime/Gateway in the table
  above. An "archive operator" today is simply an operator running
  `avalon-server` with `AVALON_RETENTION_TIER=full` (the default) and
  `AVALON_RETENTION_PRUNING_ENABLED` left unset — nothing about the
  capability table changes; only the retention-tier config differs between
  a full and a hot deployment of the same Settlement role.
- **The first real instance of "a node without its own Settlement"**
  ([#313](https://github.com/LunarVagabond/avalon-protocol/issues/313),
  implementing the decision on
  [#296](https://github.com/LunarVagabond/avalon-protocol/issues/296)): an
  `avalon-server` process can now run Indexer/Realtime/Gateway against its
  own local Postgres while deferring *writes* to a separate Settlement
  authority, still inside the same single binary/single-database-per-node
  shape — no separate deployable roles yet (that's the full
  [#291](https://github.com/LunarVagabond/avalon-protocol/issues/291) epic).
  Two independent config knobs:
  - **Reads** stay exactly #299's mirror-watcher mechanism, extended by one
    step: once `mirror_watcher::backfill` verifies and stores an entry into
    `mirrored_entries`, it now also decodes it into a `ProtocolEvent` and
    applies it to this node's own `PostgresIndexer`
    (`avalon_indexer::postgres::PostgresIndexer::apply_in_tx`), in the same
    transaction as the `mirrored_entries` write. A remote-settlement node's
    local reads are therefore fed *only* by content this node
    independently verified itself (inclusion-proof-checked against a
    signature-checked STH) — never by trusting a peer's response, and
    never by trusting `POST /ledger/submit`'s own request body.
  - **Writes**: `AVALON_SETTLEMENT_REMOTE_URL`, read by
    `crates/server/src/outbox.rs`'s `run_worker`. When set, a drain tick
    posts its `EventBatch` to `POST /ledger/submit` on the named authority
    instead of calling `chain.commit` locally; the authority runs the exact
    same `chain.commit` call there that it runs for its own local outbox.
    Unset (the default), the worker's behavior is completely unchanged.
    Exactly one Settlement authority is ever configured as a write target
    per shard — no automatic failover across multiple peers, matching
    #70/#186's "no contested writes" invariant. **Shard-aware since #532**:
    see [`settlement.md`](./settlement.md)'s "Write routing to the correct
    shard" section — `AVALON_SETTLEMENT_REMOTE_URLS` (a per-shard map,
    keyed by the event's own `GlobalId` namespace/owner) generalizes the
    singular var above, which keeps working unchanged as that map's
    implicit single `core` entry. A milestone-1 deployment with neither
    var set has exactly one shard (`core`) and behaves exactly as before
    #532 existed.
  - This is eventually consistent, not synchronous: a write accepted by a
    remote-settlement node's own API is durably committed on the authority
    immediately, but that node's own local reads only pick it up once
    mirror-watcher's next backfill tick applies it — real latency (bounded
    by `AVALON_MIRROR_POLL_INTERVAL_SECS`, 30s by default), not hidden.
  - `POST /ledger/submit` is the one write route among `settlement.rs`'s
    otherwise-public mirror-facing endpoints — see
    [`settlement.md`](./settlement.md) for its authentication.
  - Still not built (explicitly out of scope for #313, deferred to #291):
    separate deployable binaries per role, automatic multi-peer write
    failover, role-aware/partial migrations (a remote-settlement node still
    runs every migration, including settlement-only tables it never
    writes to), and general node discovery/routing (the remote write
    target is a fixed config value, not discovered).

- **Hoster-configurable resource limits** (#287/#363): every deployment of
  `avalon-server`, single-process or otherwise, now has four independently
  tunable safety floors, all defaulted to exactly what this process always
  hardcoded — an unconfigured node behaves exactly as it always has.
  - `AVALON_MAX_DB_CONNECTIONS` (default `10`) — `PgPoolOptions::max_connections`
    in `crates/server/src/main.rs`.
  - `AVALON_MAX_CONCURRENT_REQUESTS` (default `256`) —
    `tower::limit::ConcurrencyLimitLayer` in `crate::router`'s middleware
    chain. Backpressures (bounded wait) past the ceiling, never drops a
    request without a response.
  - `AVALON_RATE_LIMIT_PER_MINUTE` (default `600`) — `tower_governor`
    (GCRA), keyed by the caller's `x-avalon-integrator-key-id` header when
    present (`IntegratorOrIpKeyExtractor`, `crate::lib`), falling back to
    peer IP for pre-auth endpoints (registration, login) that don't carry
    an integrator key yet. Requires `into_make_service_with_connect_info`
    (see `main.rs`) for that IP fallback to resolve. Over-limit responses
    are always `429` with `Retry-After`.
  - `AVALON_OUTBOX_POLL_INTERVAL_SECS` (default `3`) — the outbox worker's
    drain cadence (`crates/server/src/outbox.rs::run_worker`).
  - Live-tested (`crates/server/tests/resource_limits.rs`): a rate-limited
    burst gets `429` + `Retry-After`; a concurrency-limited burst still
    completes every request (backpressure, not a silent drop) but
    measurably serializes.
  - **Per-process by default; optionally shared per-hoster (#537, decided
    #545).** Both the GCRA rate-limit bucket and the concurrency counter
    live entirely in one process's memory by default, with no shared
    backing store across processes — an operator running more than one
    `avalon-server` process gets that many independent copies of each
    ceiling unless they opt in. `AVALON_REDIS_URL` (issue #545,
    `crate::redis_limits`) makes both limits Redis-backed instead,
    **strictly scoped to that one hoster's own processes** — never a
    network-wide shared limiter (that would recreate exactly the single-
    point-of-control problem #527/#535 exist to remove; this only lets one
    operator's own N processes agree with each other, the same way their
    own `DATABASE_URL` already does). Unset (the default), behavior is
    unchanged from before #545 existed. Rate limiting: fixed-window
    `INCR`+`PEXPIRE`, atomic via a Lua script — an honest, documented
    tradeoff (up to ~2x burst right at a window boundary), strictly better
    than today's real gap (no cross-process limit at all). Concurrency:
    a Redis sorted set of in-flight request ids, pruned of anything
    stale (a crashed process's leaked slot self-heals on the next
    admission check, not stuck forever), backpressuring the same way the
    in-process `ConcurrencyLimitLayer` does rather than dropping. Both fail
    open (log a warning, let the request through) if Redis itself becomes
    unreachable, rather than taking the whole node down over a rate-limit
    backend outage. Live-verified against two real `avalon-server`
    processes sharing one Redis: the rate limit measurably held across
    both (spending most of the budget on one process starved the other,
    rather than granting it a fresh allotment), and a burst split across
    both nodes backpressured together instead of each node independently
    absorbing its own half (`crates/server/tests/redis_resource_limits.rs`).
- **Host resource metrics on `GET /nodes/status` (#517)**: a `resources`
  block — CPU core count/usage %/load averages, memory and swap used/total,
  disk used/total for the node's own storage path (`AVALON_NODE_STORAGE_PATH`,
  defaulting to the process's working directory) and, when set, a locally-
  readable Postgres data directory (`AVALON_POSTGRES_DATA_PATH` — unset,
  and simply omitted, for the common case of a remote/managed Postgres this
  sandbox's own deployment already uses), process uptime, this process's
  open-file-descriptor count as the cheapest cross-platform proxy for
  "connections/handles currently held open," and the DB pool's own tracked
  `size`/`in_use` (`sqlx::PgPool::size`/`num_idle`, no new instrumentation).
  Built on the `sysinfo` crate rather than hand-rolled `/proc` parsing,
  specifically because it abstracts Linux/macOS/Windows/BSD internally —
  hosters aren't guaranteed to run Linux. CPU usage % needs a delta between
  two `sysinfo` refreshes to mean anything, so a background task
  (`crate::resources::start_sampler`, spawned unconditionally at startup)
  keeps a shared, periodically-refreshed snapshot that the request handler
  just reads, rather than blocking each `/nodes/status` call on a fresh
  sample. Every leaf field is independently optional and best-effort — a
  metric this process can't read on the host it happens to be running on
  is `None`, never a failed request. See `crates/server/src/resources.rs`.
- **Operator-internal node-to-node RPC (#661), the foundation epic #291's
  four role-extraction tickets (#662 Indexer, #663 Realtime, #664
  Settlement, #665 discovery/routing) build on.** Once a Gateway process
  and its backing Indexer/Realtime/Settlement processes are genuinely
  separate, a Gateway's call sites need a way to reach a role that isn't
  in-process anymore — this is that mechanism, built once so the four
  extraction tickets don't each invent their own wire format.
  **Deliberately distinct from #40's `/ledger/*` mirror-sync protocol**:
  that one is multi-operator and trust-minimized (independent Settlement
  nodes verifying and mirroring one log); this one is operator-internal —
  one deployment's own processes talking to each other, no independent
  verification needed on either side. HTTP+JSON, matching every other
  endpoint in this codebase, gated on a *third* shared-secret bearer
  token (`AVALON_INTERNAL_ROLE_KEY`, `AppState::internal_role_key`) —
  deliberately never reused from `AVALON_SETTLEMENT_SUBMIT_KEY` (#313,
  can be shared across two nodes so they can submit to each other's
  ledger) or `AVALON_ADMIN_TOKEN` (#658, meant to be held by only one
  process's own operator console); unset means every request under
  `/internal/*` is refused, same "closed by default" posture the other
  two keys establish. `crate::internal_role` is the whole module — its
  own doc comment has the full design writeup. The pattern is proven
  concretely, not just designed on paper: `avalon_indexer::Indexer`
  (#42 — `apply`/`rebuild`) was picked as the first target, since
  `avalon-indexer` already depends only on `avalon-protocol`, not
  `avalon-server`. `POST /internal/indexer/apply` and
  `POST /internal/indexer/rebuild` are thin server-side twins of the
  in-process calls, and `RemoteIndexer` is a real `Indexer` implementation
  backed by HTTP calls to them — any code holding an `impl Indexer` can't
  tell it apart from a local `PostgresIndexer`. A remote role that's
  unreachable (down, or just hung — `RemoteIndexer`'s HTTP client carries
  its own 10s timeout so an unresponsive peer can't make a caller hang
  too) surfaces as `IndexError::RemoteUnreachable`, mapped by
  `AppError::RemoteRoleUnreachable` to a `503` — a real dependency-down
  signal, never conflated with "the data doesn't exist" (`404`) or a
  generic storage bug (`500`). Live-verified: two real `avalon-server`
  processes against the same Postgres via distinct schemas, one acting as
  the Indexer role (real `PostgresIndexer` behind the new endpoints), the
  other using only a `RemoteIndexer` pointed at it — `apply`/`rebuild`
  over the network produce results identical to the same calls made
  in-process against `PostgresIndexer` directly, and killing the Indexer
  process mid-request produces the clean `RemoteRoleUnreachable`/`503`,
  not a hang or a misleading error. **Not wired into `avalon-server`'s own
  startup path** — every real handler still calls `PostgresIndexer`
  in-process (several, like `register_finish`/`update_profile`, share a
  Postgres transaction with their own app-data writes via
  `PostgresIndexer::apply_in_tx`, a coupling that has to be unpicked
  first) — that wiring, and turning this from "proven pattern" into
  "an actual Gateway-only deployment mode," is #662's job.
- **Extracting the Indexer role into its own deployment mode (#662).**
  `AVALON_NODE_ROLES` is load-bearing now, for the Indexer role
  specifically — this is the point where "exactly one node type exists"
  (this section's own opening claim) stops being true. `AppState.indexer`
  is no longer a bare `PostgresIndexer`; it's `IndexerHandle`
  (`crates/server/src/state.rs`), an enum over `Local(PostgresIndexer)`
  and `Remote(RemoteIndexer)`. `main.rs` decides which at startup, off
  `nodes::indexer_role_is_local(&nodes::node_roles())`: any role list
  including `"indexer"` or `"combined"` (the default) gets `Local`,
  exactly today's behavior. Anything else needs `AVALON_INDEXER_REMOTE_URL`
  (and `AVALON_INTERNAL_ROLE_KEY`) set — reusing #661's existing env var
  rather than inventing a second name for the same concept — and gets
  `Remote`; if that URL is unset, the process refuses to start
  (`tracing::error!` + `std::process::exit(1)`, the same "refusing to
  start" posture the `network_id` genesis-mismatch and DHT-config checks
  above already establish), rather than silently falling back to a local
  index or serving reads that would quietly diverge from what a real
  Gateway-only deployment needs. Every call site that used to call
  `PostgresIndexer::apply_in_tx` directly (~20, across `handlers.rs` and
  every other domain module) now goes through `IndexerHandle::apply_in_tx`
  followed by `IndexerHandle::apply_after_commit` once its transaction has
  committed — see `docs/architecture/query-and-indexing.md`'s "Today in
  the repo" section for what those two calls actually do for each variant,
  and the real consistency tradeoff `Remote` accepts. `mirror_watcher.rs`
  was deliberately left alone: it already builds and uses its own
  independent local `PostgresIndexer` (never through `AppState.indexer`),
  since mirror-sync applies verified peer entries straight to this
  process's own local Postgres regardless of `AVALON_NODE_ROLES` — a
  different concern (mirror sync) from the Gateway/Indexer role split this
  ticket is about, so it keeps its own local indexer rather than being
  routed through `IndexerHandle`.
- **Realtime extracted into its own deployable role (#663), the second of
  epic #291's four extraction tickets to land.** Unlike Indexer (#662),
  an open WebSocket connection is stateful — it has to terminate
  somewhere real for its whole lifetime, not just answer one request at a
  time — so this ticket had a genuine connection-topology decision to
  make first: proxy every client WebSocket connection through the
  Gateway to a remote Realtime node, or tell the client to connect to the
  Realtime node directly. **Decided: proxy-through-Gateway**, recorded as
  a closed ADR,
  [#672](https://github.com/LunarVagabond/avalon-protocol/issues/672) —
  keeps the "client always talks to one node's URL" invariant every other
  role extraction in this epic preserves, and doesn't need #665's
  discovery/routing work to exist first. `AVALON_NODE_ROLES` excluding
  `realtime` now requires `AVALON_REALTIME_URL` (a reachable remote
  Realtime node's base URL) or the process refuses to start
  (`crate::nodes::realtime_mode_from_env`) — same "fail loudly, never
  silently degrade" posture every other startup-time check in `main.rs`
  already takes. `presence::presence_ws`/`chat::chat_ws` still
  authenticate the caller locally first (a bad token still gets a real
  401 before any upgrade, on either path); when a remote URL is
  configured, the upgraded socket is handed to
  `crate::realtime_proxy::proxy_websocket`, which dials the remote node's
  identical endpoint (forwarding the same session token) and pumps
  `Text`/`Binary`/`Ping`/`Pong`/`Close` frames bidirectionally until
  either side closes — a genuinely separate remote process it's talking
  to, not a second local code path pretending to be one. Audited (per the
  ticket's own ask) whether `crate::chat`'s `ChatBus` fan-out and
  `crate::interest`'s DHT interest registration assume in-process access
  that breaks once Realtime is a separate process: they don't need
  changes. `crate::realtime_relay` (#539/#584) already posts every
  locally-originated presence/chat event to any same-network peer
  advertising a `realtime`/`gateway`/`combined` role via #362's peer
  table (DHT-scoped via #584's interest registry when available) — a
  dedicated Realtime node is exactly such a peer, so a REST mutation
  handled by a Gateway (e.g. `PUT /me/presence`,
  `POST .../messages`) still reaches it exactly as it would reach any
  other realtime-capable peer, with no new relay logic needed. And since
  the proxied connection is a dumb byte pipe, `handle_presence_socket`/
  `handle_chat_socket` — and therefore `crate::interest`'s
  `InterestGuard` registration for a subscribed channel/conversation —
  still run entirely on the Realtime node itself, exactly as they would
  if the client had dialed it directly. Live-verified: a real Gateway
  process (`AVALON_NODE_ROLES=gateway`) and a real Realtime-only process
  (`AVALON_NODE_ROLES=realtime`) against the same Postgres, a real
  WebSocket client connecting to the Gateway's `/ws/presence` and
  receiving a presence update genuinely published by a second identity's
  `PUT /me/presence` call against the Gateway — relayed to the Realtime
  process and pushed down the proxied connection — plus the Realtime
  process restarting mid-session producing a clean `Close` frame on the
  client's proxied connection rather than a hang
  (`crates/server/tests/realtime_proxy.rs`, `--ignored`). **Known gap,
  not solved here**: this issue only extracts the WebSocket-serving
  decision — a Gateway configured this way still constructs a full local
  `PresenceStore`/`ChatBus`/`InterestRegistry` in `AppState` (unused for
  locally-terminated sockets, fed only by whatever the relay mesh
  delivers to `apply_relayed`/`publish_*`), since splitting `AppState`
  itself apart per role is out of this ticket's scope. **Also a known
  gap**: whether Hub/mobile-hub's own client-side WebSocket code
  reconnects cleanly after the kind of clean-`Close`-then-drop this
  proxy now produces on a Realtime restart hasn't been verified against
  real frontend reconnect logic — the live test above confirms the
  *server* side never silently hangs, not that every current client
  already reconnects gracefully.
- **Settlement as its own standalone node has landed (#664)**, the third
  of #291's four role-extraction tickets — a genuinely separable
  Settlement deployment, not just #313's remote-submit *config flag*
  inside a combined binary (that was real, working infrastructure this
  ticket builds on, but it never took Gateway modules out of the process
  at all — every WebAuthn/session/guild/presence handler stayed mounted
  and reachable either way). Same mode-flag-based single-binary shape
  #662/#663 use: `AVALON_NODE_ROLES` resolving to *exactly* `["settlement"]`
  (`crates/server/src/nodes.rs::is_settlement_only`) is the one predicate
  `main.rs` gates on. When it's true: no `AVALON_WEBAUTHN_RP_ID`/`ORIGIN`
  required (a harmless placeholder `Webauthn` instance is built instead,
  since `AppState::webauthn` is a plain `Arc<Webauthn>` and every Gateway
  handler that would touch it is never mounted); `avalon_server::router_settlement_only`
  (`crates/server/src/lib.rs`) replaces `router` and mounts only `/ledger/*`
  (`crate::settlement`'s full read+write surface, including #531's
  managed-hosting two-phase `prepare-batch`/`finalize-batch` and #529's
  cross-shard-root), `/nodes/{announce,peers,status,log-level}` (peer
  discovery/ops, not Gateway-facing), and `/mirror/notify` (#596's push
  wake) — no identity/session/social/guild/achievement/integration route
  at all; and three Gateway-only background workers (`outbox::run_worker`,
  the guild-message archive-expiry worker, and #635's identity locator) are
  never spawned, since none of them have anything to do when no Gateway
  handler ever runs on this process. Retention pruning, the mirror-watcher,
  the minimum-replication-guarantee worker, and node-to-node
  announce/bootstrap all keep running exactly as before — genuinely
  Settlement-side concerns, not gated on this ticket's predicate at all.
  **The mirror-image case — a Gateway-only node with no local Settlement
  role, pointed at a remote one — turned out to need no new mechanism**:
  #313's `AVALON_SETTLEMENT_REMOTE_URL(S)` already worked standalone before
  this ticket (a `combined`-labeled node with remote-submit configured is
  already, functionally, "Gateway-only + remote Settlement"); this ticket
  just confirmed that live rather than assuming it, and added one startup
  warning (roles excluding `settlement` with no remote-submit configured
  falls back to committing locally despite the declared role, same as
  before this ticket — never a hard failure, since `AppState::chain` is a
  required field regardless of role) so that combination isn't silently
  invisible to an operator. So **one config surface, not two**: Settlement-
  only, Settlement+Gateway combined, and Gateway-only-pointed-at-remote are
  three points on the same `AVALON_NODE_ROLES` + `AVALON_SETTLEMENT_REMOTE_URL(S)`
  surface, not separate flags. Live-verified: two real `avalon-server`
  processes against the same Postgres via distinct schemas (the
  `feedback_live_verify_two_local_processes` pattern) — a Settlement-only
  node 404s cleanly on `/identities/register/start` and `/me` (no panic,
  no route that happens to work) while `/ledger/sth/latest` still serves;
  a separate Gateway-only node (`AVALON_NODE_ROLES=gateway`,
  `AVALON_SETTLEMENT_REMOTE_URL` pointed at the Settlement-only node's
  port) registered a real identity through a full WebAuthn ceremony (a
  software authenticator, same shape `tests/identity_locator.rs` already
  established), and that identity's `identity.passkey_registered`/
  `identity.signing_key_added` entries appeared on the Settlement-only
  node's own `/ledger/entries`, with `/ledger/sth/latest`'s `tree_size`
  past them (`crates/server/tests/settlement_only.rs`, `--ignored`).

## Decisions and tickets

- [#291](https://github.com/LunarVagabond/avalon-protocol/issues/291)
  (Node Role Separation) — [#661](https://github.com/LunarVagabond/avalon-protocol/issues/661)
  (implemented) is its foundational sub-issue: the operator-internal
  node-to-node RPC protocol #662 (extract Indexer), #663 (extract
  Realtime), #664 (Settlement as its own node), and #665
  (discovery/routing) all build on — see the "Today in the repo" section
  above for the transport/auth decisions and the proven `Indexer`-over-HTTP
  instance. [#663](https://github.com/LunarVagabond/avalon-protocol/issues/663)
  (implemented) is the second to land — see
  [#672](https://github.com/LunarVagabond/avalon-protocol/issues/672)
  (closed ADR: proxy-through-Gateway, not direct-connect) and the "Today
  in the repo" section above for the full writeup.
- [#642](https://github.com/LunarVagabond/avalon-protocol/issues/642)
  decided (cross-node login's phishing-context requirement): no hard
  registered-integrator gate on the approval prompt, a visual
  verified-vs-unverified distinction instead, and mobile-hub's QR flow
  must never auto-approve — see the "Today in the repo" section above.
  [#649](https://github.com/LunarVagabond/avalon-protocol/issues/649)
  (closed, implemented) is the concrete server-side follow-up: actually
  resolving verified-vs-unverified status — see the "Today in the repo"
  section above.
- #70 mirrors of a public log, not federation
- #79 long-term settlement backend; #186 decided no blockchain/validator
  consensus (transparency log on Postgres instead, superseding part of #93);
  #40 log design and mirror sync (validator/consensus design dropped) —
  [#299](https://github.com/LunarVagabond/avalon-protocol/issues/299)
  (implemented) is the actual mirror-watcher built against that design.
  [#596](https://github.com/LunarVagabond/avalon-protocol/issues/596)
  (implemented) adds a push-registered fast path on top of #299's polling,
  reusing #583's interest-registration design rather than a new
  addressing mechanism — see the "Today in the repo" section above.
- #180 (decided) / [#208](https://github.com/LunarVagabond/avalon-protocol/issues/208)
  (implemented) — node-tiered durable history retention: retention-tier
  config, payload pruning, the settlement-state checkpoint. The
  indexer-projection-snapshot half of #180's ask stays open, tracked under
  #43.
- [#296](https://github.com/LunarVagabond/avalon-protocol/issues/296)
  (decided) the 8-node POC topology: Settlement nodes genuinely mirror each
  other (#40/#299); Indexer/Realtime/Gateway nodes reach a Settlement node
  over its API rather than sharing its database.
  [#313](https://github.com/LunarVagabond/avalon-protocol/issues/313)
  (implemented) is the scoped-down slice of #291 that makes that real —
  `AVALON_SETTLEMENT_REMOTE_URL` plus `POST /ledger/submit`, per the "Today
  in the repo" section above.
- [#287](https://github.com/LunarVagabond/avalon-protocol/issues/287)
  decided (hoster-configurable resource limits), implemented by
  [#363](https://github.com/LunarVagabond/avalon-protocol/issues/363) — see
  the "Today in the repo" section above.
- [#292](https://github.com/LunarVagabond/avalon-protocol/issues/292)
  decided (node-to-node announce/bootstrap discovery design), implemented
  by [#362](https://github.com/LunarVagabond/avalon-protocol/issues/362) —
  see the "Today in the repo" section above. [#308](https://github.com/LunarVagabond/avalon-protocol/issues/308)
  decided (version rollout design), implemented by
  [#368](https://github.com/LunarVagabond/avalon-protocol/issues/368) —
  protocol version awareness gating this peer table, see the "Version
  rollout"/"Today in the repo" sections above.
- [#91](https://github.com/LunarVagabond/avalon-protocol/issues/91) SDK node
  discovery and capability negotiation — could consume #362's peer table
  once it exists
- [#72](https://github.com/LunarVagabond/avalon-protocol/issues/72) TLS before
  any non-local deployment
- [#78](https://github.com/LunarVagabond/avalon-protocol/issues/78) realtime is
  its own vertical and can become its own node
