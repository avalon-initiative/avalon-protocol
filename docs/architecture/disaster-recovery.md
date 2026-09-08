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
| game bindings (which identities participate in which games) | `game.binding_*` |
| game and issuer registrations | `game.registered`, `issuer.registered` |
| issuer key lifecycle and status | `issuer.key_*`, `issuer.suspended` / `.reinstated` / `.revoked` |
| achievement definitions, issuance, revocation, supersession | `achievement.*`, `attestation.superseded` |
| guild existence, membership history, roles, game associations | `guild.*` |
| game event results (tournaments, seasonal events, ...) | `game_event.result_issued` |
| recognition relationships | `recognition.published` |
| ownership and provenance (later phase) | asset events, when they exist |

| Not promised — need not rebuild | Where it lives |
|---|---|
| typing indicators, ephemeral chat delivery state | realtime / memory |
| connection state, heartbeats, current presence | realtime ([`./presence.md`](./presence.md)) |
| sessions and login credentials | server-local tables, never the log |
| caches, derived aggregates | recomputed by the indexer |

Anything in the second table that is lost means, at worst, players appear
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

Can durable projections be reconstructed? The design answer is yes. The honest
answer for the current code is: partly, and not yet provably.

## What breaks today

- ~~Profile updates emit no event.~~ Fixed
  ([#86](https://github.com/LunarVagabond/avalon-protocol/issues/86)):
  `update_profile` emits `profile.updated` through the outbox in the same
  transaction as the `profiles` row, and `identity.created` carries the
  initial display name and handle discriminator, so `profiles` is fully
  reconstructable from history. The rebuild test that proves it is still
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
- **No rebuild test exists** (#43), and no `Indexer` implementation exists to
  run one against.

## Today in the repo

- `crates/indexer/src/lib.rs` — `rebuild` default implementation (replay every
  event through `apply`); no concrete indexer.
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
