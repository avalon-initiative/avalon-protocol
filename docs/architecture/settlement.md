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

## What is open

**The consensus and validator design** — now scoped inside
[#40](https://github.com/LunarVagabond/avalon-protocol/issues/40) alongside
the log's own hash/Merkle/signed-tree-head design: which BFT algorithm,
validator admission and rotation, block/round cadence and finality. Needs its
own research spike and written comparison before it closes.

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
- #68, #70, #79, #93 decided; #40, #39 open engineering decisions
- #38 batching, #71 atomicity,
  [#37](https://github.com/LunarVagabond/avalon-protocol/issues/37) (closed)
  the current provider
- #75 durable history is canonical; #82 event catalogue
