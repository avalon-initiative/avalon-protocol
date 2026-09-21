# Settlement: Implementation Notes

Full implementation detail behind [`./settlement.md`](./settlement.md)'s
`PostgresSettlementProvider` — the real Postgres-backed hash-chained,
Merkle-rooted ledger. Split into its own file so the main document can stay
focused on the design (durable history, batching, the transparency-log
structure) rather than being interrupted by the build-by-build detail below.

Same discipline as everywhere else in this project's `architecture/` tree: if this doc and
the actual code ever disagree, the code is right and this doc is stale.

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
  `crates/server/db/migrations/0024_signed_tree_heads`) — Ed25519,
  STH-only per #39 (no per-entry signatures), covering
  `(tree_size, root_hash, network_id, timestamp)`. The signing key is
  loaded from `AVALON_SETTLEMENT_SIGNING_KEY` (never stored in Postgres);
  verification needs only `AVALON_SETTLEMENT_VERIFY_KEY` (see
  `.env.example`). `Commitment.proof` now carries this Merkle root rather
  than the old chain-tip value.
- **Incremental Merkle tree, O(log n) commit and proof-serving (#349).**
  `commit` and `entry_hashes_up_to` used to re-fetch every `entry_hash` on
  every call (fixed by `leaf_cache`, an earlier same-night mitigation), but
  `crate::merkle`'s `mth`/`path` still recomputed the whole RFC 6962 tree
  from raw leaves every time — O(n) CPU per proof, O(n^2) total over the
  ledger's life. Found via a live `make test-live` run against a ~6,000-entry
  dev ledger: a full mirror backfill (one proof request per entry) took
  minutes. `crate::incremental_merkle::IncrementalMerkleTree` replaces that
  from-scratch recomputation with the standard Certificate-Transparency
  "compact range" construction: every completed perfect-subtree hash is
  kept, keyed by `(level, index)`; appending a leaf updates O(log n) of
  them, and any root or proof up to a previously-committed `tree_size` is
  reconstructed by combining O(log n) already-known node hashes rather than
  rehashing every leaf. Every root/proof this produces is required to be
  byte-identical to `crate::merkle`'s from-scratch output — enforced by an
  exhaustive equivalence test sweeping every tree size and index.
  `PostgresSettlementProvider::leaf_cache` now holds this tree alongside
  the leaf-hash prefix (`commit`, `root_at`, `inclusion_proof`,
  `consistency_proof` all read/extend it together), preserving the same
  sole-writer-assumed, correctness-first fallback posture as before: any
  time the cache can't be proven caught up, it's rebuilt from a full
  Postgres re-fetch rather than trusting a possibly-stale tree.
  **Design call: rebuilt in memory, not persisted in Postgres.** The tree
  is a derived cache over `ledger_entries.entry_hash` (already durable),
  not new authoritative state, so a process restart just pays one O(n)
  rebuild rather than risking a second persisted structure drifting from
  the truth — a real cost at very large ledger sizes, and a valid future
  optimization (a persisted `(level, index) -> hash` node table, avoiding
  replay entirely) if startup latency ever becomes the bottleneck instead.
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
  - **Mutual exclusion across processes (#536).** A drain tick first takes a
    Postgres session-scoped advisory lock (`pg_try_advisory_lock`,
    `OUTBOX_DRAIN_LOCK_KEY`) before selecting any pending rows; a tick that
    can't acquire it skips entirely rather than blocking, and tries again
    next poll. Without this, more than one `avalon-server` process draining
    the same table could both select the same pending rows and both commit
    them, producing two ledger entries for one logical event — a real,
    previously-unfixed gap, not a hypothetical one, live-verified via
    `outbox::tests::concurrent_drains_do_not_double_commit`.
- **Mirror-facing proof/sync endpoints, implemented (#211).**
  `crates/server/src/settlement.rs` exposes the read-side API surface #40's
  mirror-sync design calls for, consuming the Merkle tree and STHs above.
  All four are public reads — no session or integrator auth, matching the
  decision that a transparency log must be independently verifiable by
  anyone holding only `AVALON_SETTLEMENT_VERIFY_KEY`:
  - `GET /ledger/sth/latest` — the current `SignedTreeHead`.
  - `GET /ledger/sth/{tree_size}` — the historical STH at exactly this
    `tree_size` (404 if no batch ever closed at that exact size — STHs
    exist only per batch commit, not at every possible tree size).
  - `GET /ledger/proof/consistency?first={a}&second={b}` — an RFC 6962
    consistency proof (`crate::merkle::consistency_proof`/
    `verify_consistency_proof`) that the tree at `second` leaves is a
    strict append-only extension of the tree at `first` leaves, plus both
    sizes' recomputed root hashes for convenience (a real verifier's trust
    anchor should still be an independently-fetched STH, not this field
    alone).
  - `GET /ledger/proof/inclusion?seq={n}&tree_size={s}` — an RFC 6962
    inclusion proof (`crate::merkle::inclusion_proof`/
    `verify_inclusion_proof`) that the ledger entry at `seq` (a real
    `ledger_entries.seq` row identifier — **not** a dense position; `seq`
    can have gaps, see `crates/chain/src/postgres.rs`'s module doc comment,
    so the Merkle leaf index is the entry's rank among committed entries,
    resolved via `leaf_index_for_seq`, never `seq - 1`) is included in the
    tree at `tree_size`, plus that entry's `entry_hash` (the proof's leaf
    input — no entry payload content, same value `list_entries`/`inspect-ledger` already
    expose) and the tree's root hash.
  - Every proof is independently re-verified with the same standalone RFC
    6962 verifier a remote mirror would use, against the exact root(s) the
    response claims, before the response is ever built — the ticket's own
    invariant that a wrong proof must never leave the process. A
    `seq`/`tree_size` beyond what's actually been committed is a 404, never
    a fabricated or empty proof.
  - `crates/chain/src/merkle.rs` implements the proof algorithms
    themselves — `inclusion_proof`/`consistency_proof` (RFC 6962 §2.1.1's
    `PATH` and §2.1.2's `PROOF`/`SUBPROOF`, reproduced verbatim, sharing
    `mth`'s split-point/node-hash logic rather than a separate
    reimplementation) plus their verifiers, unit-tested by round-tripping
    every proof through the reference MTH root vectors #210 already
    sourced from `transparency-dev/merkle`, an independent brute-force
    tree-walk cross-check for inclusion proofs, and exhaustive
    single-node-tamper negative tests for both proof types.
- **Mirror-watcher, implemented (#299, closing out #40's "any mirror that
  independently observes and stores every STH it sees" design).** Two new
  pieces, both additive — nothing above this bullet changes:
  - **`GET /ledger/entries?since_seq={n}&limit={m}`**
    (`crates/server/src/settlement.rs::list_entries`,
    `PostgresSettlementProvider::list_entries_since`) — the bulk-content
    read #211 didn't cover (`entry_hashes_up_to` only ever returned bare
    hashes for Merkle computation). Public, unauthenticated, same posture
    as every other endpoint in this module; returns full entry content in
    `seq` order, capped at 1000 rows per request. **Not itself a verified
    read** — no hash-chain/content recomputation happens here (a windowed
    query can't validate a page boundary's `prev_hash` against a
    predecessor it wasn't asked to fetch); a caller that needs to trust
    this content must independently verify each entry via `GET
    /ledger/proof/inclusion` before accepting it.
  - **The mirror-watcher itself**
    (`crates/server/src/mirror_watcher.rs`) — a background task spawned
    from `avalon-server`'s `main.rs` when `AVALON_MIRROR_PEERS` is set
    (same "only spawn what's configured" pattern the retention worker
    uses), polling every configured peer (default 30s,
    `AVALON_MIRROR_POLL_INTERVAL_SECS`) for its latest STH. Chose
    in-process over a separate `avalon mirror-watch` CLI subcommand
    because `avalon` today is a short-lived diagnostic tool, not a daemon,
    and this needs the same `PgPool`/migrations `avalon-server` already
    has — see the module's own doc comment for the full reasoning. The
    POC's "2 Settlement nodes" topology is two `avalon-server` deployments,
    each against its own Postgres; a "mirror" is just one of them started
    with `AVALON_MIRROR_PEERS` pointed at the other.
  - Each observed STH is signature-verified (`sth::verify_tree_head`,
    reused as-is — never reimplemented) before anything is trusted or
    stored, then recorded in a new `observed_sths` table
    (`crates/server/db/migrations/0041_mirror_observations`) — separate
    from `signed_tree_heads`, which only ever holds STHs *this* node
    itself produced as a Settlement authority.
  - **Equivocation detection** (`avalon_chain::mirror::detect_equivocation`,
    deliberately a pure, I/O-free function so the actual security property
    is directly unit-testable without a database): every newly observed
    STH is compared against every other observation at the same
    `network_id`/`tree_size` — from other peers, and from this node's own
    signed history if it has one (source tag `self:signed-history`). A
    `root_hash` mismatch is durably recorded in `equivocation_findings`
    *and* logged via a structured `tracing::error!` — belt and suspenders,
    matching how `outbox::drain_once` already treats its own "must never
    fail silently" case: the durable row is what a human or monitoring
    system checks after the fact, the log line is what an operator watching
    the process (or a log aggregator it forwards to via #265's
    `AVALON_LOG_FORMAT=json`) sees the moment it happens. The log line
    carries a stable `event` field (`"equivocation_detected"` /
    `"equivocation_resolved"`) plus `network_id`/`tree_size`/the two
    disputed sources and root hashes as their own structured fields rather
    than folded into the message text, so an aggregator can alert on
    `event = "equivocation_detected"` directly without parsing message
    strings — no alerting/paging integration ships in this repo itself
    (issue #315). This ticket owns **detection only**: there is no automatic "pick the
    correct STH" resolution logic anywhere in this path, deliberately —
    both STHs in an equivocation are validly signed, so there is no
    automatic correct answer; resolving a real equivocation is a human
    incident-response procedure. #300 decided the POC-scoped response
    procedure (an investigation playbook to rule out a benign cause first,
    plus a mirror-recovery mechanism), implemented in #316 — see
    `docs/maintainers/equivocation-response.md` for the operator-facing
    runbook. `avalon_chain::mirror::resolve_equivocation` records which
    root hash was determined legitimate (`equivocation_findings.resolved_at`/
    `resolved_root_hash`), which is what `backfill_network`'s gate below
    actually checks (`unresolved_equivocations`, not every finding ever
    recorded). `avalon_chain::mirror::discard_mirrored_entries_from` is the
    recovery half, for a mirror that may have already advanced past the
    fork point on the losing branch. Full key-rotation procedure and real
    alerting/paging remain open, tracked separately as #315.
  - **Multi-peer by design.** `AVALON_MIRROR_PEERS` accepts more than one
    URL, and every configured peer is actually watched every tick, not
    just the first one that answers — one unreachable/misbehaving peer
    never stops the others. Peers are then grouped by `network_id` and
    handed to `mirror_watcher::backfill_network`, which (a) refuses to
    backfill a network past any `tree_size` with an already-recorded,
    unresolved equivocation finding (the same detection-not-resolution
    posture as above — no automatic pick between two disagreeing STHs),
    and (b) picks the tree head **this tick's peers most widely agree
    on** — a majority-corroboration gate, not "whichever peer answered
    first" — before trusting it for backfill.
  - **Backfill** (`mirror_watcher::backfill`) fetches entries since this
    network's last verified `seq` (`avalon_chain::mirror::mirrored_progress`,
    keyed on `network_id` — not per peer, deliberately: any configured
    peer of the same network is an interchangeable source of the same
    independently-verified content) from `GET /ledger/entries`, and for
    each one, fetches and checks its inclusion proof
    (`merkle::verify_inclusion_proof`) against the corroborated STH before
    ever storing it in the new `mirrored_entries` table
    (`UNIQUE (network_id, seq)`, also not per peer). Every request
    round-robins across the peers that corroborated the chosen tree head —
    if one is unreachable mid-backfill, the next candidate is tried before
    giving up for that tick, so backfill fails over rather than stalling
    on a single flaky peer. A restart is not a special case — backfill
    just resumes from `mirrored_progress`'s last-verified `seq` on the
    next tick, same as catching up from any other gap. Aborts the whole
    pass (retried next tick) the moment every candidate peer's response
    for a given entry fails to verify, rather than accepting a partial,
    unverifiable backfill.
  - **Feeds the local Indexer too, not just `mirrored_entries` (#313).**
    Once an entry's inclusion is verified, `backfill` decodes it into the
    same `ProtocolEvent` shape `outbox::drain_once` builds from
    `protocol_outbox` rows, and applies it to this node's own
    `PostgresIndexer` (`apply_in_tx`) in the same transaction as the
    `mirrored_entries` insert. This is what lets a node with no Settlement
    role of its own (`AVALON_SETTLEMENT_REMOTE_URL` set — see
    [`nodes.md`](./nodes.md)) still serve real, independently-verified
    reads from its own local Postgres, rather than either sharing another
    node's database or trusting unverified peer content.
- **`POST /ledger/submit`, node-to-node write endpoint (#313).** The one
  write route in `settlement.rs` — every other endpoint in this section is
  a public, unauthenticated read, deliberately (see "all four are public
  reads" above); this one accepts an `EventBatch` and runs it
  through the exact same `chain.commit` call the local outbox worker
  already runs for itself, so it is privileged rather than public.
  `outbox::run_worker` calls it instead of committing locally whenever
  `AVALON_SETTLEMENT_REMOTE_URL` is configured — see
  [`nodes.md`](./nodes.md) for the full read/write design. **Auth: a
  shared-secret bearer token**, `AVALON_SETTLEMENT_SUBMIT_KEY`, checked
  against the `Authorization: Bearer <key>` header
  (`crates/server/src/settlement.rs::submit_ledger_batch`) — chosen because
  nothing more specific for trusted node-to-node calls already existed in
  this codebase to reuse (every other authenticated route checks a
  user's session or an integrator's registered credential, neither of which
  fits "one operator's own two nodes talking to each other"). A node with
  no `AVALON_SETTLEMENT_SUBMIT_KEY` configured refuses every request to
  this endpoint outright, rather than leaving it open.
- **Genesis and network identity (#173).** A singleton `chain_genesis` table
  commits the ledger to a `network_id` (e.g. `avalon-mainnet-1` vs.
  `avalon-dev-<name>`, from the required `AVALON_NETWORK_ID` env var) —
  written once, on `avalon-server`'s first boot against an empty database,
  and never updated after. `network_id` alone carries no cryptographic
  weight — see [`network-trust-anchors.md`](./network-trust-anchors.md) for
  how a client pins it to the settlement operator's actual key. Every boot
  after that verifies the running
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
- **`verify`'s two independent checks (#210), both must pass.** (1) A
  hash-chain check: replay a batch's own entries' stored content across the
  sequential hash chain, entry by entry (`prev_hash == expected_prev`
  always checked, content checked whenever the payload is present) —
  catches content tampering that left `entry_hash` stale. This is
  deliberately per-entry, not all-or-nothing: a hot-tier node may have
  pruned some entries' payloads (not evidence of tampering), but that never
  widens into skipping the check for the batch's other, still-complete
  entries — a batch with one pruned entry and one genuinely tampered entry
  must still fail. (2) A Merkle check: recompute the RFC 6962 MTH fresh
  from every `entry_hash` up to this batch's `last_seq` and compare against
  `commitment.proof` — catches structural tampering (an `entry_hash` value
  itself, entry ordering, a deleted row) anywhere up to this batch, which a
  batch-local chain replay alone can't see. Built entirely from
  `entry_hash`, never `payload`, so pruning never affects it either way.
- **Node-tiered durable history retention, implemented (#208, closing
  #180's decision).** Settlement commitment and durable event storage are
  separate retention problems: everything above this bullet — the hash
  chain, the Merkle tree, Signed Tree Heads — is untouched by retention
  tier. What tiers is `ledger_entries.payload` specifically, now nullable
  (`0027_ledger_payload_retention`). `crates/chain/src/retention.rs` is the
  config (`AVALON_RETENTION_TIER`, `AVALON_RETENTION_HOT_WINDOW_DAYS`,
  `AVALON_RETENTION_PRUNING_ENABLED`) and cutoff logic;
  `PostgresSettlementProvider::prune_payloads_older_than` is the one-column
  `UPDATE` that actually prunes, driven by `crates/server/src/retention.rs`'s
  hourly worker when enabled, or manually via `avalon prune-ledger
  [--dry-run]`. `verify` and `list_entries` both treat a pruned entry's
  missing payload as "not independently re-checkable from here," never as
  tamper evidence — `verify`'s Merkle check (built entirely from
  `entry_hash`, never `payload`) still runs and still must pass for a
  batch to verify, even once every one of its entries' payloads is pruned;
  only the redundant hash-chain content-replay check (which does need
  payload) is skipped for a batch with any pruned entry. See
  [`nodes.md`](./nodes.md)'s "Settlement retention tiers" section for the
  full tier/config design and its milestone-1 single-database honesty
  note, and this section's next bullet for the checkpoint half of #180.
- **Settlement-state checkpoint (#180/#208).** The latest `SignedTreeHead`
  already is the checkpoint #180 asked for — `(tree_size, root_hash)` at a
  known, signed height, produced every batch commit with no new storage
  needed. `PostgresSettlementProvider::checkpoint()` is a purely-named
  alias over the existing `latest_signed_tree_head`, making that intent
  explicit at the call site. The indexer/projection read-model snapshot
  half of #180's ask is **not** built — `crates/indexer` has real
  projections (issue #42) but no rebuild-speed or snapshot-format work yet
  (issue #43), so that stays an open follow-up rather than something
  invented to close out this ticket.

