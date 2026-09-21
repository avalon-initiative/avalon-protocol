# Self-Hosting a Private Instance

**Running your own instance of the code is fully supported.** Doing so does
not join the Avalon network — it forks it. Those are two different things,
and this document exists so that distinction is never fuzzy for a studio
deciding which one they actually want.

## The three things people mean by "self-host"

1. **Mirroring the public network.** An operator runs `avalon-server`
   (Settlement/Indexer/Gateway, any combination) against the *same*
   `network_id` as the reference deployment, syncing the same public,
   verifiable log. This is what [`./nodes.md`](./nodes.md) means by "multiple
   operators" and what [`../stakeholders/Proposal.md` §20](../../../stakeholders/Proposal.md#20-self-hosting-and-decentralization)
   describes — more infrastructure serving the one network, the Certificate
   Transparency pattern, not federation. **Read-only** with respect to
   mainnet's history: a mirror never has its own write authority over any
   part of the shared state.
2. **Running as a shard operator.** [#527](https://github.com/LunarVagabond/avalon-protocol/issues/527)
   (decided) shards settlement *authority* per-integrator rather than
   leaving one operator as mainnet's sole committer. A shard operator runs
   `avalon-server` configured with its own shard's settlement signing key,
   under the **same** `network_id` (shared genesis) as every other shard,
   and is cryptographically part of mainnet via the cross-shard root
   ([`./settlement.md`](./settlement.md)'s "Cross-shard commitment"
   section, #529) — but is the real, authoritative write target for its
   own shard's events, not a read-only copy of someone else's. This is the
   category this document previously had no answer for: "run your own
   settlement node that's actually part of mainnet." See
   [Invariants](#shard-operator-invariants) below for exactly how this
   stays distinct from both mirroring and forking.
3. **Running a private, disconnected instance.** An organization runs the
   exact same code, but roots it under its own `network_id` — a studio
   spinning up Avalon for its own internal games, an air-gapped environment,
   a staging/dev copy that should never touch production data. Most of the
   rest of this document is about this case.

All three are "self-hosting" in casual conversation, and only the third one
forks the network — the other two mirror or extend the same shared
`network_id`. The line that actually matters is not "does this operator run
their own infrastructure" (all three do) but "does this deployment share
mainnet's `network_id` and genesis, or root its own": mirror and shard
operator both share it; a private instance does not.

**#1 and #2 are not mutually exclusive per node.** Framed above as three
choices a studio picks between because that's the decision an operator
actually faces — but mechanically, "mirror" and "shard operator" are a
per-*shard* property, not a per-*node* one: a single running `avalon-server`
can be the shard-operator authority for one shard and a mirror of a
completely different shard at the same time, since authored history
(`ledger_entries`) and mirrored history (`mirrored_entries`) are kept in
separate storage by construction. See
[`./nodes.md`](./nodes.md)'s "A node's three configuration axes are
independent" section for the full breakdown (this is one of those three
axes) — `avalon-peer` in this environment runs exactly this dual role for
real, live-verified.

### Shard operator invariants

- **Genesis, not config, is the boundary.** A shard operator's `chain_genesis`
  row matches mainnet's `network_id` exactly — the same boot-time check
  ([`crates/chain/src/postgres.rs`](../../../../crates/chain/src/postgres.rs))
  that makes a private fork's history cryptographically incapable of being
  mistaken for mainnet's (see below) is what makes a shard operator
  provably part of mainnet, not a separate mechanism. There is no
  config flag that turns a shard into a fork or vice versa without
  actually changing `network_id`, which is deliberately a one-time,
  write-once value.
- **Write authority is scoped, never network-wide.** A shard operator's
  settlement key signs STHs for its own shard's log only — #527's
  per-integrator sharding, not a second copy of mainnet's single log. It
  never commits on another shard's behalf, and the cross-shard root is
  computed the same deterministic way regardless of which node computes
  it (see #529's "no designated aggregator" invariant) — a shard operator
  gains write authority over its own shard, never influence over anyone
  else's.
- **A shard is never a fork by omission.** A shard operator that stops
  publishing its STH, or that a node can't currently reach, shows up as a
  named gap in that node's `CrossShardRoot` (`partial: true`, per #529) —
  never as a silent, undetectable divergence that could be confused with
  an intentional fork. The distinction between "this shard is temporarily
  unreachable" and "this deployment deliberately forked" stays legible
  from the outside, by construction, the same way this document's mirror/
  fork boundary already is (see below).
- **A `shard_id` alone proves nothing — the same standing rule this
  document already applies to `network_id`.** See
  [`./network-trust-anchors.md`](./network-trust-anchors.md)'s "Per-shard
  trust anchors" section (#543) for how a client verifies a shard
  operator's key is genuinely authorized for that shard, reusing issuer-key
  registration rather than a second trust mechanism.

### Managed hosting for a shard operator without their own infrastructure (#531)

Not every shard-authoritative integrator wants to run `avalon-server`
themselves. A managed host — first-party or third-party — can run the
storage/batching/Merkle-computation/STH-production infrastructure for an
integrator's shard while the integrator keeps its own settlement signing
key exactly as if it were self-hosting. This is deliberately a different
mechanism from [#313's existing `POST /ledger/submit`](./settlement.md)
node-to-node forwarding: that endpoint has the *receiving* node sign with
its *own* local settlement key (`crates/server/src/settlement.rs::submit_ledger_batch`
calls `state.chain.commit`, which signs with whatever key that process
holds) — correct when a remote node genuinely owns full settlement
authority, wrong here, since a managed host must never hold the
integrator's signing key at all.

**Two-phase remote signing, so the key never leaves the integrator's
control:**

1. `POST /ledger/prepare-batch` (on the managed host) — the integrator
   submits its pending events. The host performs storage, batching, and
   Merkle computation over its own accumulated shard log exactly as
   `chain.commit` does today, but stops short of signing: it returns the
   unsigned candidate `SignedTreeHead` fields (`tree_size`, `root_hash`,
   `network_id`, `timestamp`) plus a `batch_id`, not yet written as
   authoritative.
2. The integrator signs that returned digest **locally**, with its own
   settlement signing key — the same key it would use if self-hosting,
   never transmitted to the host.
3. `POST /ledger/finalize-batch` — the integrator posts back `{batch_id,
   signature}`. The host verifies the signature against the shard's own
   registered verify key (the same `AVALON_SETTLEMENT_VERIFY_KEY`-style
   check `settlement.rs` already uses for mirror verification, no new
   crypto scheme) before persisting the batch's `ledger_entries` and STH as
   valid. A batch that's `prepare`d but never validly `finalize`d simply
   stays visibly stuck in that state — never silently accepted as
   authoritative — mirroring how a stuck outbox row is visible, not lost
   (`crates/server/src/outbox.rs`).

**Trust boundary, enforced mechanically, not by policy.** A managed host
is structurally incapable of producing a valid STH for a shard whose
signing key it never holds — `finalize-batch` requires a real signature
verifiable against that shard's own registered key, the same guarantee
[`./issuers.md`](./issuers.md)'s key-custody model already gives issuer
keys. Hosting is infrastructure only; it carries no elevated trust over
the integrator's own history, matching this document's shard-operator
invariants above.

**Implemented (#531).** `POST /ledger/prepare-batch` returns a read-only
preview (`avalon_chain::sth::PreparedTreeHead`) that never touches
`ledger_entries`/`ledger_batches`/the shared Merkle cache — a preview
that mutated shared state or burned real `seq` values on every call,
whether or not the caller ever finalizes, would be a real cost with no
corresponding commit. `POST /ledger/finalize-batch` **does not trust the
earlier preview as authoritative** — `PostgresSettlementProvider::finalize`
recomputes the batch's insertion and resulting tree size/root fresh,
inside a real transaction, and only *then* verifies the caller's
signature against those freshly-computed values (never against whatever
the caller claims). A stale finalize (the tip moved since the preview) or
a forged/wrong signature both fail that one check, and the transaction
rolls back on either — a rejected finalize never burns `seq` or leaves
partial state. Live-verified end to end, including a real cross-key
rejection case (`crates/chain/tests/managed_hosting.rs`,
`crates/server/tests/managed_hosting.rs`): a valid signature commits the
batch and stamps the STH with the caller's own `signing_key_id`; an
invalid signature, and a finalize against a preview made stale by an
intervening commit, are both cleanly rejected with nothing persisted.

**Interim, single-key-per-node** (`AVALON_MANAGED_HOSTING_VERIFY_KEY`): a
managed-hosting node is presumed dedicated to exactly one hosted
integrator's shard for now — this env var names that one integrator's
public settlement key directly, rather than resolving it from #543's
real per-shard trust-anchor mechanism (issuer-key registration), which
isn't built yet. Both endpoints refuse every request when it's unset,
matching `submit_ledger_batch`'s own "exists but accepts nothing until
configured" posture.

**A managed-hosting operator is exactly who wants a genuinely
Settlement-only deployment (#664).** This section's whole premise is that
the host never holds the integrator's signing key — but before #664, the
host's own `avalon-server` process still had every Gateway-facing module
(WebAuthn, sessions, guilds, presence, friends...) mounted and reachable
regardless, purely because that's what the combined binary always ran.
None of that surface has any legitimate caller on a process whose only
job is `prepare-batch`/`finalize-batch` plus ordinary `/ledger/*` reads —
it's attack surface with no corresponding feature for this operator.
`AVALON_NODE_ROLES=settlement` (see [`./nodes.md`](./nodes.md)'s "Today in
the repo" entry for #664) is how that operator now gets a process that
genuinely never mounts any of it, not just one that happens not to be
called.

### Censorship recourse: switching hosts, or self-hosting, without losing the shard (#544)

Two-phase signing means a managed host can never forge an integrator's
history — but it can still simply refuse to `prepare-batch`, or go dark,
withholding service from that one integrator (see
[`./security-model.md`](./security-model.md)'s "Explicit limitations" for
this named as a real, undismissed liveness gap, not something this design
claims to fully solve). The recourse is that an integrator is never
cryptographically bound to one host:

- **The shard's identity is the integrator's own key, never the host's.**
  #543's per-shard trust anchors authorize a shard via the integrator's
  `issuer.key_added(purpose: shard_settlement)` event, not via anything
  the hosting operator holds or controls. A host stalling an integrator
  has no power to reassign, freeze, or otherwise touch that authorization
  — it can only decline to help.
- **Switching hosts (or to self-hosting) has zero continuity break.** The
  shard's own log is a normal, mirrorable log like any other (#529/#530)
  — a new host, or a self-hosted node, syncs the shard's existing history
  from any mirror the same way any node bootstraps into serving content
  it didn't originally author, then resumes `prepare-batch`/`finalize-batch`
  (or local `chain.commit`, for self-hosting) from where the stalled host
  left off. Nothing about the shard's identity or history changes; only
  which infrastructure is currently serving it does.
- **Automatic multi-host failover (issue #564).** An integrator wanting
  live failover, rather than a manual switch after noticing a stall, can
  use `avalon_sdk::managed_hosting::ManagedHostingClient` with more than
  one candidate host. It fans `prepare-batch` out to every candidate
  concurrently and uses whichever answers first — safe because `prepare`
  is read-only (see `PostgresSettlementProvider::prepare`'s own doc
  comment) — then `finalize-batch`es against that *one* host only, never
  more than one per batch. A concurrent *finalize* fan-out would fork the
  shard's log (two independent hosts each hold their own separate ledger,
  so committing the same batch to two of them independently appends it
  after two different tips); prepare-race/finalize-once avoids that by
  construction, not by locking. If the chosen host's finalize itself
  fails, the client falls back to prepare-racing the remaining
  candidates rather than retrying blindly. `PostgresSettlementProvider::commit`/
  `finalize` are also idempotent on a replayed `batch_id`, so a retried
  finalize against the *same* host (a dropped response) returns the
  existing commitment instead of erroring — that only de-duplicates
  retries against one host, not across two, which is why the client's own
  single-finalize discipline is what actually prevents a fork.
- **Manual switch-readiness tooling (implemented).** The design above
  describes a manual switch as always cryptographically safe, but until
  now nothing concretely answered the actual operational question an
  integrator faces mid-switch: has the candidate new host actually caught
  up, and does it agree with the old one, *before* traffic is cut over?
  `avalon check-switch-readiness <old-host-url> <new-host-url> [--shard-id
  <id>] [--verify-key <hex>]` answers exactly that, read-only, against two
  real running nodes: fetches the old host's latest STH for the shard,
  fetches the new host's STH at that *exact* `tree_size`, and reports
  `READY` (root hashes agree), `NOT_READY` (new host hasn't backfilled
  that far yet), `MISMATCH` (both claim a value at the same `tree_size`
  but disagree — a serious finding, never treated as "close enough"), or
  `UNKNOWN` (old host unreachable — plausible, it may be the very host
  that's stalling — falls back to reporting the new host's own state
  alone rather than a false `READY`). `--verify-key` additionally checks
  both STHs' signatures against the shard's registered key, the same
  check `inspect-ledger` runs locally — without it, a `READY` verdict
  only means the two hosts agree with *each other*, not that either is
  honest. Never mutates anything; what to do with a `READY` verdict
  (updating `AVALON_SETTLEMENT_REMOTE_URLS`/self-hosting config) stays a
  manual, deliberate operator action.

## Why this is safe to offer, and exactly where the line is

Every ledger entry is hashed with its `network_id` folded in ahead of the
rest of the content ([`./settlement.md`](./settlement.md),
[#173](https://github.com/LunarVagabond/avalon-protocol/issues/173)), and
`network_id` is committed once, at genesis, into a dedicated
`chain_genesis` row — every later boot reads that stored value back and
refuses to start if the configured `AVALON_NETWORK_ID` doesn't match it
exactly (`crates/chain/src/postgres.rs`). Two consequences fall out of that
by construction, not by policy:

- A private instance's history is **cryptographically incapable** of being
  mistaken for, merged into, or replayed against the public network's log —
  the hashes don't collide, by design, the same way two Certificate
  Transparency logs with different tree identities don't collide.
- There is no accidental path from "internal dev deployment" to "silently
  part of the public network" — the mismatch check fails fast and loud,
  before the process even binds a listener.

That's what makes this safe to explicitly support rather than something to
merely tolerate: a private deployment can never leak into or corrupt the
network everyone else is relying on, and there's no migration path that
quietly blurs the two.

## What a private instance gets, and what it gives up

A private instance is the real thing — identity, guilds, achievements,
attestations, the same trust model — running for one organization's own
integrators. What it does **not** get is the reason most of this exists:

- Its identities, guilds, and achievements are meaningless outside itself —
  nothing on the public network can see, verify, or recognize anything a
  private instance issues, and vice versa. [`./trust-model.md`](./trust-model.md)'s
  "authentic, valid, recognized" distinctions all still apply, but recognition
  can only ever happen among integrators pointed at the same `network_id`.
- None of the cross-integrator discovery, shared communities, or "your friends are
  already here" effects in
  [`../developers/WhyBuildOnAvalon.md`](../../rust-sdk/for-developers/WhyBuildOnAvalon.md)
  apply — those come from the network, not the code.
- It's a fork in substance, even though it's zero effort in practice (set a
  different `AVALON_NETWORK_ID` and stand up your own Postgres). Nothing
  merges the two later without a real migration; there is no "upgrade path"
  from private to public that isn't just "start recognizing the public
  network's attestations going forward."

## Where this actually makes sense

Legitimate reasons to run a private instance instead of pointing at the
public network:

- Local dev, CI, and staging environments — every `avalon-dev-<name>`
  `network_id` in `.env.example` is already exactly this
- An internal tooling/QA environment that should never touch real identity data
- A studio piloting Avalon internally before committing to the public network
- An organization with real regulatory, contractual, or air-gap constraints
  that make joining any shared network a non-starter

None of these are the goal of the project. Avalon exists to be the shared
identity and social layer between *independent* integrators — the value described
in [`../developers/WhyBuildOnAvalon.md`](../../rust-sdk/for-developers/WhyBuildOnAvalon.md)
compounds with the size of the *public* network, not with how many private
forks of the code exist. A studio is always welcome to run their own
instance; we'd just rather have their games on the actual network, where
their users' identities and communities are worth something beyond that
one studio's games. If a private deployment starts asking "how do we
eventually connect this to the real network" — the honest answer today is
"there isn't one yet beyond re-registering integrators and re-issuing attestations
on the public network" — that's a real gap, not a hidden feature; see
[Decisions and tickets](#decisions-and-tickets) below.

## Today in the repo

- `AVALON_NETWORK_ID` is a required env var
  (`crates/server/src/main.rs`); there is no default, and no way to boot
  without one.
- Genesis commit-and-check lives in
  `PostgresSettlementProvider::connect` (`crates/chain/src/postgres.rs`),
  covered by `connect_fails_fast_on_network_id_mismatch` and
  `connect_succeeds_when_network_id_matches_existing_genesis` in
  `crates/chain/tests/settlement.rs`.
- `network_id` is hashed into every ledger entry
  (`hash_entry`/`hash_event`) and into every Signed Tree Head's signing
  message (`crates/chain/src/sth.rs`), so it's load-bearing for integrity,
  not just a boot-time label.
- `avalon inspect-ledger`/`-full` (`crates/cli`) print whichever
  `network_id` the connected database is actually rooted in, so an operator
  can always confirm which instance they're looking at.
- There is exactly one reference deployment target today
  (`avalon-mainnet-1`, per `.env.example`'s convention) and no tooling yet
  that assumes multiple simultaneous `network_id`s in one process — a
  private instance today means a wholly separate deployment, not a mode flag.
- Whether a private instance or a mirror of the public network, any instance
  reachable beyond localhost must run behind TLS termination — see
  [`../hosters/deployment.md`](../for-hosters/deployment.md).
- Standing up a single instance no longer requires a Rust toolchain —
  `make stack-up` (issue #289, root `Dockerfile` + `docker-compose.yml`)
  builds and runs `avalon-server` + Postgres from a fresh checkout,
  generating a fresh `AVALON_SETTLEMENT_SIGNING_KEY`/`AVALON_NETWORK_ID`
  into `.env` on first run rather than requiring either to be hand-set
  first. See [`../hosters/hosting-quickstart.md`](../for-hosters/hosting-quickstart.md).
- Plain `.env` storage for `AVALON_SETTLEMENT_SIGNING_KEY` is a decided
  floor (issue #352), not an oversight — a dedicated secrets backend was
  weighed and rejected as the default because it would fork `make
  stack-up`'s single zero-manual-steps bring-up path per OS/platform.
  `make stack-up` now `chmod`s the generated `.env` to `600` (#354), and
  [`../maintainers/key-rotation.md`](../for-maintainers/key-rotation.md)
  documents the rotation procedure that decision left open (#315).
- Structured logging (issue #265): `avalon-server` logs via `tracing`, with
  an HTTP request span (method/path/status/latency) per request via
  `tower-http`'s `TraceLayer`. `RUST_LOG` (standard env-filter syntax)
  controls level without a rebuild; `AVALON_LOG_FORMAT=json` switches from
  the human-readable dev format to one JSON object per line, the shape a
  self-hoster's log aggregator (Grafana/Loki, etc.) expects. See
  [`../maintainers/local-development.md`](../../../maintainers/local-development.md#logs-and-run-state).
- **Runtime log-level control (issue #658)**: `RUST_LOG` above only ever
  sets the *starting* filter — bumping verbosity to chase a live issue
  used to mean a restart, losing in-memory state (DHT peer table, host-
  metrics warm-up, in-flight requests). `GET`/`POST /nodes/log-level`
  (`crate::admin`) reads/swaps the active filter live via
  `tracing_subscriber::reload`, taking effect on the very next log call —
  `POST` body is `{"filter": "<RUST_LOG-style string>"}`, an unparseable
  one is a clean `400` that leaves the current filter untouched. **Gated on
  a separate shared secret, `AVALON_ADMIN_TOKEN`** — deliberately not the
  existing `AVALON_SETTLEMENT_SUBMIT_KEY` (below), since that key's trust
  domain is "this operator's own nodes talking to each other" and may be
  shared across two nodes one hoster runs, which would let one of *their
  own* nodes flip the other's log level; admin control is narrower, only
  whoever holds this one process's own credential. Unset (the default),
  both endpoints refuse every request. Live-verified: a token-less or
  wrong-token request 401s, a correct one reads the active filter, a
  `POST` bumping `tower_http` to `debug` produces real `DEBUG`-level
  `tower_http::trace` lines on the very next request with no restart, and
  an intentionally invalid filter string 400s without changing what's
  active.
- Issue #564's prepare-race/finalize-once multi-host client:
  `avalon_sdk::managed_hosting::ManagedHostingClient` (`crates/sdk/src/managed_hosting.rs`),
  live-verified over real HTTP against two independently-running
  `avalon-server` processes (`crates/sdk/tests/managed_hosting_live.rs`,
  `--ignored`). `PostgresSettlementProvider::commit`/`finalize`'s
  replayed-`batch_id` idempotency (`crates/chain/src/postgres.rs`) is the
  companion server-side piece, covered by
  `finalize_is_idempotent_on_a_replayed_batch_id`
  (`crates/chain/tests/managed_hosting.rs`, `--ignored`).

## Decisions and tickets

- [#173](https://github.com/LunarVagabond/avalon-protocol/issues/173)
  `network_id` genesis commitment and mismatch-is-fatal design
- [#70](https://github.com/LunarVagabond/avalon-protocol/issues/70),
  [#79](https://github.com/LunarVagabond/avalon-protocol/issues/79),
  [ADR #186](https://github.com/LunarVagabond/avalon-protocol/issues/186) —
  why mirroring the *same* network is the decentralization story, not
  federation between different ones
- No open ticket yet on whether/how a private instance's history could ever
  be selectively re-issued onto the public network (see the last paragraph
  above) — worth filing as a `decision` issue if a studio actually asks for
  it, rather than speculating here
