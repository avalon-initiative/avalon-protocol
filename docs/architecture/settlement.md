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
(#365 — a write-time abuse-floor quota) and making a subject's own history
cheap to sync selectively without replaying the whole ledger (#364). Both
are real, organic-use costs this rule doesn't touch. A structural bound on
long-run per-subject growth for legitimate, ongoing use (log
compaction/checkpointing) was considered and deliberately deferred — see
#366 — real design work, not needed at this project's current scale.

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
  (per-issuer-subject write quota) implement the near-term pieces; #366
  (log compaction/checkpointing for long-run organic growth) is
  deliberately deferred, still open
