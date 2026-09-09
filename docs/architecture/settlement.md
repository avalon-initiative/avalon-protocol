# Settlement

Settlement is the durable-history vertical: the place protocol facts are
committed so that anyone can verify them later. **Settlement is not the general
purpose query database.** **One protocol event is never one settlement
transaction.** **Milestone 1 is a hash-chained, append-only ledger; the
long-term backend is Avalon's own chain, with no native currency at launch —
the consensus mechanism itself is still an open engineering question.**

## Durable history, not a database

Settlement is responsible for durable commitments, provenance, canonical protocol
history, verifiable attestations, ownership history, durable identity facts,
guild history, game/issuer registration, and key lifecycle. It is not
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
root itself is a placeholder deterministic value (the batch's chain tip) —
whether it becomes a real Merkle root is
[#40](https://github.com/LunarVagabond/avalon-protocol/issues/40)'s call.

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
`sdk`, and every game integration are indifferent to how a commitment is
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
  server a game trusts. **Avalon does not run mining, or consensus staked on a
  currency,** to referee write ordering; Avalon's writes are already
  unambiguous because each actor signs its own. (This does not rule out
  currency-free validator consensus — see the backend decision below.)
- Milestone 1 stays Postgres-backed, with the constraint that the schema and
  signing scheme must make entries independently verifiable and the log
  exportable from day one rather than retrofitted.
- **The long-term backend: Avalon operates its own chain**
  ([#79](https://github.com/LunarVagabond/avalon-protocol/issues/79), closed;
  [ADR #93](https://github.com/LunarVagabond/avalon-protocol/issues/93)). Not
  anchored to and not built on top of an existing chain's checkpoints or
  token. No native currency or token at launch — the chain settles protocol
  facts, not value; a cross-game currency layer is an explicitly later,
  optional phase ([`../stakeholders/Proposal.md` §15](../stakeholders/Proposal.md#15-economy-and-currency))
  evaluated on its own, not a prerequisite for the chain existing. Consensus
  among Avalon's own validators does not require proof-of-work or a
  stake-weighted token — a permissioned set of registered, known node
  operators reaching Byzantine-fault-tolerant agreement is sufficient, and
  does not reopen #70's rejection of mining/consensus (there is still no
  scarce resource to referee) or its rejection of federation (every validator
  proposes into, and every mirror reads from, the same canonical chain).

## What is decided (continued): block storage is not Postgres

Real chains don't use a shared relational database for the blocks/entries
themselves — confirmed against how the two closest reference systems
actually work: Bitcoin Core stores raw blocks in flat files (`blk*.dat`) with
LevelDB only for rebuildable indexes (block metadata, the UTXO set); Kaspa's
Rust node (`rusty-kaspa`) uses RocksDB for blocks, DAG structure, and UTXO
state. Both are embedded, per-node key-value stores or flat files — each
full node holds its own complete local copy, synced peer-to-peer, never a
shared central SQL server every participant reaches into. **Avalon's ledger
data follows the same shape**: each validator/mirror holds its own local
copy in an embedded store, not a connection string into one shared Postgres.
Kaspa is the specific reference given its block-rate requirements are the
closer analog to a high-throughput commitment log's needs.

This does not change Postgres's role anywhere else — the indexer's
projections and ephemeral state (sessions, presence, ceremony state) stay
exactly where they are. This decision is scoped to settlement data
specifically, sharpening the boundary this document already draws
("settlement is not the general-purpose query database").

## What is open

**The consensus and validator design, and the storage engine choice** — both
scoped inside [#40](https://github.com/LunarVagabond/avalon-protocol/issues/40)
alongside the log's own hash/Merkle/signed-tree-head design: which BFT
algorithm, validator admission and rotation, block/round cadence and
finality; and which embedded engine (RocksDB, `sled`, `redb`, or a
comparable alternative) actually backs the per-node storage above — the
*shape* (embedded, per-node, not Postgres) is decided, the specific engine
is not. Needs its own research spike and written comparison of real
candidates for both before it closes.

## Today in the repo

`crates/chain/src/postgres.rs` — `PostgresSettlementProvider`, real and in use:

- One row per event in `ledger_entries`
  (`crates/server/db/migrations/0002_ledger/up.sql`, plus `batch_id` from
  `0013_ledger_batches/up.sql`): `seq`, `event_id`, `kind`, `issuer`,
  `subject`, `payload`, `event_timestamp`, `version`, `prev_hash`,
  `entry_hash`, `batch_id`, `committed_at`. One row per batch in
  `ledger_batches`: `batch_id`, `first_seq`, `last_seq`, `batch_root`,
  `committed_at`.
- `entry_hash = SHA-256(prev_hash ‖ event content)`; the first entry's
  `prev_hash` is the all-zero `GENESIS_HASH`, and entries stay hash-chained
  across batch boundaries — `commit` never resets the chain per batch.
  `commit` inserts every entry in `batch.events` plus the batch's own
  `ledger_batches` row in one transaction (the row-to-batch foreign key is
  deferred to transaction end, since the batch row is inserted after its
  entries) and returns the batch's root (its last entry's hash — a
  placeholder deterministic root, not yet a Merkle root) as the
  `Commitment.proof`.
- `get_commitment` reads `ledger_batches` by `batch_id`. `verify` recomputes
  a batch's root by replaying the hash chain across its own entries' stored
  content (not by trusting the stored `entry_hash`/`batch_root` values), so
  tampering with any entry in the batch is caught, not just a missing row.
  `list_entries` still independently rehashes every row and checks each
  link across the whole ledger, which is what `avalon inspect-ledger`
  (`crates/cli/src/main.rs`, `make inspect-ledger`) prints, now with batch
  boundary headers and each batch's root — `avalon inspect-ledger-full` /
  `make inspect-ledger-full` is the same view plus each entry's actual JSON
  payload.
- The settlement worker (`crates/server/src/outbox.rs`, #71) drains pending
  `protocol_outbox` rows into one `EventBatch` per drain tick and commits it
  with a single `SettlementProvider::commit` call — a batch closes when the
  worker runs, not on a size threshold or timer, and a single-event batch is
  legal. No handler calls `commit` directly.
- Not yet: signatures (#39), Merkle roots or signed tree heads (#40), an
  export/mirror format.
- The ledger shares `avalon-server`'s `PgPool` and migrations; milestone 1 has
  one database. Split when `chain` gets its own deployment, not before.
- `list_entries_for_issuer_prefix` — a narrower, unverified issuer-filtered
  read (no hash/link recomputation, unlike `list_entries`), behind
  `GET /me/history` (issue #121, `crates/server/src/handlers.rs::my_history`).
  **Design decision**: reads the ledger directly rather than through the
  indexer, since `crates/indexer` is still scaffolding (no real projection
  store exists yet) — a ledger read is the only real read path today. This
  stays a narrow, server-constructed, caller-scoped read (the issuer prefix
  is always built from the authenticated identity id, never accepted as a
  request parameter) rather than a general query surface, so it doesn't
  cross this document's "settlement is not the general-purpose query
  database" boundary. Revisit once the indexer (#42/#43) is real — see
  [query-and-indexing.md](./query-and-indexing.md).

## Decisions and tickets

- Epic [#36](https://github.com/LunarVagabond/avalon-protocol/issues/36)
  Settlement Ledger
- #68, #70, #79, #93 decided; #40, #39 open engineering decisions
- #38 batching, #71 atomicity,
  [#37](https://github.com/LunarVagabond/avalon-protocol/issues/37) (closed)
  the current provider
- #75 durable history is canonical; #82 event catalogue
