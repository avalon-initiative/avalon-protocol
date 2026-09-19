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
Unset defaults to `combined`, today's only actually-implemented mode
(specialized single-role deployments are designed for, per the doc
comment above, but not yet exercised operationally). This value is purely
advisory/self-reported (peer-table bookkeeping and #539's realtime relay
routing) — it doesn't gate which endpoints a node actually serves.

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
Two things this second node/mechanism does *not* by itself solve, and
shouldn't be read as solving: **write availability** during a primary
outage (a mirror-only node never becomes a new writer/authority — that's a
promotion/failover story neither of these attempts) and the harder
multi-writer/consensus question #40 still owns. What it does solve: a
genuine second, independently-verifiable copy of Settlement history no
longer depends on one physical database being up (reads against
`avalon-peer` succeed today even with the primary down, served from its
own independently-verified mirrored data), and a hot node's pruning
decision is no longer a documented risk an operator has to manage by
hand — it's an enforced check against real, confirmed coverage.

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
(`AVALON_DHT_ENABLED`, off by default — see `crate::dht`'s module doc for
the full rationale) holds a libp2p `PeerId`, a *fourth* independent key
domain alongside player keys (#73), issuer keys (#80/#84), and the
settlement log operator's key (#39) — none of those fit a peer-transport
identity, so this is genuinely new rather than reused. Bootstrap reuses
this section's own peer table rather than inventing a second discovery
mechanism: `PeerInfo`/`POST /nodes/announce` now also carry a peer's
`libp2p_peer_id` and dialable multiaddrs (`#[serde(default)]`, so an older
peer's announce without these fields still deserializes fine), and
`crate::dht`'s worker watches that same table for identities it hasn't
dialed into the DHT yet.

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
  to change behavior.

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
  of epic #580)**: `crate::dht`, gated behind `AVALON_DHT_ENABLED`
  (off by default — every deployment's behavior is unchanged until an
  operator opts in). `PeerInfo` now also carries `libp2p_peer_id`/
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
  **Known limitation, not solved here:** the DHT keyspace has no
  `network_id` segregation the way the HTTP peer table does — see
  `crate::realtime_relay`'s own module doc comment.
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
    (write availability during an outage remains open).
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

## Decisions and tickets

- #70 mirrors of a public log, not federation
- #79 long-term settlement backend; #186 decided no blockchain/validator
  consensus (transparency log on Postgres instead, superseding part of #93);
  #40 log design and mirror sync (validator/consensus design dropped) —
  [#299](https://github.com/LunarVagabond/avalon-protocol/issues/299)
  (implemented) is the actual mirror-watcher built against that design
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
