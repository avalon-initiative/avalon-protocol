# Settlement

Settlement is the durable-history vertical: the place protocol facts are
committed so that anyone can verify them later. **Settlement is not the general
purpose query database.** **One protocol event is never one settlement
transaction.** **Settlement is a transparency log, not a blockchain — a
hash-chained, append-only, publicly verifiable store with no validator or
consensus layer, because nothing written to it is ever contested.**

## Durable history, not a database

Settlement is responsible for durable commitments, provenance, canonical protocol
history, verifiable attestations, ownership history, durable identity facts,
guild history, integrator/issuer registration, and key lifecycle. It is not
responsible for fast reads — that is [`./query-and-indexing.md`](./query-and-indexing.md).
A mutable Postgres row is never the ultimate authority for something Avalon
promises to preserve forever.

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
Events accumulate in a buffer (the outbox), are batched, a commitment is
computed over the batch, and the commitment is what gets settled. `ledger_entries.batch_id`
groups every entry under the `EventBatch` it was committed with, `ledger_batches` is
the one row per batch that `get_commitment` looks up, and `verify` recomputes a batch's
root from its entries rather than trusting a single stored value. How large a batch is
and how often commitments are produced stay tuning questions — today a batch closes
whenever the settlement worker's drain tick runs (`crates/server/src/outbox.rs`), not
on a size threshold or timer, and a single-event batch is legal — see
[`./scalability.md`](./scalability.md). The batch root is a real RFC 6962 Merkle Tree
Hash over the whole ledger as of that batch.

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
the SDKs, and every integrator integration are indifferent to how a commitment is
produced. Consensus, block production, and P2P networking do not belong on this
trait. Interfaces beyond it are introduced only when an actual implementation
needs them.

## Design decisions

- **Attestations before blockchain.** Durable facts need to *behave like* a
  verifiable ledger — tamper-evident, independently verifiable — which a signed,
  append-only store provides without a chain.
- **The shape is a public transparency log.** Every durable fact is a signed,
  hash-chained, append-only entry; anyone can verify an entry and its position without
  permission; anyone can mirror the log and serve reads. **Federation is rejected** —
  visibility must not depend on which server an integrator trusts. **Avalon does not
  run mining, or consensus staked on a currency,** to referee write ordering; Avalon's
  writes are already unambiguous because each actor signs its own — which is also why
  no validator consensus is needed either, currency-free or not.
- Postgres is the settlement backend, permanently, not a stand-in for something more
  "real." Real production transparency logs (Certificate Transparency, Sigsum,
  Google's Trillian) run on ordinary SQL backends, because verification happens by
  fetching Merkle proofs over the network, not by every party independently holding a
  full replicated copy the way a blockchain full node does. The schema and signing
  scheme make entries independently verifiable and the log exportable; that's a
  property of the data model, not a reason to leave Postgres.
- **No blockchain, no validator/BFT consensus.** Consensus of any kind exists to
  adjudicate contention over a single, shared, mutable, scarce resource — the
  double-spend problem. Nothing settlement records is contested: `identity.created`,
  `achievement.issued`, `friend.requested`, `guild.created`, `game.registered` are each
  a fact asserted by exactly one authoritative signer about something only that
  signer has authority over. There is no native currency or token, and no validator
  set to run one — a cross-integrator currency layer remains an explicitly later,
  optional, separately decided phase ([`../stakeholders/Proposal.md`
  §15](../../../stakeholders/Proposal.md#15-economy-and-currency)): only if that's
  ever actually proposed does consensus become a real question again, scoped to that
  feature specifically. There is no anchoring to, or building on, an existing chain's
  token — there was never a chain to anchor to under this design.

## Transparency log structure

The log's hash/Merkle structure and signing scheme follow the RFC 6962 (Certificate
Transparency) design directly rather than inventing one:

- **Two structures, not one, doing different jobs.** The sequential hash chain
  (`entry_hash = SHA-256(prev_hash ‖ content)`) stays the cheap, O(1)-per-link
  tamper-evidence mechanism that `avalon inspect-ledger` walks end-to-end. Layered on
  top: a single append-only **Merkle tree over the whole ledger's `entry_hash`
  values, ordered by `seq`**, computed with RFC 6962's exact tree-hashing algorithm
  (domain-separated leaf/interior hashing, the standard
  largest-power-of-two-less-than-n split) rather than a bespoke scheme — this is what
  makes succinct inclusion/consistency proofs possible, which a plain hash chain alone
  cannot give a mirror without transferring every entry.
- **`ledger_batches.batch_root` is that tree's root**: the tree's Merkle Tree Hash
  (MTH) over every entry committed so far — not a per-batch sub-tree, the whole
  ledger's tree as of that batch. `tree_size` (the STH's field, stored separately from
  `ledger_batches`) is always the real leaf *count*, never `last_seq` — `seq` is
  `GENERATED ALWAYS AS IDENTITY`, and Postgres identity/sequence advancement isn't
  transactional, so a batch commit that fails partway through and rolls back
  permanently burns whatever `seq` values it had already allocated. Conflating
  `last_seq` with leaf count once a gap like that exists would silently mislabel every
  `tree_size` from that point on; `commit` derives `tree_size` from the actual number
  of rows fetched, not from `last_seq`. A batch, and an STH, closes whenever the
  outbox drain tick runs.
- **Signed Tree Heads, not per-entry signatures.** Certificate Transparency logs never
  sign individual certificates, only the tree head; an inclusion proof anchored to one
  valid signed STH already lets anyone verify a specific entry belongs to the
  operator-endorsed tree, with no need for the operator to separately sign every
  entry. One `SignedTreeHead { tree_size, root_hash, network_id, timestamp,
  signing_key_id, signature }` is produced per batch commit, in the same transaction,
  Ed25519, covering exactly this settlement-operator key domain — distinct from issuer
  keys and identity keys, three separate lifecycles.
- **Mirror sync stays minimal, no witness quorum required with a single settlement
  operator.** The simplest viable protocol suffices: expose the latest STH, a
  historical STH by `tree_size`, RFC 6962 consistency proofs (tree at size A is a
  strict append-only extension of tree at size B), and inclusion proofs (entry at
  `seq` is in the tree at `tree_size`). Any mirror that stores every STH it has
  independently observed can be compared against any other mirror's history for the
  same `tree_size` — a mismatch is cryptographic proof of operator equivocation,
  publishable as misbehavior evidence, without a formal witness-cosigning protocol.
  Witness cosigning (Sigsum-style — a small independent witness set must countersign
  an STH before it's trusted) is the natural strengthening if Avalon ever runs more
  than one independent settlement operator — not needed, and not built, while there's
  only one.
- **Storage**: Merkle proofs computed from `ledger_entries.entry_hash`, ordered by
  `seq` — no embedded engine, no per-node full copy in Postgres. An in-memory
  incremental tree (below) makes that computation O(log n) instead of from-scratch
  every call; it doesn't change this wire format.

## Postgres implementation

`crates/chain/src/postgres.rs`'s `PostgresSettlementProvider` is the real,
in-use `SettlementProvider`.

- **Storage.** One row per event in `ledger_entries`: `seq`, `event_id`, `kind`,
  `issuer`, `subject`, `payload`, `event_timestamp`, `version`, `prev_hash`,
  `entry_hash`, `batch_id`, `committed_at`. One row per batch in `ledger_batches`:
  `batch_id`, `first_seq`, `last_seq`, `batch_root`, `committed_at`.
  `entry_hash = SHA-256(prev_hash ‖ event content)`; the first entry's `prev_hash` is
  the all-zero `GENESIS_HASH`, and entries stay hash-chained across batch boundaries —
  `commit` never resets the chain per batch. `commit` inserts every entry in
  `batch.events` plus the batch's own `ledger_batches` row in one transaction (the
  row-to-batch foreign key is deferred to transaction end, since the batch row is
  inserted after its entries).
- **Merkle root + Signed Tree Heads.** `crates/protocol/src/sth.rs` implements STH
  signing/verification — pure Ed25519 logic with no Postgres dependency, so `chain`
  re-exports it as `avalon_chain::sth` for its own internal callers. `crates/chain/src/merkle.rs`
  implements RFC 6962's Merkle Tree Hash (domain-separated leaf/interior hashing,
  tested against the reference vectors published by `transparency-dev/merkle`, the
  maintained successor to Google's original Certificate Transparency Go/Trillian
  implementation). `commit` reads back every `entry_hash` up to and including its
  batch's `last_seq`, in the same transaction as the inserts above, computes that
  tree's root, and stores it as `ledger_batches.batch_root`. `commit` also signs and
  stores one `SignedTreeHead` per batch (table `signed_tree_heads`) covering
  `(tree_size, root_hash, network_id, timestamp)`. The signing key is loaded from
  `AVALON_SETTLEMENT_SIGNING_KEY` (never stored in Postgres); verification needs only
  `AVALON_SETTLEMENT_VERIFY_KEY` (see `.env.example`). `Commitment.proof` carries this
  Merkle root.
- **Incremental Merkle tree, O(log n) commit and proof-serving.** `crate::merkle`'s
  from-scratch RFC 6962 recomputation is O(n) CPU per proof, O(n^2) total over the
  ledger's life — on a several-thousand-entry ledger, a full mirror backfill (one
  proof request per entry) can take minutes.
  `crate::incremental_merkle::IncrementalMerkleTree` replaces that recomputation with
  the standard Certificate-Transparency "compact range" construction: every completed
  perfect-subtree hash is kept, keyed by `(level, index)`; appending a leaf updates
  O(log n) of them, and any root or proof up to a previously-committed `tree_size` is
  reconstructed by combining O(log n) already-known node hashes rather than rehashing
  every leaf. Every root/proof this produces is required to be byte-identical to
  `crate::merkle`'s from-scratch output, enforced by an exhaustive equivalence test
  sweeping every tree size and index.
  `PostgresSettlementProvider::leaf_cache` holds this tree alongside the leaf-hash
  prefix (`commit`, `root_at`, `inclusion_proof`, `consistency_proof` all read/extend
  it together); any time the cache can't be proven caught up, it's rebuilt from a full
  Postgres re-fetch rather than trusting a possibly-stale tree. **The tree is rebuilt
  in memory on restart, not persisted in Postgres** — it's a derived cache over
  `ledger_entries.entry_hash` (already durable), not new authoritative state, so a
  process restart pays one O(n) rebuild rather than risking a second persisted
  structure drifting from the truth. A persisted `(level, index) -> hash` node table,
  avoiding replay entirely, remains a valid future optimization if startup latency
  ever becomes the bottleneck instead.
- **Verification.** `get_commitment` reads `ledger_batches` by `batch_id`. `verify`
  runs two independent checks, both of which must pass:
  1. A hash-chain check: replay a batch's own entries' stored content across the
     sequential hash chain, entry by entry (`prev_hash == expected_prev` always
     checked, content checked whenever the payload is present) — catches content
     tampering that left `entry_hash` stale. This is per-entry, not all-or-nothing: a
     hot-tier node may have pruned some entries' payloads (not evidence of tampering),
     but that never widens into skipping the check for the batch's other,
     still-complete entries — a batch with one pruned entry and one genuinely
     tampered entry must still fail.
  2. A Merkle check: recompute the RFC 6962 MTH fresh from every `entry_hash` up to
     this batch's `last_seq` and compare against `commitment.proof` — catches
     structural tampering (an `entry_hash` value itself, entry ordering, a deleted
     row) anywhere up to this batch, which a batch-local chain replay alone can't see.
     Built entirely from `entry_hash`, never `payload`, so pruning never affects it.

  `list_entries` independently rehashes every row and checks each link across the
  whole ledger, which is what `avalon inspect-ledger` (`make inspect-ledger`) prints,
  with batch boundary headers and each batch's root, followed by STH verification
  (Merkle recompute + Ed25519 signature check against `AVALON_SETTLEMENT_VERIFY_KEY`,
  reported with the same severity as a broken hash-chain link on mismatch) —
  `avalon inspect-ledger-full` / `make inspect-ledger-full` is the same view plus each
  entry's actual JSON payload.
- **Settlement worker.** `crates/server/src/outbox.rs` drains pending `protocol_outbox`
  rows into one `EventBatch` per drain tick and commits it with a single
  `SettlementProvider::commit` call — a batch closes when the worker runs, not on a
  size threshold or timer, and a single-event batch is legal. No handler calls
  `commit` directly. A drain tick first takes a Postgres session-scoped advisory lock
  (`pg_try_advisory_lock`, `OUTBOX_DRAIN_LOCK_KEY`) before selecting any pending rows;
  a tick that can't acquire it skips entirely rather than blocking, and tries again
  next poll — without this, more than one `avalon-server` process draining the same
  table could both select the same pending rows and both commit them, producing two
  ledger entries for one logical event.
- **Mirror-facing proof/sync endpoints.** `crates/server/src/settlement.rs` exposes
  the read-side API surface a mirror needs, consuming the Merkle tree and STHs above.
  All four are public reads — no session or integrator auth, matching the requirement
  that a transparency log must be independently verifiable by anyone holding only
  `AVALON_SETTLEMENT_VERIFY_KEY`:
  - `GET /ledger/sth/latest` — the current `SignedTreeHead`.
  - `GET /ledger/sth/{tree_size}` — the historical STH at exactly this `tree_size`
    (404 if no batch ever closed at that exact size — STHs exist only per batch
    commit, not at every possible tree size).
  - `GET /ledger/proof/consistency?first={a}&second={b}` — an RFC 6962 consistency
    proof (`crate::merkle::consistency_proof`/`verify_consistency_proof`) that the
    tree at `second` leaves is a strict append-only extension of the tree at `first`
    leaves, plus both sizes' recomputed root hashes for convenience (a real
    verifier's trust anchor should still be an independently-fetched STH, not this
    field alone).
  - `GET /ledger/proof/inclusion?seq={n}&tree_size={s}` — an RFC 6962 inclusion proof
    (`crate::merkle::inclusion_proof`/`verify_inclusion_proof`) that the ledger entry
    at `seq` (a real `ledger_entries.seq` row identifier — **not** a dense position;
    `seq` can have gaps, so the Merkle leaf index is the entry's rank among committed
    entries, resolved via `leaf_index_for_seq`, never `seq - 1`) is included in the
    tree at `tree_size`, plus that entry's `entry_hash` (the proof's leaf input) and
    the tree's root hash. The response also includes `leaf_index` — a one-off
    verifier, unlike a continuously backfilling mirror, has no local backfill
    progress to derive it from.

  Every proof is independently re-verified with the same standalone RFC 6962 verifier
  a remote mirror would use, against the exact root(s) the response claims, before the
  response is ever built — a wrong proof must never leave the process. A
  `seq`/`tree_size` beyond what's actually been committed is a 404, never a
  fabricated or empty proof. `crates/chain/src/merkle.rs` implements the proof
  algorithms themselves — `inclusion_proof`/`consistency_proof` (RFC 6962 §2.1.1's
  `PATH` and §2.1.2's `PROOF`/`SUBPROOF`, reproduced verbatim, sharing `mth`'s
  split-point/node-hash logic) plus their verifiers, unit-tested by round-tripping
  every proof through the reference MTH root vectors, an independent brute-force
  tree-walk cross-check for inclusion proofs, and exhaustive single-node-tamper
  negative tests for both proof types.

  `hash_entry`/`EntryContent` are `pub` — a cross-shard verifier fetching one entry
  from a node it doesn't mirror needs to independently recompute that entry's
  `entry_hash` from fetched content and compare, not just trust a signed root plus a
  structurally-valid inclusion proof (see [`nodes.md`](./nodes.md) for the full
  fetch-and-verify primitive this enables, `crate::cross_shard_fetch`).
- **Mirror-watcher.** Two additive pieces:
  - **`GET /ledger/entries?since_seq={n}&limit={m}`**
    (`crates/server/src/settlement.rs::list_entries`,
    `PostgresSettlementProvider::list_entries_since`) — bulk-content read, public,
    unauthenticated, same posture as every other endpoint in this module; returns full
    entry content in `seq` order, capped at 1000 rows per request. **Not itself a
    verified read** — no hash-chain/content recomputation happens here (a windowed
    query can't validate a page boundary's `prev_hash` against a predecessor it wasn't
    asked to fetch); a caller that needs to trust this content must independently
    verify each entry via `GET /ledger/proof/inclusion` before accepting it.
  - **The mirror-watcher itself** (`crates/server/src/mirror_watcher.rs`) — a
    background task spawned from `avalon-server`'s `main.rs` when `AVALON_MIRROR_PEERS`
    is set, polling every configured peer (default 30s,
    `AVALON_MIRROR_POLL_INTERVAL_SECS`) for its latest STH. This runs in-process rather
    than as a separate daemon, since it needs the same `PgPool`/migrations
    `avalon-server` already has. Push-registered mirrors also get a low-latency
    wake-up on top of polling — the corroboration/equivocation-detection gate below is
    unchanged in shape, since a push only changes when a poll-equivalent tick runs,
    never what it trusts; see [`nodes.md`](./nodes.md) for the full mechanism.

  Each observed STH is signature-verified (`sth::verify_tree_head`) before anything is
  trusted or stored, then recorded in an `observed_sths` table — separate from
  `signed_tree_heads`, which only ever holds STHs *this* node itself produced as a
  Settlement authority.

  **Equivocation detection** (`avalon_chain::mirror::detect_equivocation`, a pure,
  I/O-free function so the security property is directly unit-testable without a
  database): every newly observed STH is compared against every other observation at
  the same `network_id`/`tree_size` — from other peers, and from this node's own
  signed history if it has one (source tag `self:signed-history`). A `root_hash`
  mismatch is durably recorded in `equivocation_findings` *and* logged via a
  structured `tracing::error!` — the durable row is what a human or monitoring system
  checks after the fact, the log line is what an operator watching the process (or a
  log aggregator it forwards to) sees the moment it happens. The log line carries a
  stable `event` field (`"equivocation_detected"` / `"equivocation_resolved"`) plus
  `network_id`/`tree_size`/the two disputed sources and root hashes as their own
  structured fields, so an aggregator can alert on `event = "equivocation_detected"`
  directly without parsing message strings. This is **detection only**: there is no
  automatic "pick the correct STH" resolution logic anywhere in this path,
  deliberately — both STHs in an equivocation are validly signed, so there is no
  automatic correct answer; resolving a real equivocation is a human incident-response
  procedure — see `docs/projects/backend-server/for-maintainers/equivocation-response.md`
  for the operator-facing runbook. `avalon_chain::mirror::resolve_equivocation` records
  which root hash was determined legitimate (`equivocation_findings.resolved_at`/
  `resolved_root_hash`), which is what `backfill_network`'s gate checks
  (`unresolved_equivocations`, not every finding ever recorded).
  `avalon_chain::mirror::discard_mirrored_entries_from` is the recovery half, for a
  mirror that may have already advanced past the fork point on the losing branch.

  **Multi-peer by design.** `AVALON_MIRROR_PEERS` accepts more than one URL, and every
  configured peer is actually watched every tick, not just the first one that answers
  — one unreachable/misbehaving peer never stops the others. Peers are grouped by
  `network_id` and handed to `mirror_watcher::backfill_network`, which (a) refuses to
  backfill a network past any `tree_size` with an already-recorded, unresolved
  equivocation finding, and (b) picks the tree head **this tick's peers most widely
  agree on** — a majority-corroboration gate, not "whichever peer answered first" —
  before trusting it for backfill.

  **Backfill** (`mirror_watcher::backfill`) fetches entries since this network's last
  verified `seq` (`avalon_chain::mirror::mirrored_progress`, keyed on `network_id` —
  not per peer, since any configured peer of the same network is an interchangeable
  source of the same independently-verified content) from `GET /ledger/entries`, and
  for each one, fetches and checks its inclusion proof
  (`merkle::verify_inclusion_proof`) against the corroborated STH before ever storing
  it in a `mirrored_entries` table (`UNIQUE (network_id, seq)`, also not per peer).
  Every request round-robins across the peers that corroborated the chosen tree head —
  if one is unreachable mid-backfill, the next candidate is tried before giving up for
  that tick. A restart is not a special case — backfill just resumes from
  `mirrored_progress`'s last-verified `seq` on the next tick. The whole pass aborts
  (retried next tick) the moment every candidate peer's response for a given entry
  fails to verify, rather than accepting a partial, unverifiable backfill.

  **Feeds the local indexer too, not just `mirrored_entries`.** Once an entry's
  inclusion is verified, `backfill` decodes it into the same `ProtocolEvent` shape
  `outbox::drain_once` builds from `protocol_outbox` rows, and applies it to this
  node's own `PostgresIndexer` (`apply_in_tx`) in the same transaction as the
  `mirrored_entries` insert. This is what lets a node with no Settlement role of its
  own (`AVALON_SETTLEMENT_REMOTE_URL` set — see [`nodes.md`](./nodes.md)) still serve
  real, independently-verified reads from its own local Postgres, rather than either
  sharing another node's database or trusting unverified peer content.

  Mirror storage/verification (`mirrored_entries`/`observed_sths`/
  `equivocation_findings`) is keyed on both `network_id` and `shard_id`, since every
  shard has its own independent `seq`/`tree_size` numbering — a node mirroring more
  than one shard of the same network needs this scoping to keep each shard's
  inclusion-proof verification state independent.
- **`POST /ledger/submit`, node-to-node write endpoint.** The one write route in
  `settlement.rs` — every other endpoint in this section is a public, unauthenticated
  read; this one accepts an `EventBatch` and runs it through the exact same
  `chain.commit` call the local outbox worker already runs for itself, so it is
  privileged rather than public. `outbox::run_worker` calls it instead of committing
  locally whenever `AVALON_SETTLEMENT_REMOTE_URL` is configured — see
  [`nodes.md`](./nodes.md) for the full read/write design. **Auth: a shared-secret
  bearer token**, `AVALON_SETTLEMENT_SUBMIT_KEY`, checked against the `Authorization:
  Bearer <key>` header (`crates/server/src/settlement.rs::submit_ledger_batch`),
  covering trusted node-to-node calls where neither a user session nor an integrator's
  registered credential fits ("one operator's own two nodes talking to each other"). A
  node with no `AVALON_SETTLEMENT_SUBMIT_KEY` configured refuses every request to
  this endpoint outright, rather than leaving it open.
- **`GET /ledger/remote-submit-status`** (`crate::settlement::remote_submit_status`,
  public, unauthenticated — the URL a node forwards to isn't sensitive, and gating it
  would defeat the point for exactly the misconfigured caller this helps) answers with
  `failing_shards`: every shard currently failing to reach its configured authority,
  each entry's `authority` being exactly that shard's own already-configured
  `AVALON_SETTLEMENT_REMOTE_URL(S)` value — never an inferred or alternate one.
  **503** while any shard is failing, **200** with an empty list otherwise (including
  when this node forwards nothing at all). This is a discovery hint only, not a
  failover mechanism: exactly one Settlement authority is still expected per shard.
  Out of scope: the authority itself being down — there is no "other node" to point at
  there, and hold-and-retry against the same authority is still correct.
- **Genesis and network identity.** A singleton `chain_genesis` table commits the
  ledger to a `network_id` (e.g. `avalon-mainnet-1` vs. `avalon-dev-<name>`, from the
  required `AVALON_NETWORK_ID` env var) — written once, on `avalon-server`'s first boot
  against an empty database, and never updated after. `network_id` alone carries no
  cryptographic weight — see [`network-trust-anchors.md`](./network-trust-anchors.md)
  for how a client pins it to the settlement operator's actual key today, and
  [`witness-cosigning.md`](./witness-cosigning.md) for the in-progress replacement of
  that single pinned key with a bounded, self-filling list of witnesses. Every boot after
  that verifies the running process's configured `network_id` against the stored one
  (`PostgresSettlementProvider::connect`); a mismatch is fatal — the process exits
  before binding a listener, not a warning. `network_id` is also hashed into every
  `entry_hash` ahead of the entry's own content, so two ledgers with different network
  identities produce disjoint hash spaces: a dev chain's entries cannot be mistaken
  for, or spliced into, a valid link in production's chain, by construction rather than
  by convention. This does not yet extend to per-event *signatures* (an
  EIP-155-style "signature covers chain identity" guarantee) — that depends on a
  signing scheme that isn't built yet, and is left for when it is. `avalon
  inspect-ledger`/`-full` print whichever `network_id` the target database already has
  (read-only, no genesis creation) so an operator always sees which network they're
  looking at.
- The ledger shares `avalon-server`'s `PgPool` and migrations; there is one database.
  Splitting is a future step when `chain` gets its own deployment.
- **`list_entries_for_issuer_prefix`** — a narrower, unverified issuer-filtered read
  (no hash/link recomputation, unlike `list_entries`), behind `GET /me/history`
  (`crates/server/src/handlers.rs::my_history`). It reads the ledger directly rather
  than through the indexer, since `crates/indexer` does not yet have a fully general
  projection store — a ledger read is the only real read path today. This stays a
  narrow, server-constructed, caller-scoped read (the issuer prefix is always built
  from the authenticated identity id, never accepted as a request parameter) rather
  than a general query surface, so it doesn't cross settlement's "not the
  general-purpose query database" boundary. See [query-and-indexing.md](./query-and-indexing.md).
- **Node-tiered durable history retention.** Settlement commitment and durable event
  storage are separate retention problems: the hash chain, the Merkle tree, and Signed
  Tree Heads are untouched by retention tier. What tiers is `ledger_entries.payload`
  specifically, nullable. `crates/chain/src/retention.rs` is the config
  (`AVALON_RETENTION_TIER`, `AVALON_RETENTION_HOT_WINDOW_DAYS`,
  `AVALON_RETENTION_PRUNING_ENABLED`) and cutoff logic;
  `PostgresSettlementProvider::prune_payloads_older_than` is the one-column `UPDATE`
  that actually prunes, driven by `crates/server/src/retention.rs`'s hourly worker
  when enabled, or manually via `avalon prune-ledger [--dry-run]`. `verify` and
  `list_entries` both treat a pruned entry's missing payload as "not independently
  re-checkable from here," never as tamper evidence — `verify`'s Merkle check (built
  entirely from `entry_hash`, never `payload`) still runs and still must pass for a
  batch to verify, even once every one of its entries' payloads is pruned; only the
  redundant hash-chain content-replay check (which does need payload) is skipped for
  a batch with any pruned entry. See [`nodes.md`](./nodes.md)'s "Settlement retention
  tiers" section for the full tier/config design.
- **Settlement-state checkpoint.** The latest `SignedTreeHead` is the checkpoint — a
  known, signed `(tree_size, root_hash)` at a known height, produced every batch
  commit with no new storage needed. `PostgresSettlementProvider::checkpoint()` is a
  purely-named alias over `latest_signed_tree_head`, making that intent explicit at
  the call site. An indexer/projection read-model snapshot for fast rebuild is not yet
  built — `crates/indexer` has real projections but no rebuild-speed or
  snapshot-format work yet, so that stays an open follow-up.

## `verify`'s two independent checks

Both must pass. (1) A hash-chain check: replay a batch's own entries' stored content
across the sequential hash chain, entry by entry (`prev_hash == expected_prev` always
checked, content checked whenever the payload is present) — catches content tampering
that left `entry_hash` stale. This is per-entry, not all-or-nothing: a hot-tier node
may have pruned some entries' payloads (not evidence of tampering), but that never
widens into skipping the check for the batch's other, still-complete entries — a batch
with one pruned entry and one genuinely tampered entry must still fail. (2) A Merkle
check: recompute the RFC 6962 MTH fresh from every `entry_hash` up to this batch's
`last_seq` and compare against `commitment.proof` — catches structural tampering (an
`entry_hash` value itself, entry ordering, a deleted row) anywhere up to this batch,
which a batch-local chain replay alone can't see. Built entirely from `entry_hash`,
never `payload`, so pruning never affects it either way.

## Cross-shard commitment

Sharded settlement authority is per-integrator instead of one operator, with an
explicit invariant: the cross-shard root must be independently computable by any node
from public inputs — no designated "gluer"/aggregator node, since aggregation itself
would just become a new single point of failure.

**Each shard keeps exactly what a single-operator ledger has today.** A shard is a
`PostgresSettlementProvider`-style log with its own hash chain, Merkle tree, and STH,
signed by that shard's own settlement key — unchanged from everything above. Sharding
adds one more layer on top; it does not change how any individual shard works
internally, and existing single-shard inclusion/consistency proofs keep working
unchanged from a client's perspective.

**Shard identity and registration.** Each shard has a stable `shard_id` (the sharded
integrator's own slug/id, already unique under the per-integrator model). A shard
announces itself to the network with a durable, signed `shard.registered` event
(`kind`, `shard_id`, `shard_id`'s settlement public key, first-known STH), gossiped the
same way node announcements already propagate via the peer table — not a new
transport. This is what lets a newly-joined shard get included without a coordinated
"everyone add this shard" event: the shard simply announces itself, and any node that
has received the announcement now knows to include it.

**Canonical ordering: sort by `shard_id`, byte-wise.** The cross-shard tree's leaves
are `(shard_id, shard_sth)` pairs, sorted by `shard_id` as a plain byte-wise (UTF-8)
ascending sort — no registration-order, join-time, or other stateful ordering. This is
what makes the aggregation deterministic: two nodes with the same *set* of
currently-known shard STHs always produce the same sorted leaf order, and therefore
the same tree, regardless of the order they happened to learn about each shard in.

**Each shard's contributing STH is verified by majority witness cosignature,
not a bare signature** (#938, see `docs/projects/backend-server/architecture/witness-cosigning.md`) — a
degenerate known list of size 0 or 1 (today's and most real deployments')
behaves exactly like the plain signature check described below; a larger
known list additionally requires a majority of it to have cosigned before a
shard's STH is trusted enough to contribute a leaf. A shard whose STH fails
this check is folded into `missing_shard_ids` the same as an unreachable
one, never included unverified.

**Aggregation recipe.** For each currently-known shard, compute a leaf hash
`SHA-256(shard_id ‖ sth.tree_size ‖ sth.root_hash ‖ sth.signing_key_id ‖
sth.signature)` — the STH's own signature is included in the leaf so the cross-shard
root also commits to *which* STH (not just which shard) was aggregated, making a
shard's downgrade to a stale/rolled-back STH detectable the same way any Merkle
inclusion mismatch is. Leaves are sorted by `shard_id` (see above) and combined with
the exact same RFC 6962 tree-hashing algorithm the single-shard Merkle tree already
uses — no new hashing scheme, just a second tree of the same shape, one level up. The
result is a `CrossShardRoot { root_hash, shard_count, computed_at }` that any node can
recompute byte-for-byte from public gossip alone. This is deliberately *not* itself
STH-signed by any one party — signing it would reintroduce exactly the
designated-aggregator chokepoint this design exists to remove. A node that wants to
publish "the network's current global state as I see it" publishes the
`CrossShardRoot` plus the full list of `(shard_id, sth)` pairs it used, so anyone can
independently verify by recomputing.

**Detecting a missing or stale shard, instead of silently disagreeing.** A node tracks
two sets: shards it has ever seen a `shard.registered` event for ("known shards"), and
shards it currently holds an STH for ("STH'd shards"). A `CrossShardRoot` computed
while `STH'd shards` is a strict subset of `known shards` is marked `partial: true`
and lists exactly which `shard_id`s are missing, rather than silently producing a root
over whatever subset happens to be on hand. A node with a `partial` root should not be
treated as authoritative for the shards it's missing; it can still serve full proofs
for shards it does have complete STHs for.

**Verifying "is shard X part of mainnet's current global state."** Given a
`CrossShardRoot`, the full list of `(shard_id, sth)` pairs it was computed from, and a
standard Merkle inclusion proof for shard X's leaf within that list — any node (or
mirror, or client) can verify shard X's current STH is part of that specific
cross-shard root using the same inclusion-proof verification code the single-shard
tree already has, applied one level up. No new proof type; the cross-shard tree is
verified exactly like the per-shard tree is.

**What this does not change.** No consensus, no ordering across shards — this is
purely an aggregation/commitment structure over independently authoritative logs. A
network with exactly one shard degenerates to a `CrossShardRoot` over a single leaf,
which is trivially equal to that shard's own STH in everything but shape — the
single-operator case earlier in this document is the one-shard special case of this
design, not a separate code path.

**Implementation.** `avalon_chain::cross_shard` is the pure aggregation math —
`compute_cross_shard_root`/`compute_cross_shard_root_checked` take a
`Vec<ShardTreeHead>` (already-verified `(shard_id, SignedTreeHead)` pairs, from
wherever a node gathered them) and produce a `CrossShardRoot`, reusing the exact same
RFC 6962 `merkle::mth`/`inclusion_proof`/`verify_inclusion_proof` the per-shard tree
already uses — no new hashing scheme, no new proof type. `avalon_server::cross_shard`
is the network-facing half: `GET /ledger/cross-shard-root` fetches each configured
known shard's `/ledger/sth/latest` (the same read any mirror already uses), verifies
it, and aggregates — or, with no shards configured or discovered, falls back to this
node's own local STH as the one-shard degenerate case, no network calls. Shard
discovery is not only config-based (see "Automatic shard discovery" below):
`AVALON_KNOWN_SHARDS`/`AVALON_SHARD_VERIFY_KEYS` remain valid, explicit, narrower
configuration, but a node with none configured at all can aggregate a real cross-shard
root purely from shards it discovered via peer-announce gossip. Trust is unaffected
either way: real per-shard key resolution
(`crate::cross_shard::resolve_shard_verify_keys_from_db`, reading `issuer_keys` rows
with `purpose = 'shard_settlement'`) is tried first for a configured and a discovered
shard alike — a shard with no key resolved from either that or the static fallback, or
a failed fetch/verify, is folded into `missing_shard_ids` exactly like a shard this
node has never heard from, never silently included unverified.

Verified live: two independent `avalon-server` processes, both configured with the
identical known-shards set, compute byte-for-byte identical `root_hash` values
(`crates/server/tests/cross_shard.rs`), plus the pure aggregation math's own
determinism/partial-detection/inclusion-proof unit tests
(`crates/chain/src/cross_shard.rs`).

### Write routing to the correct shard

Sharding is per-integrator by candidate — but not every durable event has an
integrator to shard by. A `ProtocolEvent`'s `issuer` is a
[`GlobalId`](../../../../crates/protocol/src/ids.rs)
(`<namespace>:<owner>:<kind>:<key>`); its `namespace` is exactly the signal write
routing needs:

- **`namespace` is `game`/`app`/`service`** (an integrator acting as itself —
  achievement issuance, attestation revocation, integrator-owned connections/schema
  events) — routes to that integrator's own shard, `shard_id = "{namespace}:{owner}"`.
- **`namespace` is `identity`** (identity/social-graph/guild events —
  `identity.created`, `friend.*`, `guild.*`, and everything else with no single owning
  integrator) — routes to a single reserved **core shard** (`shard_id = "core"`), not
  split per-integrator. This is a deliberately narrower scope than "every event is
  sharded": identity/social/guild history has no natural per-integrator partition (a
  friendship or guild isn't owned by any one integrator), so those stay on one shared
  log. The core shard is not exempt from the sharding model in principle — it can
  itself become a managed/shard-operator-run shard like any other — it just isn't
  split further by this design.
- `outbox::drain_once` groups pending rows by this derived `shard_id` before building
  a batch (one batch per shard per tick), and looks up that shard's own commit target:
  local `chain.commit` if this node holds that shard's own signing key, otherwise the
  shard's own configured remote authority (`AVALON_SETTLEMENT_REMOTE_URLS`, a
  `shard_id=url` map — the existing singular `AVALON_SETTLEMENT_REMOTE_URL` env var
  keeps working unchanged as the implicit `core=<url>` entry).

A node's ledger is its shard: the reserved `core` label only names where
identity-issued events *route*; when no remote authority is configured for that
label the handling node commits them locally, into the ledger of whichever shard it
authors (`AVALON_OWN_SHARD_ID`). `core` denotes the network's pinned core authority
alone, and every other node authors a named, registered shard. Shard ids may carry a
sibling suffix, `{namespace}:{owner}[/{instance}]` (for example `game:wow/1`,
`game:wow/2`): key resolution (`resolve_shard_verify_keys_from_db`) uses `owner`
only, so all siblings verify against the same integrator's `shard_settlement` keys,
while each sibling keeps its own ledger and tree heads and no ledgers are merged.
Integrator-issued events keep routing to `{namespace}:{owner}`; a sibling receives
writes only when it is configured as the remote authority for its exact shard id. The
core authority is also the trust root registrar: an integrator and its
`shard_settlement` key become known to the network through events recorded in its
ledger. See [`nodes.md`](./nodes.md) for the startup guard that refuses an unpinned
`core` author.

This keeps exactly one legitimate write authority per shard, never contested — routing
picks *which* shard's authority to use, it never introduces a second writer for the
same shard. A deployment with no `AVALON_SETTLEMENT_REMOTE_URLS` configured has
exactly one shard (`core`) and behaves exactly as today.

`outbox::shard_id_for_event` derives the shard exactly as designed above;
`RemoteSubmitConfig::from_env` parses `AVALON_SETTLEMENT_REMOTE_URLS` into a
`shard_id -> url` map, merging in `AVALON_SETTLEMENT_REMOTE_URL` as the implicit
`core` entry; `drain_locked` groups a tick's pending rows by shard and commits one
independent `EventBatch` per shard (never mixing two shards into one batch).
Live-verified: a single drain tick spanning both a `core`-shard event and a
`game:...`-shard event produced two distinct `ledger_entries.batch_id` values
(`crates/server/src/outbox.rs`'s
`a_tick_spanning_two_shards_commits_two_separate_batches`), and the full existing live
suite (real achievement issuance, Integrator Space instance publication, settlement
proof endpoints — all real `game:...`-namespace writes) re-runs unchanged against the
new routing with no regression.

This has also been verified cross-machine, not only with two local processes sharing
one Postgres: a second node genuinely acts as the remote authority for a second,
distinct `game:...` shard, with its own real, registered `shard_settlement`
operational key (`POST /integrations/{slug}/keys` with `purpose: shard_settlement`,
resolved server-side by `crate::cross_shard::resolve_shard_verify_keys_from_db`), not a
shared operator key. Enqueuing a real event for that shard on the primary routes it
over the real network to the remote node, which commits it locally (landing in its own
`ledger_entries`, distinct from the `mirrored_entries` its core-shard mirroring also
populates) and signs its own real STH with that shard's registered key. The primary's
`GET /ledger/cross-shard-root` independently verifies that STH against the
DB-resolved key and produces a real, non-partial two-node cross-shard root.

### Automatic shard discovery

Everything above still assumed an operator already knew a shard existed and
hand-listed it in `AVALON_KNOWN_SHARDS`/`AVALON_MIRROR_PEERS`. At real scale — many
independent shard operators onboarding over a mainnet's life — that manual step is
itself a gap: a node's own picture of "which shards exist on this network" can
silently fragment, with no guarantee any given node has the full set. Two layers of
epidemic/gossip-style propagation over a node's existing bounded peer connections
close this, deliberately **not** a registry/directory node type (see
`docs/projects/backend-server/architecture/distributed-topology.md`'s "no central
dependency" stance).

**Layer 1 (`crates/server/src/nodes.rs`): a node's active announce/exchange peer set
grows past its bootstrap list.** `nodes::run_worker` maintains its own growing
`active_peers` list, seeded from the bootstrap set (never evicted — still how a
brand-new node reaches the mesh at all) and extended, capped by
`AVALON_NODE_MAX_PEERS` (default 50), with peers discovered via announce responses —
the same bounded-fan-out/full-eventual-reach property Kademlia's k-bucket maintenance
and gossip-membership protocols (SWIM, HyParView) rely on. A peer discovered through
one of the bootstrap peers is recorded in the local, passive `PeerTable` (so it shows
up in `GET /nodes/peers`) and also becomes an ongoing announce target itself, so
propagation continues past one hop. A peer that stops re-announcing is dropped both
from the passive `PeerTable` and from `active_peers` (unless it's a bootstrap peer),
freeing a slot for the network's continued fan-out rather than pinning a dead one
forever.

**Layer 2 (also `nodes.rs`): shard-existence gossip rides on the same mechanism**, the
same way DHT identity rides along announce. `ShardRegistry` is a node's anti-entropy
view of "every shard I currently know exists, and a URL claiming to serve it" —
gossiped bidirectionally on every announce exchange (`AnnounceRequest`/
`AnnounceResponse` both carry a `known_shards` snapshot), not looked up via a single
fixed key, since enumerating the *full set* of everything that exists is what gossip
solves and a DHT's point-lookup model doesn't. A node authoritative for a shard (it has
real local, signed settlement history for it — checked via
`chain.latest_signed_tree_head()`) records its own claim into the registry every tick,
so it's included in what it gossips out. `crate::cross_shard::combined_shard_urls`
unions this registry with `AVALON_KNOWN_SHARDS` (static config wins on overlap) for
`GET /ledger/cross-shard-root`'s own aggregation — so a node with zero
`AVALON_KNOWN_SHARDS` configured at all can compute a real, non-degenerate cross-shard
root purely from what it discovered.

**Trust is completely unchanged.** Discovering a shard's existence never implies
trusting it — a discovered shard's STH still goes through exactly the same
`crate::cross_shard::resolve_shard_verify_keys_from_db` key resolution/verification a
config-listed shard already uses; an unverifiable, unreachable, or
not-yet-issuer-registered discovered shard is folded into `missing_shard_ids` like any
other, never trusted on the strength of merely being gossiped.

**Auto-mirroring a discovered shard is opt-in**, separate from discovery itself —
mirroring N shards' full history is a real resource cost an operator should still
choose. `AVALON_MIRROR_ALL_DISCOVERED_SHARDS=true`
(`crates/server/src/mirror_watcher.rs`) has the mirror-watcher scan `ShardRegistry`
each tick for any shard not already named in `AVALON_MIRROR_PEERS`, verify its STH via
the same key-resolution mechanism (a deliberately separate path from
`AVALON_MIRROR_PEERS`'s own network-trust-anchor verification — a shard's settlement
key is integrator-registered, not pinned in `docs/trusted-networks.json`), and, once
verified, feed it into the same backfill machinery any statically configured mirror
peer already uses. `AVALON_KNOWN_SHARDS`/`AVALON_MIRROR_PEERS` remain valid,
unchanged, narrower configuration — this is additive, and a node can auto-mirror
discovered shards with zero explicit peer configuration at all.

Verified live across a multi-machine test topology: a node bootstrapped only from a
second node, which was itself bootstrapped only from a third, discovers and begins
actively exchanging with the third node it was never directly configured to talk to; a
shard authoritative on one machine is discovered, resolved, and verified from a peer
with zero prior config about it. When mirroring more than one shard of the same
network, fetching a discovered shard's STH must include the `?shard_id=` query
parameter the same way `crate::cross_shard`'s own fetch does — `fetch_latest_sth`
takes an explicit `shard_id: Option<&str>`, sent as a query param when given, so
verification against the correct shard's STH doesn't silently target the wrong one on
a peer that serves more than one shard.

### Discovering a forwarding node's configured authority

A forwarding node whose remote-submit attempts are failing (its configured authority
unreachable or rejecting it) only logs that locally — an integrator whose SDK ends up
misconfigured against the wrong node (a gateway/indexer rather than the real
Settlement authority) has no way to discover the correct target from the error alone.
`GET /ledger/remote-submit-status` answers this — see above.

## Bounding ledger growth

Standing rule: **high-frequency ephemeral data never touches the settlement ledger,
full stop.** Chat (`guild_messages.rs`) and DM conversation content
(`conversations.rs`) already follow this — neither calls `outbox::enqueue`, each
enforced by a dedicated grep-based test (`crates/server/tests/guild_messages_no_ledger.rs`,
`crates/server/tests/conversations_no_ledger.rs`) rather than left as an unenforced
convention. Any new feature considering a write to the durable ledger should default
to this same question first: is this paced by real, human-scale actions (identity,
friendship, guild membership, achievements — what the ledger already carries), or is
it machine-speed/high-cardinality data that belongs in the indexer's rebuildable
projections instead? When it's the latter, add the same grep-test enforcement other
ledger-adjacent modules already use rather than trusting review alone to catch it
later.

This bounds *new* unbounded growth from careless design choices. It does not bound two
things that remain genuinely open regardless of this rule: how many events one issuer
can write about one subject, and making a subject's own history cheap to sync
selectively without replaying the whole ledger. Both are real, organic-use costs this
rule doesn't touch. A structural bound on long-run per-subject growth for legitimate,
ongoing use (log compaction/checkpointing) has been considered and deliberately
deferred — real design work, not needed at this project's current scale.

**Per-issuer-subject write quota** — a volume-only abuse floor on `(issuer, subject)`
attestation writes, enforced in `crates/server/src/achievements.rs`'s
`issue_attestation`/`bulk_issue_attestation` before either does any signature
verification: a rolling window (`AVALON_ACHIEVEMENT_WRITE_QUOTA`,
`AVALON_ACHIEVEMENT_WRITE_QUOTA_WINDOW_HOURS`, generous defaults) counts recent writes
for that exact `(issuer, subject)` pair and rejects (`429
ATTESTATION_WRITE_QUOTA_EXCEEDED`) a write, or a whole bulk call, that would cross it —
never a partial application, and it never inspects *what* is being attested to, only
volume. Scoped to `achievement_attestations` writes specifically, matching the rule
above — chat/presence never reach this quota because they never reach the ledger.

**Subject-scoped selective sync** — `GET /ledger/entries?subject=` pre-filters the
existing bulk entries endpoint to one subject's own entries, same pagination/ordering
semantics as the unfiltered form, backed by a `(subject, seq)` composite index.
`subject` matches the *exact* compound value `ledger_entries.subject` stores — e.g.
`identity:<uuid>:self:achievement_issued` — not just the bare owner id, since every
event-producing module in this codebase bakes its own verb into `subject`
(`issuer_ref`/`identity_ref`/`guild_ref` helpers). A caller filters on the exact string
it already knows it wrote; there's no owner-level "everything about this identity
regardless of verb" query yet. A subject with no entries returns an empty list, not an
error. Filtering never changes an entry's hash-chain position — inclusion proofs for a
filtered row still verify against the same global tree.
