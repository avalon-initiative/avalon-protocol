# Disaster Recovery

The requirement is simple to state and strict to meet. **Assume every
PostgreSQL database in the Avalon network disappears. Everything Avalon
promises to preserve durably must have a canonical, reconstructable
representation in durable protocol history.** That does not mean every row
lives in settlement; it means every promised fact does, and every table is
rebuildable from those facts
([#75](https://github.com/LunarVagabond/avalon-protocol/issues/75)).

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
2. Verify the chain      — rehash every entry, check every link (and, once
                           signed, every signature and tree head)
3. Replay                — Indexer::rebuild(events) from genesis, in order
4. Compare               — projections match a known-good snapshot, or, with no
                           snapshot, satisfy invariants (every identity has a
                           creation event, every attestation an issuer, ...)
```

Step 3 is why `Indexer::apply` must be idempotent
([`./query-and-indexing.md`](./query-and-indexing.md)). Step 4 is the test
[#43](https://github.com/LunarVagabond/avalon-protocol/issues/43) exists to
automate: drop the projection tables, replay, diff.

Rebuild time is a scaling dimension in its own right
([`./scalability.md`](./scalability.md)): a log that takes a week to replay is
technically recoverable and practically not.

## Scenario J — PostgreSQL disappears

Can durable projections be reconstructed? Yes, and as of
[#43](https://github.com/LunarVagabond/avalon-protocol/issues/43) it's
actually proven, not just designed: `avalon rebuild-index` (or
`avalon_server::rebuild::rebuild_index_from_ledger`, the function it calls)
truncates every table in `avalon_indexer::postgres::PROJECTION_TABLES` and
replays the entire `ledger_entries` history back through
`PostgresIndexer::rebuild_from_scratch` — one transaction, so a failure
partway through leaves the pre-rebuild database untouched rather than a
half-rebuilt one. `crates/server/tests/rebuild_from_events.rs` is the live
test: it drives real registration/profile-update/friend/guild actions
through the actual HTTP ceremony, rebuilds, and diffs. Two things that
test's own module doc is explicit about, given today's wiring:

- `profiles` is compared byte-for-byte against its real pre-rebuild state,
  since `handlers::register_finish`/`update_profile` already keep it live
  via the indexer (`identity.created`/`profile.updated`).
- `indexer_friendships`/`indexer_guild_members` are, as of
  [#506](https://github.com/LunarVagabond/avalon-protocol/issues/506),
  also kept live (`friends.rs`/`guilds.rs` no longer write the old
  `friendships`/`guild_members` tables at all) — the test still asserts
  the rebuilt rows exactly match what the actions taken should produce
  (rather than folding them into the `profiles` byte-for-byte comparison),
  plus that replaying the same history twice
  (`replay_onto_rebuilt_index_is_noop`) reproduces an identical snapshot.

The gap this rebuild test originally surfaced — a guild's owner was never
separately event-sourced into `indexer_guild_members`, since `guild.created`
wasn't a kind `projections::guild_rosters::decode` recognized — is closed:
`decode` now folds the owner's implicit membership (`role_index` 0) into
the same upsert every other member row gets, so the projection has no gap
between a guild's owner and its other members.

`crates/cli/tests/milestone_1_walkthrough.rs` (issue #65, automating #64's
walkthrough) is the same proof from a different angle: instead of a
narrow, indexer-focused before/after diff, it drives the full milestone-1
vertical slice (identity, friends, guild, channel, achievement) through
real HTTP/SDK calls, shells out to `avalon rebuild-index`, and re-asserts
the exact reads a Hub page would make — proving the rebuild guarantee
holds for what a real client actually depends on, not just for the
projection tables in isolation.

## What breaks today

- ~~A `profile.updated` event for an identity with no `identity.created`
  in the ledger crashed a full rebuild.~~ Fixed (found live-testing #43's
  rebuild against this repo's own accumulated dev ledger, filed and
  corrected as [#505](https://github.com/LunarVagabond/avalon-protocol/issues/505)):
  `profiles::apply`'s `INSERT ... ON CONFLICT` had no existing row to fall
  back to for such an event and manufactured one with an empty-string
  `display_name` (its `COALESCE(..., '')` placeholder for the NOT NULL
  column) — a second orphaned identity then collided with the first on
  the unique display-name index (`profiles_display_name_discriminator_idx`
  at the time; superseded by `profiles_display_name_lower_idx`, issue
  #510). Can't happen in
  production (#71's outbox guarantees `identity.created` is durable before
  any request that could produce a `profile.updated` is even possible); it
  surfaced only because ~28 of this repo's own live test files seed
  identities directly via SQL (bypassing registration) as a documented
  shortcut, and some of those still exercise `PATCH /me`, leaving a real
  orphaned `profile.updated` on the shared dev ledger. Fixed defensively
  regardless: the INSERT is now an `INSERT ... SELECT ... WHERE` that skips
  (with a logged warning) a partial update with no `display_name` and no
  existing row, rather than fabricating a broken placeholder.
- ~~Profile updates emit no event.~~ Fixed
  ([#86](https://github.com/LunarVagabond/avalon-protocol/issues/86)):
  `update_profile` emits `profile.updated` through the outbox in the same
  transaction as the `profiles` row, and `identity.created` carries the
  initial display name (issue #510: the handle itself, globally unique),
  so `profiles` is fully reconstructable from history. The rebuild test
  that proves it is still
  [#43](https://github.com/LunarVagabond/avalon-protocol/issues/43).
- ~~Identity creation is not atomic with its ledger entry.~~ Fixed
  ([#71](https://github.com/LunarVagabond/avalon-protocol/issues/71)):
  `register_finish` inserts the `identity.created` event into
  `protocol_outbox` in the same transaction as the identity/profile/key rows
  (`crates/server/src/outbox.rs`), so a crash before the ledger ever sees it
  cannot orphan an identity — the event is already durable, just not yet
  published. The pattern is generic; every future write path reuses the same
  table and worker rather than needing its own fix.
- **`identity_keys`' passkey counter/backup-state is not reconstructable from
  history.** A rebuild from genesis would restore the identity, its
  `identity_signing_keys` public key, and every event it signed — but the
  WebAuthn passkey's operational clone-detection counter lives only in
  Postgres. Accepted trade-off: a rebuilt system's counter resets to a fresh
  baseline, a minor security regression (clone detection re-learns its
  baseline) rather than a correctness one.
- **The ledger and the app tables share one database.** Losing Postgres today
  loses the log too. Mirrors and an export format
  ([#40](https://github.com/LunarVagabond/avalon-protocol/issues/40)) are what
  make "obtain the log" in step 1 possible.
- ~~No rebuild test exists.~~ Fixed (#43): `avalon rebuild-index` and
  `crates/server/tests/rebuild_from_events.rs` (see above) now drive a real
  rebuild-from-genesis end to end, on a live database, and diff the result.
- **Step 1 ("obtain the log") is not full-replay-only for the commitment
  layer any more.** #208 gives a settlement-state checkpoint — the latest
  `SignedTreeHead` (`PostgresSettlementProvider::checkpoint`, see
  [`settlement.md`](./settlement.md)) — that lets a node trust a signed
  `(tree_size, root_hash)` instead of rehashing every entry from genesis
  for the commitment structure specifically. This does not yet extend to
  step 3/4 (replaying into indexer projections): no projection-level
  snapshot format exists, so rebuilding the actual read model is still
  full-replay-from-genesis, tracked under #43.

## Today in the repo

- `crates/indexer/src/lib.rs` — `rebuild` default implementation (replay every
  event through `apply`); `crates/indexer/src/postgres.rs::PostgresIndexer`
  is the concrete implementation (see above), but nothing yet exercises
  `rebuild` itself against it.
- `crates/chain/src/postgres.rs` — `list_entries` re-verifies every entry's
  content hash and link; this is step 2 for the current unsigned chain.
- `crates/server/src/migrate.rs` — `reset` drops and recreates the schema for
  local development; it is not a recovery tool.

## Decisions and tickets

- #75 durable history is canonical
- #43 rebuild proof (the `profiles` replay that #86's events now make possible)
- #71 atomic commit — done for the identity path via the outbox pattern
- #40 export / mirror format;
  [#82](https://github.com/LunarVagabond/avalon-protocol/issues/82) the event
  catalogue every rebuild decodes
- [#78](https://github.com/LunarVagabond/avalon-protocol/issues/78) presence is
  out of rebuild scope by design
