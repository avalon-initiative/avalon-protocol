# Query and Indexing

The query layer is the second vertical: fast, conventional reads over a
projection of durable history. **Every query database is a projection, never a
source of truth.** **Fast reads never walk settlement data directly.** **The
indexer consumes protocol semantics; it never redefines them.**

## What it is

A read model. PostgreSQL is a reasonable first implementation, and milestone 1
uses it, but the property that matters is not the engine — it is that the whole
thing can be dropped and regenerated from the log
([#75](https://github.com/LunarVagabond/avalon-protocol/issues/75)). The
indexer is what makes "durable history is canonical" true in practice rather
than on paper.

Responsibilities:

- consuming durable protocol events, in order, idempotently
- building queryable projections (current state) and historical views
- maintaining the Postgres read models `avalon-server` serves
- aggregating network statistics for the integrator registry
  ([`./registry.md`](./registry.md))
- exposing data to the server, and through it to the SDK and the Hub

What it serves: profiles, friends lists, guild rosters and history, achievement
lists and attestation status, integrator discovery, integrator and issuer statistics,
recognition relationships, aggregate network analytics, and point-in-time
historical queries.

## The trait

`crates/indexer/src/lib.rs`:

```rust
#[async_trait]
pub trait Indexer: Send + Sync {
    /// Must be idempotent: replaying the same event twice must not corrupt the index.
    async fn apply(&self, event: &ProtocolEvent) -> Result<(), IndexError>;

    /// Drop and rebuild from scratch by replaying every durable event.
    async fn rebuild(&self, events: &[ProtocolEvent]) -> Result<(), IndexError> { ... }
}
```

Two properties are load-bearing:

- **Idempotent `apply`.** Redelivery, retries, and rebuilds all replay events.
  An index that double-counts on replay is not derived state.
- **`rebuild` from genesis.** This is the proof that the index is a projection.
  It is also the disaster-recovery path
  ([`./disaster-recovery.md`](./disaster-recovery.md)).

## Not a second source of truth

The indexer does not:

- decide what an event means — semantics live in `protocol`
  ([`./protocol-events.md`](./protocol-events.md))
- accept writes that did not come from an event
- compute a trust judgment; it derives facts with published definitions
  ([`./trust-model.md`](./trust-model.md))
- store presence or any other ephemeral state in rebuild scope
  ([`./presence.md`](./presence.md))

A "current status" column (an attestation's revoked flag, a member's current
role) is a cache of the latest relevant event. History remains in the log and,
where useful, in a history projection alongside the current-state one.

## Settlement vs querying

Kept apart on purpose ([#69](https://github.com/LunarVagabond/avalon-protocol/issues/69)):
`chain` commits and verifies; `indexer` reads and aggregates. They may share a
database in milestone 1. They must never share a definition of truth. A server
reading directly from `ledger_entries` to answer a profile lookup is the thing
[#44](https://github.com/LunarVagabond/avalon-protocol/issues/44) exists to
prevent.

## Scaling the read side

Indexers are the natural place to partition and specialize: an operator can run
indexer + gateway nodes without settlement ([`./nodes.md`](./nodes.md)), shard
projections by domain, or maintain a registry-only projection. Rebuild time is
a first-class scaling dimension — see
[`./scalability.md`](./scalability.md).

## Today in the repo

- `crates/indexer/src/postgres.rs` — `PostgresIndexer`, the first real
  `Indexer` (#42). `apply_in_tx` does the actual dispatch (per-event dedup
  via `indexer_applied_events`, then a `match` on `event.kind` into
  `crates/indexer/src/projections/`); `Indexer::apply` is a thin wrapper that
  opens its own transaction around the same call. `crates/indexer/src/projections/`
  has one module per read model — `profiles`, `friendships`, `guild_rosters`,
  `attestations`, `integrator_bindings` (#261), `integrator_schemas` (#255) — each a
  pure `decode` (event → typed write, unit-tested without Postgres) plus an
  `apply` (typed write → an upsert keyed by its natural key).
- `profiles` is the one projection retargeted onto its *existing* table
  (`crates/server/db/migrations/0001_identity_and_auth`,
  `.../0005_friend_handles`): `handlers::register_finish`/`update_profile`
  no longer `INSERT`/`UPDATE` it themselves — they call
  `state.indexer.apply_in_tx` with the same `identity.created`/
  `profile.updated` event they enqueue into the outbox, in the same
  transaction, so identity/profile/outbox rows commit or roll back
  together. As of #44, every *read* of it in `handlers.rs` is gone too:
  `avalon_indexer::projections::profiles::fetch`/`fetch_many`/
  `is_display_name_taken` (each generic over `sqlx::PgExecutor`, so a
  caller can pass the shared pool or an open transaction) back `me`,
  `get_identity_profile`, `list_profiles`, and the advisory taken-name
  pre-checks in `register_start`/`update_profile` (issue #510 — the real,
  atomic enforcement is `profiles_display_name_lower_idx` at `apply`'s own
  write, not these pre-checks).
  `crates/server/tests/read_model_boundary.rs` guards this with a plain
  source-text check (no live infra needed) that `handlers.rs` never
  queries `profiles` or `ledger_entries` directly. One deliberate,
  documented exception: `handlers::my_history` still reads
  `ledger_entries` via `PostgresSettlementProvider::list_entries_for_issuer_prefix`,
  since it serves the raw historical log itself, not current state — see
  that function's own doc comment.
- `friendships` and `guild_rosters` are, as of #506, the live read/write
  path for the social graph and guild rosters, not just a rebuild target:
  `friends.rs`/`guilds.rs` no longer write or read the old `friendships`/
  `guild_members` tables at all (`crates/server/db/migrations/0004_social_graph`,
  `.../0009_guild_membership` — both now dead, kept only until a follow-up
  migration drops them once nothing references them, per that ticket's own
  design). Writes go through `state.indexer.apply_in_tx` with the same
  `friend.accepted`/`.removed` and `guild.member_added`/`.member_removed`/
  `.role_changed` events enqueued into the outbox, in the same transaction.
  Reads go through `avalon_indexer::projections::{friendships,guild_rosters}`'s
  functions (`are_friends`/`partners_of`/`friends_of_any`;
  `role_index_for`/`is_member`/`roster`/`memberships_for`/`member_count`/
  `any_member_with_role`/`mutual_members`), generic over `sqlx::PgExecutor`
  same as `profiles`. `guild_rosters::decode` also recognizes `guild.created`
  now, folding the owner's implicit membership (`role_index` 0) into the
  same upsert every other member row gets — closing the gap #43's rebuild
  test surfaced, where a guild's owner never had a durable roster row of
  their own. `crates/server/tests/read_model_boundary.rs` extends its
  source-text guard to both modules. `guild_roles` (role definitions/
  permissions) and `bindings`/`permission_grants` (`connections.rs`) stay
  outside this projection on purpose — they're genuinely server-owned data,
  not "two writers of one projection." `attestations` has no producer yet
  (achievement issuing, Epic #30, isn't built) — its `decode`/`apply` are
  proven by fixture events only, ready for #30 to start emitting into.
- **Rebuild-from-events is real** (#43): `avalon rebuild-index`
  (`avalon_server::rebuild::rebuild_index_from_ledger`) truncates every
  table in `avalon_indexer::postgres::PROJECTION_TABLES` and replays all of
  `ledger_entries` back through `PostgresIndexer::rebuild_from_scratch`, in
  one transaction. `crates/server/tests/rebuild_from_events.rs` is the live
  proof — see [`./disaster-recovery.md`](./disaster-recovery.md).
- **Registry projections now exist**: `integrator_bindings` (`indexer_integrator_bindings`,
  migration `0039_indexer_game_bindings`) backs the `players`/`total players
  ever` metrics (#261); `integrator_schemas` (#255) backs schema-version discovery
  — see [`./registry.md`](./registry.md). No history projections
  yet.

## Decisions and tickets

- Epic [#41](https://github.com/LunarVagabond/avalon-protocol/issues/41)
  Query/Index Layer
- [#42](https://github.com/LunarVagabond/avalon-protocol/issues/42) read models
  for profiles / friends / guild rosters / achievements
- [#43](https://github.com/LunarVagabond/avalon-protocol/issues/43)
  rebuild-from-events + idempotency guarantee
- #44 server reads go through the indexer, not the ledger — the `profiles`
  slice (`me`/`get_identity_profile`/`list_profiles`/taken-name checks) is
  closed
- [#506](https://github.com/LunarVagabond/avalon-protocol/issues/506)
  retargeted `friends.rs`/`guilds.rs` onto the indexer's `friendships`/
  `guild_rosters` projections, the same shape #44 established for
  `profiles` — closed
- [#89](https://github.com/LunarVagabond/avalon-protocol/issues/89) integrator
  registry read model
- #75 durable history is canonical;
  [#82](https://github.com/LunarVagabond/avalon-protocol/issues/82) the event
  catalogue the indexer decodes
