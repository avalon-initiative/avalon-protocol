# Settlement

Settlement is the durable-history vertical: the place protocol facts are
committed so that anyone can verify them later. **Settlement is not the general
purpose query database.** **One protocol event is never one settlement
transaction.** **Settlement is a transparency log, not a blockchain — a
hash-chained, append-only, publicly verifiable store with no validator or
consensus layer, because nothing written to it is ever contested (ADR #186).**

## Durable history, not a database

Settlement is responsible for durable commitments, provenance, canonical protocol
history, verifiable attestations, ownership history, durable identity facts,
guild history, integrator/issuer registration, and key lifecycle. It is not
responsible for fast reads — that is [`./query-and-indexing.md`](./query-and-indexing.md).
A mutable Postgres row is never the ultimate authority for something Avalon
promises to preserve forever ([#75](https://github.com/LunarVagabond/avalon-protocol/issues/75)).

## Batching

```text
Event 1
Event 2
...
Event N
    ↓
Batch                    (EventBatch)
    ↓
Merkle tree / commitment (Commitment)
    ↓
Settlement               (SettlementProvider::commit)
```

Achievement → transaction, per event, does not scale and is never the design.
Events accumulate in a buffer (the outbox, #71), are batched, a commitment is
computed over the batch, and the commitment is what gets settled. This is real
today, not aspirational: `ledger_entries.batch_id` groups every entry under
the `EventBatch` it was committed with, `ledger_batches` is the one row per
batch that `get_commitment` looks up, and `verify` recomputes a batch's root
from its entries rather than trusting a single stored value. How large a
batch is and how often commitments are produced stay tuning questions —
today a batch closes whenever the settlement worker's drain tick runs
(`crates/server/src/outbox.rs`), not on a size threshold or timer, and a
single-event batch is legal — see [`./scalability.md`](./scalability.md) and
[#38](https://github.com/LunarVagabond/avalon-protocol/issues/38). The batch
root is a real RFC 6962 Merkle Tree Hash over the whole ledger as of that
batch ([#210](https://github.com/LunarVagabond/avalon-protocol/issues/210),
implementing [#40](https://github.com/LunarVagabond/avalon-protocol/issues/40)'s
decision) — see "What is decided (continued)" and "Today in the repo"
below for the full design and what's actually built.

## The boundary

`crates/chain/src/lib.rs`:

```rust
#[async_trait]
pub trait SettlementProvider: Send + Sync {
    async fn commit(&self, batch: &EventBatch) -> Result<Commitment, SettlementError>;
    async fn verify(&self, commitment: &Commitment) -> Result<bool, SettlementError>;
    async fn get_commitment(&self, batch_id: Uuid) -> Result<Commitment, SettlementError>;
}
```

This trait is the only thing outside `chain` may depend on. `protocol`, `server`,
`sdk`, and every integrator integration are indifferent to how a commitment is
produced. Consensus, block production, and P2P networking do not belong on this
trait. Interfaces beyond it are introduced only when an actual implementation
needs them.

## What is decided

- **Attestations before blockchain**
  ([#68](https://github.com/LunarVagabond/avalon-protocol/issues/68)). Durable
  facts need to *behave like* a verifiable ledger — tamper-evident,
  independently verifiable — which a signed, append-only store provides without
  a chain.
- **The shape is a public transparency log**
  ([#70](https://github.com/LunarVagabond/avalon-protocol/issues/70)). Every
  durable fact is a signed, hash-chained, append-only entry; anyone can verify
  an entry and its position without permission; anyone can mirror the log and
  serve reads. **Federation is rejected** — visibility must not depend on which
  server an integrator trusts. **Avalon does not run mining, or consensus staked on a
  currency,** to referee write ordering; Avalon's writes are already
  unambiguous because each actor signs its own — which is also why no
  validator consensus is needed either, currency-free or not (ADR #186).
- Postgres is the settlement backend, permanently, not a milestone-1
  stand-in for something more "real"
  ([ADR #186](https://github.com/LunarVagabond/avalon-protocol/issues/186)).
  Real production transparency logs (Certificate Transparency, Sigsum,
  Google's Trillian) run on ordinary SQL backends, because verification
  happens by fetching Merkle proofs over the network, not by every party
  independently holding a full replicated copy the way a blockchain full
  node does. The schema and signing scheme still need to make entries
  independently verifiable and the log exportable, but that's a property of
  the data model, not a reason to leave Postgres.
- **No blockchain, no validator/BFT consensus**
  ([ADR #186](https://github.com/LunarVagabond/avalon-protocol/issues/186),
  superseding part of [ADR #93](https://github.com/LunarVagabond/avalon-protocol/issues/93)).
  Consensus of any kind exists to adjudicate contention over a single,
  shared, mutable, scarce resource — the double-spend problem. Nothing
  settlement records is contested: `identity.created`, `achievement.issued`,
  `friend.requested`, `guild.created`, `game.registered` are each a fact
  asserted by exactly one authoritative signer about something only that
  signer has authority over. No native currency or token at launch, and no
  validator set to run one — a cross-integrator currency layer remains an
  explicitly later, optional, *separately decided* phase
  ([`../stakeholders/Proposal.md` §15](../stakeholders/Proposal.md#15-economy-and-currency)):
  only if that's ever actually proposed does consensus become a real
  question again, scoped to that feature specifically. #79/#93's rejection
  of anchoring to or building on an existing chain's token stands regardless
  — there was never a chain to anchor to in the first place under this
  decision.

## What is decided (continued): transparency log structure

[#40](https://github.com/LunarVagabond/avalon-protocol/issues/40) and
[#39](https://github.com/LunarVagabond/avalon-protocol/issues/39) (both
closed) settled the log's actual hash/Merkle structure and signing scheme,
following the RFC 6962 (Certificate Transparency) design directly rather
than inventing one — the same precedent ADR #186 already cited for keeping
Postgres as the backend:

- **Two structures, not one, doing different jobs.** The existing
  sequential hash chain (`entry_hash = SHA-256(prev_hash ‖ content)`,
  #37/#173) is untouched — it stays the cheap, O(1)-per-link tamper-evidence
  mechanism that `avalon inspect-ledger` already walks end-to-end. Layered
  on top: a single append-only **Merkle tree over the whole ledger's
  `entry_hash` values, ordered by `seq`**, computed with RFC 6962's exact
  tree-hashing algorithm (domain-separated leaf/interior hashing, the
  standard largest-power-of-two-less-than-n split) rather than a bespoke
  scheme — this is what makes succinct inclusion/consistency proofs
  possible, which a plain hash chain alone cannot give a mirror without
  transferring every entry.
- **`ledger_batches.batch_root` becomes that real answer** to the question
  #38 explicitly left open ("whether the root is a simple chain-tip or a
  Merkle root over the batch is #40's call"): `batch_root` is now the
  tree's Merkle Tree Hash (MTH) over every entry committed so far,
  replacing the placeholder chain-tip value — not a per-batch sub-tree, the
  whole ledger's tree as of that batch. `tree_size` (the STH's field,
  stored separately from `ledger_batches`) is always the real leaf
  *count*, never `last_seq` — `seq` is `GENERATED ALWAYS AS IDENTITY`, and
  Postgres identity/sequence advancement isn't transactional, so a batch
  commit that fails partway through and rolls back permanently burns
  whatever `seq` values it had already allocated. Conflating `last_seq`
  with leaf count once a gap like that exists would silently mislabel
  every `tree_size` from that point on; `commit` derives `tree_size` from
  the actual number of rows fetched, not from `last_seq`. Cadence needs no
  new decision: it's already whatever #38/#71's settlement worker does (a
  batch, and now also an STH, closes whenever the outbox drain tick runs).
- **Signed Tree Heads, not per-entry signatures.** #39 resolves to
  STH-only signing, matching real transparency-log precedent — Certificate
  Transparency logs never sign individual certificates, only the tree head;
  an inclusion proof anchored to one valid signed STH already lets anyone
  verify a specific entry belongs to the operator-endorsed tree, with no
  need for the operator to separately sign every entry. One `SignedTreeHead
  { tree_size, root_hash, network_id, timestamp, signing_key_id, signature
  }` is produced per batch commit, in the same transaction, Ed25519,
  covering exactly this settlement-operator key domain (distinct from
  issuer keys, #80, and identity keys, #73 — three separate lifecycles, per
  #39's own original scoping).
- **Mirror sync stays minimal, no witness quorum required yet.** With one
  settlement operator today, the simplest viable protocol suffices: expose
  the latest STH, a historical STH by `tree_size`, RFC 6962 consistency
  proofs (tree at size A is a strict append-only extension of tree at size
  B), and inclusion proofs (entry at `seq` is in the tree at `tree_size`).
  Any mirror that stores every STH it has independently observed can be
  compared against any other mirror's history for the same `tree_size` —
  a mismatch is cryptographic proof of operator equivocation, publishable
  as misbehavior evidence, without a formal witness-cosigning protocol.
  Witness cosigning (Sigsum-style — a small independent witness set must
  countersign an STH before it's trusted) stays the natural strengthening
  *if* Avalon ever runs more than one independent settlement operator —
  not needed, and not built, while there's only one.
- **Storage stays exactly what ADR #186 already decided**: Merkle proofs
  computed from `ledger_entries.entry_hash`, ordered by `seq` — no embedded
  engine, no per-node full copy in Postgres. #349 added an in-memory
  incremental tree (see "Today in the repo") so that computation is O(log n)
  instead of from-scratch every call; it doesn't change this decision's wire
  format.

Implementation tracked as
[#210](https://github.com/LunarVagabond/avalon-protocol/issues/210)
(real Merkle root + signed tree heads, replacing #38's placeholder) and
[#211](https://github.com/LunarVagabond/avalon-protocol/issues/211)
(mirror-facing proof/sync endpoints), both under epic #36 — both now
implemented, see "Today in the repo" below.

## Cross-shard commitment (#527 decided, #529 design)

[#527](https://github.com/LunarVagabond/avalon-protocol/issues/527)
(decided, closed) sharded settlement authority per-integrator instead of one
operator, with an explicit invariant: the cross-shard root must be
independently computable by any node from public inputs — no designated
"gluer"/aggregator node, because aggregation itself would just become a new
single point of failure. This section is #529's design for making that
concrete, revisiting #40/#210/#211's closed single-log Merkle/STH design
above for a world with more than one log.

**Each shard keeps exactly what a single-operator ledger has today.** A
shard is a `PostgresSettlementProvider`-style log with its own hash chain,
Merkle tree, and STH, signed by that shard's own settlement key —
unchanged from everything in "What is decided (continued)" above. Sharding
adds one more layer on top; it does not change how any individual shard
works internally, and existing single-shard inclusion/consistency proofs
keep working unchanged from a client's perspective (#529's own invariant).

**Shard identity and registration.** Each shard has a stable `shard_id`
(the sharded integrator's own slug/id, already unique under #527's
per-integrator model — no new uniqueness mechanism needed). A shard
announces itself to the network with a durable, signed `shard.registered`
event (`kind`, `shard_id`, `shard_id`'s settlement public key, first-known
STH), gossiped the same way node announcements already propagate via
#362's peer table — not a new transport. This is what lets a newly-joined
shard get included without a coordinated "everyone add this shard" event:
the shard simply announces itself, and any node that has received the
announcement now knows to include it.

**Canonical ordering: sort by `shard_id`, byte-wise.** The cross-shard
tree's leaves are `(shard_id, shard_sth)` pairs, sorted by `shard_id` as a
plain byte-wise (UTF-8) ascending sort — no registration-order, join-time,
or other stateful ordering. This is what makes the aggregation
deterministic: two nodes with the same *set* of currently-known shard STHs
always produce the same sorted leaf order, and therefore the same tree,
regardless of the order they happened to learn about each shard in.

**Aggregation recipe.** For each currently-known shard, compute a leaf hash
`SHA-256(shard_id ‖ sth.tree_size ‖ sth.root_hash ‖ sth.signing_key_id ‖
sth.signature)` — the STH's own signature is included in the leaf so the
cross-shard root also commits to *which* STH (not just which shard) was
aggregated, making a shard's downgrade to a stale/rolled-back STH
detectable the same way any Merkle inclusion mismatch is. Leaves are
sorted by `shard_id` (see above) and combined with the exact same RFC 6962
tree-hashing algorithm the single-shard Merkle tree already uses — no new
hashing scheme, just a second tree of the same shape, one level up. The
result is a `CrossShardRoot { root_hash, shard_count, computed_at }` that
any node can recompute byte-for-byte from public gossip alone. This is
deliberately *not* itself STH-signed by any one party — signing it would
reintroduce exactly the designated-aggregator chokepoint #527 exists to
remove. A node that wants to publish "the network's current global state
as I see it" publishes the `CrossShardRoot` plus the full list of
`(shard_id, sth)` pairs it used, so anyone can independently verify by
recomputing.

**Detecting a missing or stale shard, instead of silently disagreeing.** A
node tracks two sets: shards it has ever seen a `shard.registered` event
for ("known shards"), and shards it currently holds an STH for ("STH'd
shards"). A `CrossShardRoot` computed while `STH'd shards` is a strict
subset of `known shards` is marked `partial: true` and lists exactly which
`shard_id`s are missing, rather than silently producing a root over
whatever subset happens to be on hand — the same "detectable, not silent"
standard #543's per-shard trust anchors already applies to a shard id
alone proving nothing. A node with a `partial` root should not be treated
as authoritative for the shards it's missing; it can still serve full
proofs for shards it does have complete STHs for.

**Verifying "is shard X part of mainnet's current global state."** Given a
`CrossShardRoot`, the full list of `(shard_id, sth)` pairs it was computed
from, and a standard Merkle inclusion proof for shard X's leaf within that
list — any node (or mirror, or client) can verify shard X's current STH is
part of that specific cross-shard root using the same inclusion-proof
verification code the single-shard tree already has, applied one level up.
No new proof type; the cross-shard tree is verified exactly like the
per-shard tree is.

**What this does not change.** No consensus, no ordering across shards —
this is purely an aggregation/commitment structure over independently
authoritative logs, exactly as #527 decided. A network with exactly one
shard degenerates to a `CrossShardRoot` over a single leaf, which is
trivially equal to that shard's own STH in everything but shape — the
single-operator case from earlier in this document is the one-shard
special case of this design, not a separate code path.

Implementation (building this into `chain`/`server`, plus the live tests
#529 calls for — two independent nodes given the same gossiped shard STHs
producing byte-identical cross-shard roots, and a node with a missing
shard correctly marking its root `partial`) is separate follow-up work
under epic #528, not part of this design pass.

### Write routing to the correct shard (#532)

Sharding is per-integrator by candidate (#527's own wording) — but not
every durable event has an integrator to shard by. A `ProtocolEvent`'s
`issuer` is a [`GlobalId`](../../crates/protocol/src/ids.rs)
(`<namespace>:<owner>:<kind>:<key>`); its `namespace` is exactly the
signal write routing needs:

- **`namespace` is `game`/`app`/`service`** (an integrator acting as
  itself — achievement issuance, attestation revocation, integrator-owned
  connections/schema events) — routes to that integrator's own shard,
  `shard_id = "{namespace}:{owner}"`. This is the case #527's motivating
  examples (`achievement.issued`) are actually about.
- **`namespace` is `identity`** (identity/social-graph/guild events —
  `identity.created`, `friend.*`, `guild.*`, and everything else with no
  single owning integrator) — routes to a single reserved **core shard**
  (`shard_id = "core"`), not split per-integrator. This is a deliberate,
  narrower scope than "every event is sharded": #527 never claimed
  identity/social/guild history has a natural per-integrator partition
  (it doesn't — a friendship or guild isn't owned by any one integrator),
  so those stay on one shared log, same as today. The core shard is not
  exempt from #527's motivation in principle — it can itself become a
  managed/shard-operator-run shard like any other (#531) — it just isn't
  *split further* by this design.
- `outbox::drain_once` groups pending rows by this derived `shard_id`
  before building a batch (today it builds exactly one batch per tick;
  this changes it to one batch per shard per tick), and looks up that
  shard's own commit target: local `chain.commit` if this node holds that
  shard's own signing key, otherwise the shard's own configured remote
  authority (`AVALON_SETTLEMENT_REMOTE_URLS`, extending #313's existing
  single-`AVALON_SETTLEMENT_REMOTE_URL` config to a `shard_id=url` map —
  the existing singular env var keeps working unchanged as the implicit
  `core=<url>` entry, so milestone-1's single-shard topology needs no
  config change).

This keeps #532's invariant intact: exactly one legitimate write authority
per shard, never contested — routing picks *which* shard's authority to
use, it never introduces a second writer for the same shard. A milestone-1
deployment with no `AVALON_SETTLEMENT_REMOTE_URLS` configured has exactly
one shard (`core`) and behaves exactly as today.

**Implemented (#532).** `outbox::shard_id_for_event` derives the shard
exactly as designed above; `RemoteSubmitConfig::from_env` parses
`AVALON_SETTLEMENT_REMOTE_URLS` into a `shard_id -> url` map, merging in
`AVALON_SETTLEMENT_REMOTE_URL` as the implicit `core` entry; `drain_locked`
groups a tick's pending rows by shard and commits one independent
`EventBatch` per shard (never mixing two shards into one batch). Live-
verified: a single drain tick spanning both a `core`-shard event and a
`game:...`-shard event produced two distinct `ledger_entries.batch_id`
values (`crates/server/src/outbox.rs`'s
`a_tick_spanning_two_shards_commits_two_separate_batches`), and the full
existing live suite (real achievement issuance, Integrator Space instance
publication, settlement proof endpoints — all real `game:...`-namespace
writes) re-run unchanged against the new routing with no regression.

## Bounding ledger growth

Standing rule, decided in #306: **high-frequency ephemeral data never
touches the settlement ledger, full stop.** Chat (`guild_messages.rs`) and
DM conversation content (`conversations.rs`) already follow this — neither
calls `outbox::enqueue`, each enforced by a dedicated grep-based test
(`crates/server/tests/guild_messages_no_ledger.rs`,
`crates/server/tests/conversations_no_ledger.rs`) rather than left as an
unenforced convention. Any new feature considering a write to the durable
ledger should default to this same question first: is this paced by real,
human-scale actions (identity, friendship, guild membership, achievements —
what the ledger already carries), or is it machine-speed/high-cardinality
data that belongs in the indexer's rebuildable projections instead? When
it's the latter, add the same grep-test enforcement other ledger-adjacent
modules already use rather than trusting review alone to catch it later.

This bounds *new* unbounded growth from careless design choices. It does
not bound the two things #306 identified as genuinely open regardless of
this rule: how many events one issuer can write about one subject
(#365, implemented) and making a subject's own history cheap to sync
selectively without replaying the whole ledger (#364, implemented). Both
are real, organic-use costs this rule doesn't touch. A structural bound on
long-run per-subject growth for legitimate, ongoing use (log
compaction/checkpointing) was considered and deliberately deferred — see
#366 — real design work, not needed at this project's current scale.

**#365** — a volume-only abuse floor on `(issuer, subject)` attestation
writes, enforced in `crates/server/src/achievements.rs`'s
`issue_attestation`/`bulk_issue_attestation` before either does any
signature verification: a rolling window (`AVALON_ACHIEVEMENT_WRITE_QUOTA`,
`AVALON_ACHIEVEMENT_WRITE_QUOTA_WINDOW_HOURS`, generous defaults) counts
recent writes for that exact `(issuer, subject)` pair and rejects
(`429 ATTESTATION_WRITE_QUOTA_EXCEEDED`) a write, or a whole bulk call,
that would cross it — never a partial application, and it never inspects
*what* is being attested to, only volume. Scoped to
`achievement_attestations` writes specifically, matching the "high-frequency
ephemeral data never touches the ledger at all" rule above — chat/presence
never reach this quota because they never reach the ledger.

**#364** — `GET /ledger/entries?subject=` pre-filters the existing bulk
entries endpoint to one subject's own entries, same pagination/ordering
semantics as the unfiltered form, backed by a `(subject, seq)` composite
index (migration 0061). `subject` matches the *exact* compound value
`ledger_entries.subject` stores — e.g. `identity:<uuid>:self:achievement_issued`
— not just the bare owner id, since every event-producing module in this
codebase bakes its own verb into `subject` (`issuer_ref`/`identity_ref`/
`guild_ref` helpers). A caller filters on the exact string it already
knows it wrote; there's no owner-level "everything about this identity
regardless of verb" query yet. A subject with no entries returns an empty
list, not an error. Filtering never changes an entry's hash-chain position
— inclusion proofs for a filtered row still verify against the same
global tree.

## Today in the repo

`crates/chain/src/postgres.rs`'s `PostgresSettlementProvider` is real and in
use. Full build-by-build detail — exact tables, migrations, algorithms — lives
in its own file: [`./settlement-implementation-notes.md`](./settlement-implementation-notes.md).
Everything below is real and implemented unless noted otherwise.

- **Ledger storage**: one row per event in `ledger_entries`, one row per batch
  in `ledger_batches`, hash-chained via `entry_hash = SHA-256(prev_hash ‖
  event content)` across batch boundaries.
- **Merkle root + Signed Tree Heads** (#210) — RFC 6962 Merkle Tree Hash over
  the whole ledger, one Ed25519-signed `SignedTreeHead` per batch.
- **Incremental Merkle tree** (#349) — an in-memory O(log n) compact-range
  structure replaces from-scratch O(n) recomputation on every commit/proof;
  every root/proof is verified byte-identical to the from-scratch version.
  Rebuilt from Postgres on restart, not itself persisted.
- **Verification**: `verify` independently replays both the sequential hash
  chain and the Merkle root, and both must pass.
- **Mirror-facing proof/sync endpoints** (#211) and a **mirror-watcher**
  (#299) — a mirror can independently verify authenticity rather than
  trusting whichever node answered its request.
- **`POST /ledger/submit`** (#313) — the node-to-node write endpoint.
- **Genesis and network identity** (#173) — a singleton `chain_genesis` table.
- **Node-tiered durable history retention** (#208) and a **settlement-state
  checkpoint** (#180/#208) for fast restart.

See [`./settlement-implementation-notes.md`](./settlement-implementation-notes.md)
for exact types, migrations, and algorithms behind every item above.

## Decisions and tickets

- Epic [#36](https://github.com/LunarVagabond/avalon-protocol/issues/36)
  Settlement Ledger
- #68, #70, #79, #186 decided (#93 partially superseded by #186); #40, #39
  decided (Merkle/STH structure, STH-only signing) — #210 (real Merkle root
  + Signed Tree Heads), #211 (mirror-facing proof/sync endpoints), and #299
  (mirror-watcher: STH observation, equivocation detection, entry
  backfill) all implemented
- #38 batching, #71 atomicity, #173 genesis/network identity,
  [#37](https://github.com/LunarVagabond/avalon-protocol/issues/37) (closed)
  the current provider
- #75 durable history is canonical; #82 event catalogue
- #178/#179 (RocksDB-backed provider) reverted (#185), won't-fix per #186
- #80, #84 — issuer key lifecycle, a separate key domain from the
  settlement operator key #39 decided
- #180 decided (node-tiered durable history retention: pruning, snapshots,
  archive-node role) — implemented by
  [#208](https://github.com/LunarVagabond/avalon-protocol/issues/208)
- [#232](https://github.com/LunarVagabond/avalon-protocol/issues/232) —
  publish and pin trusted network identities (the settlement operator's key
  pinned to `network_id`, enforced client-side) — see
  [`network-trust-anchors.md`](./network-trust-anchors.md)
- [#349](https://github.com/LunarVagabond/avalon-protocol/issues/349) —
  incremental Merkle tree, O(log n) commit and proof-serving — implemented
- [#306](https://github.com/LunarVagabond/avalon-protocol/issues/306)
  decided (bounding ledger growth — see the section above): the
  ledger-vs-indexer split is now a standing rule (#306's "option D"), not
  just precedent; #364 (subject-scoped selective sync) and #365
  (per-issuer-subject write quota) implement the near-term pieces —
  both now built (see the section above); #366 (log
  compaction/checkpointing for long-run organic growth) is
  deliberately deferred, still open
