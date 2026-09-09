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
  validator set to run one — a cross-game currency layer remains an
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
  tree's Merkle Tree Hash (MTH) at `tree_size = last_seq`, replacing the
  placeholder chain-tip value — not a per-batch sub-tree, the whole
  ledger's tree as of that batch. Cadence needs no new decision: it's
  already whatever #38/#71's settlement worker does (a batch, and now also
  an STH, closes whenever the outbox drain tick runs).
- **Signed Tree Heads, not per-entry signatures.** #39 resolves to
  STH-only signing, matching real transparency-log precedent — Certificate
  Transparency logs never sign individual certificates, only the tree head;
  an inclusion proof anchored to one valid signed STH already lets anyone
  verify a specific entry belongs to the operator-endorsed tree, with no
  need for the operator to separately sign every entry. One `SignedTreeHead
  { tree_size, root_hash, network_id, timestamp, signing_key_id, signature
  }` is produced per batch commit, in the same transaction, Ed25519,
  covering exactly this settlement-operator key domain (distinct from
  issuer keys, #80, and player keys, #73 — three separate lifecycles, per
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
  computed on demand from `ledger_entries.entry_hash`, ordered by `seq`, in
  Postgres — no embedded engine, no per-node full copy. An incremental
  frontier/proof-cache table is a valid future optimization if proof
  computation cost ever matters at scale; it doesn't change this decision's
  wire format and isn't required to close it.

Implementation tracked as
[#210](https://github.com/LunarVagabond/avalon-protocol/issues/210)
(real Merkle root + signed tree heads, replacing #38's placeholder) and
[#211](https://github.com/LunarVagabond/avalon-protocol/issues/211)
(mirror-facing proof/sync endpoints), both under epic #36.

## Today in the repo

`crates/chain/src/postgres.rs` — `PostgresSettlementProvider`, real and in use:

- One row per event in `ledger_entries`
  (`crates/server/db/migrations/0002_ledger/up.sql`, plus `batch_id` from
  `0014_ledger_batches/up.sql`): `seq`, `event_id`, `kind`, `issuer`,
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
  entries).
- **Real Merkle root + Signed Tree Heads, implemented (#210, closing out
  #39/#40's design — see "What is decided (continued)" above).**
  `crates/chain/src/merkle.rs` implements RFC 6962's Merkle Tree Hash
  (domain-separated leaf/interior hashing, tested against the reference
  vectors published by `transparency-dev/merkle`, the maintained successor
  to Google's original Certificate Transparency Go/Trillian implementation,
  for tree sizes 1 through 8). `commit` reads back every `entry_hash` up to
  and including its batch's `last_seq`, in the same transaction as the
  inserts above, computes that tree's root, and stores it as
  `ledger_batches.batch_root` — the whole ledger's tree as of that batch,
  not a per-batch sub-tree, replacing the placeholder chain-tip value #38
  shipped. `commit` also signs and stores one `SignedTreeHead` per batch
  (`crates/chain/src/sth.rs`, table `signed_tree_heads`,
  `crates/server/db/migrations/0022_signed_tree_heads`) — Ed25519,
  STH-only per #39 (no per-entry signatures), covering
  `(tree_size, root_hash, network_id, timestamp)`. The signing key is
  loaded from `AVALON_SETTLEMENT_SIGNING_KEY` (never stored in Postgres);
  verification needs only `AVALON_SETTLEMENT_VERIFY_KEY` (see
  `.env.example`). `Commitment.proof` now carries this Merkle root rather
  than the old chain-tip value.
- `get_commitment` reads `ledger_batches` by `batch_id`. `verify` runs two
  independent checks, both must pass: it still replays the sequential hash
  chain across a batch's own entries' stored content (unchanged from #38 in
  spirit — content tampering that leaves `entry_hash` stale is caught, just
  no longer compared against `commitment.proof`, which is now a ledger-wide
  value rather than this batch's own chain tip), and it separately
  recomputes the RFC 6962 tree fresh from every `entry_hash` up to the
  batch's `last_seq` and compares that against `commitment.proof` — catching
  tampering with the ledger's structure anywhere up to this batch, not just
  within it. `list_entries` still independently rehashes every row and
  checks each link across the whole ledger, which is what
  `avalon inspect-ledger` (`crates/cli/src/main.rs`, `make inspect-ledger`)
  prints, with batch boundary headers and each batch's root, now followed by
  STH verification (Merkle recompute + Ed25519 signature check against
  `AVALON_SETTLEMENT_VERIFY_KEY`, reported with the same severity as a
  broken hash-chain link on mismatch) — `avalon inspect-ledger-full` /
  `make inspect-ledger-full` is the same view plus each entry's actual JSON
  payload.
- The settlement worker (`crates/server/src/outbox.rs`, #71) drains pending
  `protocol_outbox` rows into one `EventBatch` per drain tick and commits it
  with a single `SettlementProvider::commit` call — a batch closes when the
  worker runs, not on a size threshold or timer, and a single-event batch is
  legal. No handler calls `commit` directly.
- Not yet implemented: mirror-facing proof/sync endpoints (inclusion and
  consistency proofs, latest/historical STH lookup) — tracked as #211,
  which consumes the Merkle tree and STHs this section now describes as
  real.
- **Genesis and network identity (#173).** A singleton `chain_genesis` table
  commits the ledger to a `network_id` (e.g. `avalon-mainnet-1` vs.
  `avalon-dev-<name>`, from the required `AVALON_NETWORK_ID` env var) —
  written once, on `avalon-server`'s first boot against an empty database,
  and never updated after. Every boot after that verifies the running
  process's configured `network_id` against the stored one
  (`PostgresSettlementProvider::connect`); a mismatch is fatal — the process
  exits before binding a listener, not a warning. `network_id` is also
  hashed into every `entry_hash` ahead of the entry's own content, so two
  ledgers with different network identities produce disjoint hash spaces:
  a dev chain's entries cannot be mistaken for, or spliced into, a valid
  link in production's chain, by construction rather than by convention.
  This deliberately does not yet extend to per-event *signatures* (an
  EIP-155-style "signature covers chain identity" guarantee) — that depends
  on a signing scheme that doesn't exist yet (#39/#80/#84) and is left for
  when it does. `avalon inspect-ledger`/`-full` print whichever `network_id`
  the target database already has (read-only, no genesis creation) so an
  operator always sees which network they're looking at.
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
- #68, #70, #79, #186 decided (#93 partially superseded by #186); #40, #39
  decided (Merkle/STH structure, STH-only signing) — #210 (real Merkle root
  + Signed Tree Heads) implemented; #211 (mirror-facing proof/sync
  endpoints) still open
- #38 batching, #71 atomicity, #173 genesis/network identity,
  [#37](https://github.com/LunarVagabond/avalon-protocol/issues/37) (closed)
  the current provider
- #75 durable history is canonical; #82 event catalogue
- #178/#179 (RocksDB-backed provider) reverted (#185), won't-fix per #186
- #80, #84 — issuer key lifecycle, a separate key domain from the
  settlement operator key #39 decided
