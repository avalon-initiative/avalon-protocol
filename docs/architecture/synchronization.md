# Synchronization: Offline and Deferred Protocol Participation

**Avalon connectivity should be eventually available, not continuously
required.** A game stays playable, and able to keep recording what its
player is doing, when Avalon is unreachable — a genuinely offline
single-player game, a handheld with no signal, bad rural internet, or a
temporary Avalon outage. The SDK owns this complexity, not each game
developer.

**The other half is equally load-bearing: an offline client-generated event
must never carry the same trust guarantees as an authoritative online
issuer, unless the protocol explicitly says otherwise.** A player who can
edit a local pending-events file must never turn that into an accepted,
authoritative claim. Every offline-capable operation needs an explicit
answer to what a receiving party actually gets to believe about it — the
convenience half of this document is inert without the trust half.

## Why this is a cross-cutting capability, not a per-feature convenience

The same problem — record something locally, submit it later, don't
manufacture trust while doing it — shows up for achievements, guild
membership requests, friend requests, and direct messages
([communication.md](./communication.md)) alike. Solving it once, as an SDK
capability every consumer reuses, is the whole point; a feature that invents
its own offline queue is a bug, not a feature.

```text
Avalon SDK
├── Identity
├── Social
├── Achievements
├── Communication
└── Synchronization
      ├── Local durable journal
      ├── Deferred submission (retry, backoff, idempotency, ordering)
      ├── Reconciliation
      └── Sync status (pending / submitted / rejected)
```

The developer experience this is meant to produce:

```rust
avalon.achievements().issue("dragon_slayer").await?;
```

— one call, online or offline. The game developer never writes the branch
themselves:

```rust
// exactly what the SDK exists to make unnecessary
if internet_available {
    send_to_avalon();
} else {
    serialize_event();
    put_in_local_database();
    retry_later();
}
```

## Not every operation can defer

Some things need a live round trip by their nature: an achievement needs
*someone* to eventually issue it, but guild membership is shared state that
can't be unilaterally granted client-side, and voice needs a live connection,
full stop. The classification is decided — see
[#109](https://github.com/LunarVagabond/avalon-protocol/issues/109) (closed) —
and this table is now the authoritative version, referenced by every
operation ticket rather than each re-deriving its own answer:

| Operation | Offline? | Behavior |
|---|---|---|
| Read cached profile / roster / friends list | Yes | Serve from local cache; label staleness |
| Game-local achievement earned | Yes, queued | Recorded locally; see below for what it's worth before submission |
| Chat / conversation message | Yes, queued | Stored `pending`; UI shows it, never silent loss |
| Friend request / accept | Deferred, queued | Submitted on reconnect; may be rejected on reconciliation |
| Guild join / leave request | Deferred, queued | A *request*, not a grant — nothing is unilaterally true until confirmed |
| Presence update | No | Meaningless without a live connection by definition |
| Voice | No | Requires a live connection, full stop |
| Asset transfer (future) | No / special | Contested shared state; not safe to defer without its own design |
| Permission grant/revoke | No | Authorization changes must be authoritative immediately |

## The hard problem: what does an offline claim actually prove

An offline single-player game has no live server to attest anything while
disconnected — but its *client* could still record "the player defeated the
dragon" locally. Treating that queued claim as equivalent to a normal
`Issuer::Game` attestation the moment it's submitted is a real hole: it
implies the game's live issuer signing key exists somewhere the client can
reach, and a key reachable from a shipped client is a key that can be
extracted — a solved, common reverse-engineering problem. Whoever extracts
it can forge arbitrary achievements for any player, retroactively, and
rotating the key afterward doesn't invalidate what already verifies against
its old validity window.

This is the trust model ([trust-model.md](./trust-model.md), ADR #76 —
authentic, valid, and recognized are separate) applied to a new axis: *how*
a claim came to exist changes what a receiving game should be willing to
believe, even when the signature checks out. [#112](https://github.com/LunarVagabond/avalon-protocol/issues/112)
(closed) decided **deferred requests, not deferred attestations**: the
offline period queues an unsigned local record of intent, not a valid
attestation — no issuer key exists client-side at all. On reconnect the
request goes to the game's *own* server, which independently decides
whether to believe its own client's record and only then issues a normal,
fully-authoritative attestation through the existing online path.

This adds no new key material anywhere and doesn't expand what a
compromised client can forge — "authoritative attestation" keeps meaning
exactly what it already means everywhere else in the protocol; the offline
period is a queuing convenience for *requests*, never a new class of
cryptographic claim. The alternative considered and rejected as the
default — a distinct, lower-trust issuer variant with a separate,
explicitly lower-privilege key a game embeds client-side — is a real option
some games may still want later (a genuinely offline-only game with no
server of its own at all), but it's a strictly bigger, riskier addition and
isn't where this defaults.

## Mechanism

- **Local durable journal** ([#110](https://github.com/LunarVagabond/avalon-protocol/issues/110),
  done) — a crash-safe local store for offline-capable operations, behind a
  `SyncJournal` trait so each SDK (Rust, C#, future) backs it with whatever's
  appropriate. Every entry gets a client-generated, stable id (`EntryId`,
  a `Uuid`) — the idempotency key everything downstream depends on. Shipped
  in `avalon-sdk` (`crates/sdk/src/sync_journal.rs`) with a dependency-light
  reference implementation, `FileJournal`, backed by an append-only,
  `fsync`-per-write JSON-lines file rather than embedded SQLite — see that
  module's doc comment for the full trade-off. Nothing calls `append()` from
  `AvalonClient`/`Session` yet; that wiring, plus draining/submitting what's
  recorded, is #111.
- **Deferred submission** ([#111](https://github.com/LunarVagabond/avalon-protocol/issues/111)) —
  drains the journal on reconnect, submits in per-kind order, retries with
  capped exponential backoff, dedupes on the entry id so a retried
  submission never double-applies.
- **Reconciliation** (same ticket) — a submission can come back rejected
  because the world moved on while it was pending (a guild disbanded, a
  target blocked the sender, an achievement definition retired). Surfaced
  as an explicit `Rejected { reason }` outcome, never silently dropped and
  never retried forever.
- **Sync status** ([#113](https://github.com/LunarVagabond/avalon-protocol/issues/113)) —
  a read-only, local-only API (`pending_count`, `status_of(entry_id)`, a
  subscription for transitions) so a game can render "🕓 Pending" for a
  queued message the same way it would for anything else, per
  [communication.md](./communication.md)'s direct-message example.

## What this is not

- Not a general-purpose offline mode for gameplay. Hot gameplay state (HP,
  position, combat) was never Avalon's concern online or offline; this is
  strictly about the durable, interoperable facts Avalon already cares about.
- Not a way around the trust model. Every offline-capable operation still
  answers authentic/valid/recognized — deferring *when* something reaches
  Avalon never changes *what* it's allowed to claim about itself.
- Not built yet, except the journal itself. Deferred submission,
  reconciliation, and sync status are still open tickets; nothing in this
  document past the local journal describes shipped behavior.

## Today in the repo

- The local durable journal (#110) exists: `SyncJournal` trait and
  `FileJournal` reference implementation in `crates/sdk/src/sync_journal.rs`
  — `append`/`pending`/`mark_submitted`/`mark_failed`, crash-recovery tested
  by dropping a `FileJournal` mid-session (no clean-shutdown method exists
  to call) and reopening it from the same path. Nothing else in this
  document is built yet: no deferred submission, no reconciliation, no sync
  status API in any SDK.
- `crates/sdk/src/lib.rs`'s `AvalonClient` methods either succeed against a
  live server or fail outright — no code path appends to the journal yet,
  so there is no *end-to-end* offline path today, even though the local
  storage half now exists.
- `crates/server/src/outbox.rs` (issue #71, done) is the *server-side*
  analog of the same pattern — durable local recording before a slower,
  retriable downstream step — applied to the settlement ledger rather than
  the SDK. Worth reading as a reference for the shape, not reusable code:
  the outbox lives in Postgres on a machine that's always online; this
  epic's journal lives on a client that, by definition, sometimes isn't.

## Decisions and tickets

- [#108](https://github.com/LunarVagabond/avalon-protocol/issues/108) —
  Epic: Offline & Deferred Protocol Synchronization.
- [#109](https://github.com/LunarVagabond/avalon-protocol/issues/109) —
  decision: operation capability classification (closed/decided — table
  above is authoritative).
- [#110](https://github.com/LunarVagabond/avalon-protocol/issues/110) —
  SDK local event journal + persistence abstraction.
- [#111](https://github.com/LunarVagabond/avalon-protocol/issues/111) —
  deferred submission engine: retry, idempotency, ordering, reconciliation.
- [#112](https://github.com/LunarVagabond/avalon-protocol/issues/112) —
  decision: offline trust model, client-recorded vs server-attested
  (closed/decided — deferred requests, not deferred attestations).
- [#113](https://github.com/LunarVagabond/avalon-protocol/issues/113) —
  SDK sync status API.
- [#76](https://github.com/LunarVagabond/avalon-protocol/issues/76) — ADR:
  attestation trust model, the framework #112 extends.
- [#82](https://github.com/LunarVagabond/avalon-protocol/issues/82) — event
  catalogue; a new provenance field may be needed depending on #112's
  outcome.
- [communication.md](./communication.md), [sdk.md](./sdk.md) — the first
  consumer, and the "capabilities, not infrastructure" principle this
  epic serves.
