# Disaster Recovery

The requirement is simple to state and strict to meet. **Assume every
PostgreSQL database in the Avalon network disappears. Everything Avalon
promises to preserve durably must have a canonical, reconstructable
representation in durable protocol history.** That does not mean every row
lives in settlement; it means every promised fact does, and every table is
rebuildable from those facts.

## What must be reconstructable

| Promised durable — must rebuild from history | Source events |
|---|---|
| identity existence and the metadata Avalon promises (display name, avatar) | `identity.created`, `profile.updated` |
| integrator bindings (which identities participate in which integrators) | `game.binding_*` |
| integrator and issuer registrations | `game.registered`, `issuer.registered` |
| issuer key lifecycle and status | `issuer.key_*`, `issuer.suspended` / `.reinstated` / `.revoked` |
| achievement definitions, issuance, revocation, supersession | `achievement.*`, `attestation.superseded` |
| guild existence, membership history, roles, integrator associations | `guild.*` |
| integrator event results (tournaments, seasonal events, ...) | `game_event.result_issued` |
| recognition relationships | `recognition.published` |
| ownership and provenance (later phase) | asset events, when they exist |

| Not promised — need not rebuild | Where it lives |
|---|---|
| typing indicators, ephemeral chat delivery state | realtime / memory |
| connection state, heartbeats, current presence | realtime ([`./presence.md`](./presence.md)) |
| sessions and login credentials | server-local tables, never the log |
| caches, derived aggregates | recomputed by the indexer |

Anything in the second table that is lost means, at worst, identities appear
offline and have to log in again. Anything in the first table that cannot be
rebuilt is a broken promise.

## The rebuild procedure

```text
1. Obtain the log        — from the operator's settlement store or any mirror
2. Verify the chain      — rehash every entry, check every link, every
                           signature and tree head
3. Replay                — Indexer::rebuild(events) from genesis, in order
4. Compare               — projections match a known-good snapshot, or, with no
                           snapshot, satisfy invariants (every identity has a
                           creation event, every attestation an issuer, ...)
```

Step 3 is why `Indexer::apply` must be idempotent
([`./query-and-indexing.md`](./query-and-indexing.md)). Step 4 is proven, not just
designed, by the automated rebuild-and-diff test described below.

Rebuild time is a scaling dimension in its own right
([`./scalability.md`](./scalability.md)): a log that takes a week to replay is
technically recoverable and practically not.

## Scenario J — PostgreSQL disappears

Can durable projections be reconstructed? Yes. `avalon rebuild-index` (or
`avalon_server::rebuild::rebuild_index_from_ledger`, the function it calls)
truncates every table in `avalon_indexer::postgres::PROJECTION_TABLES` and
replays the entire `ledger_entries` history back through
`PostgresIndexer::rebuild_from_scratch` — one transaction, so a failure
partway through leaves the pre-rebuild database untouched rather than a
half-rebuilt one. `crates/server/tests/rebuild_from_events.rs` is the live
test: it drives real registration/profile-update/friend/guild actions
through the actual HTTP ceremony, rebuilds, and diffs.

- `profiles` is compared byte-for-byte against its real pre-rebuild state,
  since `handlers::register_finish`/`update_profile` already keep it live
  via the indexer (`identity.created`/`profile.updated`).
- `indexer_friendships`/`indexer_guild_members` are also kept live
  (`friends.rs`/`guilds.rs` write directly to those projections, not the
  older `friendships`/`guild_members` tables) — the test asserts the
  rebuilt rows exactly match what the actions taken should produce, plus
  that replaying the same history twice
  (`replay_onto_rebuilt_index_is_noop`) reproduces an identical snapshot.

A guild's owner is folded into `indexer_guild_members` on `guild.created` the same
way any other member row is upserted (`role_index` 0), so the projection has no gap
between a guild's owner and its other members.

`crates/cli/tests/milestone_1_walkthrough.rs` is the same proof from a different
angle: instead of a narrow, indexer-focused before/after diff, it drives the full
identity/friends/guild/channel/achievement vertical slice through real HTTP/SDK
calls, shells out to `avalon rebuild-index`, and re-asserts the exact reads a Hub page
would make — proving the rebuild guarantee holds for what a real client actually
depends on, not just for the projection tables in isolation.

## Known limitations

- **`identity_keys`' passkey counter/backup-state is not reconstructable from
  history.** A rebuild from genesis restores the identity, its
  `identity_signing_keys` public key, and every event it signed — but the
  WebAuthn passkey's operational clone-detection counter lives only in
  Postgres. Accepted trade-off: a rebuilt system's counter resets to a fresh
  baseline, a minor security regression (clone detection re-learns its
  baseline) rather than a correctness one.
- **The ledger and the app tables share one database.** Losing Postgres today
  loses the log too. Mirrors and an export format are what make "obtain the
  log" in step 1 possible against something other than the same database.
- **Step 1 ("obtain the log") is not full-replay-only for the commitment
  layer.** A settlement-state checkpoint — the latest `SignedTreeHead`
  (`PostgresSettlementProvider::checkpoint`, see [`settlement.md`](./settlement.md))
  — lets a node trust a signed `(tree_size, root_hash)` instead of rehashing
  every entry from genesis for the commitment structure specifically. This
  does not yet extend to step 3/4 (replaying into indexer projections): no
  projection-level snapshot format exists, so rebuilding the actual read
  model is still full-replay-from-genesis.

## Current implementation

- `crates/indexer/src/lib.rs` — `rebuild` default implementation (replay every
  event through `apply`); `crates/indexer/src/postgres.rs::PostgresIndexer`
  is the concrete implementation exercised by the live rebuild test above.
- `crates/chain/src/postgres.rs` — `list_entries` re-verifies every entry's
  content hash and link; this is step 2.
- `crates/server/src/migrate.rs` — `reset` drops and recreates the schema for
  local development; it is not a recovery tool.

Identity creation is atomic with its ledger entry: `register_finish` inserts the
`identity.created` event into `protocol_outbox` in the same transaction as the
identity/profile/key rows, so a crash before the ledger ever sees it cannot orphan an
identity — the event is already durable, just not yet published. The pattern is
generic; every write path reuses the same table and worker. Profile updates emit
`profile.updated` through the outbox in the same transaction as the `profiles` row,
and `identity.created` carries the initial display name, so `profiles` is fully
reconstructable from history — proven by the rebuild test above.

An edge case in the rebuild path — a `profile.updated` event for an identity with no
matching `identity.created` in the ledger — is handled defensively even though it
cannot occur through the real API surface (the outbox guarantees `identity.created`
is durable before any request that could produce a `profile.updated` is even
possible): `profiles::apply`'s upsert is an `INSERT ... SELECT ... WHERE` that skips
(with a logged warning) a partial update with no `display_name` and no existing row,
rather than fabricating a broken placeholder row.
</content>
</invoke>
