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
- aggregating network statistics for the game registry
  ([`./game-registry.md`](./game-registry.md))
- exposing data to the server, and through it to the SDK and the Hub

What it serves: profiles, friends lists, guild rosters and history, achievement
lists and attestation status, game discovery, game and issuer statistics,
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

- `crates/indexer/src/lib.rs` — the `Indexer` trait and `IndexError`, nothing
  else. No implementation.
- `avalon-server` reads `identities` and `profiles` directly in
  `crates/server/src/handlers.rs` (`me`, `update_profile`). These tables are
  written by request handlers, not by an indexer applying events — which is
  exactly the shape #75 forbids for promised-durable state and #44 replaces.
- No registry projections, no history projections, no rebuild test.

## Decisions and tickets

- Epic [#41](https://github.com/LunarVagabond/avalon-protocol/issues/41)
  Query/Index Layer
- [#42](https://github.com/LunarVagabond/avalon-protocol/issues/42) read models
  for profiles / friends / guild rosters / achievements
- [#43](https://github.com/LunarVagabond/avalon-protocol/issues/43)
  rebuild-from-events + idempotency guarantee
- #44 server reads go through the indexer, not the ledger
- [#89](https://github.com/LunarVagabond/avalon-protocol/issues/89) game
  registry read model
- #75 durable history is canonical;
  [#82](https://github.com/LunarVagabond/avalon-protocol/issues/82) the event
  catalogue the indexer decodes
