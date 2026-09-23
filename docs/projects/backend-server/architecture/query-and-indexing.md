# Query and Indexing

The query layer is the second vertical: fast, conventional reads over a
projection of durable history. **Every query database is a projection, never a
source of truth.** **Fast reads never walk settlement data directly.** **The
indexer consumes protocol semantics; it never redefines them.**

## What it is

A read model. PostgreSQL is a reasonable first implementation and the one in
use, but the property that matters is not the engine — it is that the whole
thing can be dropped and regenerated from the log. The indexer is what makes
"durable history is canonical" true in practice rather than on paper.

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

## Events with their own source of truth

Not every real, current event kind gets an indexer projection, and that is by
design, not a gap. A protocol event's data has exactly one of two homes:

- **Indexer-projected**: the handler that emits the event writes only to
  `ledger_entries` (via the outbox); the event's data reaches Postgres only
  when `PostgresIndexer::apply_in_tx` later dispatches it into one of the
  tables under `Self::KNOWN`'s `PROJECTION_TABLES`
  (`profiles`, `indexer_friendships`, `indexer_guild_members`,
  `indexer_attestations`, and so on). This is the only kind of table
  `rebuild_from_scratch` truncates and replays — the only kind
  "always rebuildable from durable events" actually promises.
- **Server-owned, its own direct table**: the handler writes its own table
  synchronously, in the same transaction as the `outbox::enqueue` call that
  makes the event durable — the event is emitted for other consumers
  (mirrors, future subscribers, audit), but this repo's own reads of that
  data never go through the indexer at all. Rebuilding the indexer's
  projection tables from scratch has nothing to do with this table's
  correctness, because the indexer never wrote to it in the first place.

`PostgresIndexer::apply_in_tx`'s dispatch `match` reflects this explicitly: a
kind in the second category gets its own no-op arm with a comment pointing
here, rather than falling into the generic "unrecognized kind" branch (which
exists for a kind this build genuinely doesn't know about yet — an old
indexer surviving a new event kind added elsewhere in the protocol). Every
server-owned kind has been investigated and given this explicit no-op arm,
so a rebuild's log output distinguishes "known, intentionally server-owned"
from "genuinely unrecognized" rather than treating both the same way.

Server-owned kinds and their table:

| Event kind(s) | Table | Handler |
| --- | --- | --- |
| `game.registered` | `integrators` (+ `issuer_keys`, `integrator_requested_capabilities`) | `crates/server/src/integrators.rs::register_integrator` |
| `issuer.key_added` / `issuer.key_revoked` | `issuer_keys` | `crates/server/src/integrators.rs::add_issuer_key`/`revoke_issuer_key` |
| `guild.channel_created` | `guild_channels` | `crates/server/src/channels.rs::create_channel` |
| `permission.granted` / `permission.revoked` | `permission_grants` | `crates/server/src/connections.rs` |
| `achievement.defined` / `.definition_updated` / `.definition_retired` | `achievement_definitions` | `crates/server/src/achievements.rs::create_definition`/`update_definition` |
| `milestone.defined` / `.definition_updated` / `.definition_retired` | `achievement_definitions` | same as above — milestones and achievements share one claim-vocabulary table |
| `milestone.issued` / `milestone.revoked` | `achievement_attestations` | `crates/server/src/achievements.rs` |
| `identity.recovery_configured` | `recovery_guardians` / `recovery_guardian_settings` | `crates/server/src/recovery.rs::set_guardians` |
| `identity.recovery_requested` | `recovery_requests` | `crates/server/src/recovery.rs::finish_request` |
| `identity.recovery_approved` | `recovery_approvals` (+ `recovery_requests` status update) | `crates/server/src/recovery.rs::approve_request` |
| `identity.recovery_cancelled` | `recovery_requests` | `crates/server/src/recovery.rs::cancel_request` |
| `identity.recovered` | `identity_keys` (+ `recovery_requests` status update) | `crates/server/src/recovery.rs::finalize_request` |
| `issuer.registered` | `issuer_network_registrations` | `crates/server/src/issuer_registration.rs::register_issuer` — a separate table from `issuer_keys`; see that module's own doc comment for why registration and keys are split |
| `guild.updated` | `guilds` | `crates/server/src/guilds.rs::update_guild` |
| `guild.role_defined` | `guild_roles` | `crates/server/src/guilds.rs::create_role` and `::update_role` — both emit this kind |
| `guild.role_deleted` | `guild_roles` | `crates/server/src/guilds.rs::delete_role` |
| `guild.owner_transferred` | `guilds` | `crates/server/src/guilds.rs::transfer_ownership` |
| `guild.game_associated` | `guild_integrator_associations` | `crates/server/src/guilds.rs::associate_integrator` |
| `guild.favorite_games_updated` | `guild_favorite_games` | `crates/server/src/guilds.rs::set_favorite_games` |
| `guild.channel_renamed` | `guild_channels` | `crates/server/src/channels.rs::update_channel` |
| `guild.channel_archived` | `guild_channels` | `crates/server/src/channels.rs::archive_channel` |

`achievement.issued`/`achievement.revoked` are the one asymmetric case worth
calling out: they get *both* a direct table write (`achievement_attestations`,
server-owned) *and* an indexer projection (`indexer_attestations`, feeding the
`achievements_issued`/`achievements_revoked`/`unique_achievement_holders`
registry metrics — [`./registry.md`](./registry.md)). `milestone.issued`/
`milestone.revoked` deliberately do not get the second half: those registry
metrics are achievement-scoped by name and by design, not milestone-inclusive
— see `crates/indexer/src/registry.rs`. Whether milestones should eventually
feed their own or a combined metric is a real open question, not decided here.

## Settlement vs querying

Kept apart on purpose: `chain` commits and verifies; `indexer` reads and
aggregates. They may share a database today. They must never share a
definition of truth. A server reading directly from `ledger_entries` to
answer a profile lookup is exactly the thing this separation exists to
prevent.

## Scaling the read side

Indexers are the natural place to partition and specialize: an operator can run
indexer + gateway nodes without settlement ([`./nodes.md`](./nodes.md)), shard
projections by domain, or maintain a registry-only projection. Rebuild time is
a first-class scaling dimension — see
[`./scalability.md`](./scalability.md).

## Current implementation

- `crates/indexer/src/postgres.rs` — `PostgresIndexer`, the real
  `Indexer`. `apply_in_tx` does the actual dispatch (per-event dedup
  via `indexer_applied_events`, then a `match` on `event.kind` into
  `crates/indexer/src/projections/`); `Indexer::apply` is a thin wrapper that
  opens its own transaction around the same call. `crates/indexer/src/projections/`
  has one module per read model — `profiles`, `friendships`, `guild_rosters`,
  `attestations`, `integrator_bindings`, `integrator_schemas` — each a
  pure `decode` (event → typed write, unit-tested without Postgres) plus an
  `apply` (typed write → an upsert keyed by its natural key).
- `profiles` is retargeted onto its *existing* table
  (`crates/server/db/migrations/0001_identity_and_auth`,
  `.../0005_friend_handles`): `handlers::register_finish`/`update_profile`
  don't `INSERT`/`UPDATE` it themselves — they call
  `state.indexer.apply_in_tx` with the same `identity.created`/
  `profile.updated` event they enqueue into the outbox, in the same
  transaction, so identity/profile/outbox rows commit or roll back
  together. Every *read* of it in `handlers.rs` goes through the indexer
  too: `avalon_indexer::projections::profiles::fetch`/`fetch_many`/
  `is_display_name_taken` (each generic over `sqlx::PgExecutor`, so a
  caller can pass the shared pool or an open transaction) back `me`,
  `get_identity_profile`, `list_profiles`, and the advisory taken-name
  pre-checks in `register_start`/`update_profile` (the real, atomic
  enforcement is `profiles_display_name_lower_idx` at `apply`'s own write,
  not these pre-checks). `crates/server/tests/read_model_boundary.rs`
  guards this with a plain source-text check (no live infra needed) that
  `handlers.rs` never queries `profiles` or `ledger_entries` directly. One
  deliberate, documented exception: `handlers::my_history` still reads
  `ledger_entries` via `PostgresSettlementProvider::list_entries_for_issuer_prefix`,
  since it serves the raw historical log itself, not current state — see
  that function's own doc comment.
- `friendships` and `guild_rosters` are the live read/write path for the
  social graph and guild rosters, not just a rebuild target: `friends.rs`/
  `guilds.rs` write and read exclusively through
  `avalon_indexer::projections::{friendships,guild_rosters}`'s functions
  (`are_friends`/`partners_of`/`friends_of_any`;
  `role_index_for`/`is_member`/`roster`/`memberships_for`/`member_count`/
  `any_member_with_role`/`mutual_members`), generic over `sqlx::PgExecutor`
  same as `profiles`, with writes going through `state.indexer.apply_in_tx`
  in the same transaction as the outbox enqueue. `guild_rosters::decode`
  also recognizes `guild.created`, folding the owner's implicit membership
  (`role_index` 0) into the same upsert every other member row gets, so the
  projection has no gap between a guild's owner and its other members.
  `crates/server/tests/read_model_boundary.rs` extends its source-text
  guard to both modules. `guild_roles` (role definitions/permissions) and
  `bindings`/`permission_grants` (`connections.rs`) stay outside this
  projection on purpose — they're genuinely server-owned data, not "two
  writers of one projection."
- **Rebuild-from-events is real**: `avalon rebuild-index`
  (`avalon_server::rebuild::rebuild_index_from_ledger`) truncates every
  table in `avalon_indexer::postgres::PROJECTION_TABLES` and replays all of
  `ledger_entries` back through `PostgresIndexer::rebuild_from_scratch`, in
  one transaction. `crates/server/tests/rebuild_from_events.rs` is the live
  proof — see [`./disaster-recovery.md`](./disaster-recovery.md).
- **Registry projections exist**: `integrator_bindings` (`indexer_integrator_bindings`)
  backs the `players`/`total players ever` metrics; `integrator_schemas`
  backs schema-version discovery — see [`./registry.md`](./registry.md). No
  history projections yet.
- **A Gateway-only deployment can run without a local `PostgresIndexer`**,
  building on the operator-internal RPC/`RemoteIndexer` protocol (see
  [`nodes.md`](./nodes.md)'s Indexer extraction section for the
  startup-mode/env-var wiring). Every call site above that would otherwise
  hold a bare `PostgresIndexer` instead holds
  `crates/server/src/state.rs`'s `IndexerHandle` (`Local(PostgresIndexer)`
  or `Remote(RemoteIndexer)`), and every write that used to be a single
  `apply_in_tx` call is now two: `apply_in_tx`, inside the same Postgres
  transaction as the app-data write as before, followed by
  `apply_after_commit`, called once that transaction has actually
  committed. For `Local`, this is exactly the old behavior: `apply_in_tx`
  does the real, atomic write, `apply_after_commit` is a no-op — a
  combined-binary deployment (still the default) is byte-for-byte
  unaffected. For `Remote`, it's the reverse, and it's a genuine,
  deliberately accepted eventual-consistency tradeoff, not full atomicity:
  `apply_in_tx` is a no-op (a `RemoteIndexer` talks over HTTP — it cannot
  join a caller's local Postgres transaction), and `apply_after_commit`
  makes the real remote `apply` call afterward. Between the local
  transaction committing and that call completing — or for however long a
  transient failure there takes to be corrected — the app-data write is
  already durable and is the source of truth, but the remote indexer's
  projection can lag behind it, or miss it outright if the call fails. A
  failure there is logged loudly (`tracing::error!`) but does not fail the
  request itself, since the app-data write genuinely already succeeded; the
  rebuild-from-events guarantee above is what lets the projection catch up
  later. A background retry/backfill mechanism specifically for this gap is
  a possible future improvement, not built yet.
</content>
</invoke>
