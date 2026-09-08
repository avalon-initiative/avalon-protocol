# Settlement

Settlement is the durable-history vertical: the place protocol facts are
committed so that anyone can verify them later. **Settlement is not the general
purpose query database.** **One protocol event is never one settlement
transaction.** **Milestone 1 is a hash-chained, append-only ledger; the
long-term shape is a public transparency log, and what that log anchors to or
becomes is still an open decision.**

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
Events accumulate in a buffer, are batched, a commitment is computed over the
batch, and the commitment is what gets settled. How large a batch is and how
often commitments are produced are tuning questions for
[`./scalability.md`](./scalability.md) and
[#38](https://github.com/LunarVagabond/avalon-protocol/issues/38).

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
  server a game trusts. **Avalon does not run mining or validator consensus** to
  referee write ordering; Avalon's writes are already unambiguous because each
  actor signs its own.
- Milestone 1 stays Postgres-backed, with the constraint that the schema and
  signing scheme must make entries independently verifiable and the log
  exportable from day one rather than retrofitted.

## What is open

**Log design** —
[#40](https://github.com/LunarVagabond/avalon-protocol/issues/40): the
hash-chaining / Merkle structure, signed-tree-head cadence, how mirrors sync and
detect tampering, and what the milestone-1 schema needs to support this.
Everything decided there is needed under every outcome of the backend decision.

**Ledger signing key** —
[#39](https://github.com/LunarVagabond/avalon-protocol/issues/39): how the log
operator signs entries and tree heads. A separate key with a separate lifecycle
from issuer keys ([`./games-and-issuers.md`](./games-and-issuers.md)) and player
keys ([`./identity.md`](./identity.md)).

**Long-term backend** —
[#79](https://github.com/LunarVagabond/avalon-protocol/issues/79). An
operator-run log is verifiable and mirrorable, but the operator still controls
what gets appended. Three shapes could deliver operator-independent permanence,
and none is chosen:

1. **Anchored transparency log** — the log stays canonical; its signed tree
   heads / Merkle roots are periodically published to an existing public chain.
   Only checkpoints touch the chain.
2. **Partnership with a specific chain** — the same batching model, committed to
   one chain's ecosystem (a high-throughput L1, a BlockDAG such as Kaspa, an L2
   or rollup, or a data-availability layer), possibly with deeper integration.
3. **Custom Avalon chain** — Avalon's own block/DAG production, node software,
   and consensus. Full control, full cost; would supersede #70's consensus
   rejection (the federation rejection stands regardless).

Candidates are evaluated against: checkpoint throughput and latency (not
per-event volume), finality and how it composes with the log's own
verifiability, cost per commitment over 5/10/20 years, decentralization actually
delivered, storage and verification cost for a mirror or light client, batched
commitment and historical-verification support, node/operator requirements and
operational complexity, developer experience, ecosystem maturity and long-term
viability, and what happens to Avalon if the chain stalls, forks, or dies.
Kaspa/BlockDAG is a research reference for concurrent block production under
high commitment volume. It is not an assumption.

Invariants that hold whichever way #79 goes: batching, gameplay never touching
settlement, `SettlementProvider` as the sole boundary, and a chain-agnostic
`protocol` crate.

## Today in the repo

`crates/chain/src/postgres.rs` — `PostgresSettlementProvider`, real and in use:

- One row per event in `ledger_entries`
  (`crates/server/db/migrations/0002_ledger/up.sql`): `seq`, `event_id`,
  `kind`, `issuer`, `subject`, `payload`, `event_timestamp`, `version`,
  `prev_hash`, `entry_hash`, `committed_at`.
- `entry_hash = SHA-256(prev_hash ‖ event content)`; the first entry's
  `prev_hash` is the all-zero `GENESIS_HASH`. `commit` runs inside one
  transaction and returns the last entry hash as the `Commitment.proof`.
- `verify` checks that a claimed hash exists; `list_entries` rehashes every row
  and checks each link, which is what `avalon inspect-ledger`
  (`crates/cli/src/main.rs`, `make inspect-ledger`) prints.
- Not yet: signatures (#39), a `batch_id` column or real batching (#38 —
  `get_commitment` returns `BatchNotFound`), Merkle roots or signed tree heads
  (#40), an export/mirror format, an outbox so app rows and ledger rows commit
  together ([#71](https://github.com/LunarVagabond/avalon-protocol/issues/71)).
- The ledger shares `avalon-server`'s `PgPool` and migrations; milestone 1 has
  one database. Split when `chain` gets its own deployment, not before.

## Decisions and tickets

- Epic [#36](https://github.com/LunarVagabond/avalon-protocol/issues/36)
  Settlement Ledger
- #68, #70 decided; #40, #79, #39 open decisions
- #38 batching, #71 atomicity,
  [#37](https://github.com/LunarVagabond/avalon-protocol/issues/37) (closed)
  the current provider
- #75 durable history is canonical; #82 event catalogue
