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
full stop. The classification is its own decision, not an assumption baked
into this document — see [#109](https://github.com/LunarVagabond/avalon-protocol/issues/109),
whose table is the authoritative version of:

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
is the open decision, with two real alternatives:

1. **A distinct, lower-trust issuer variant for client-recorded claims** —
   a separate, explicitly lower-privilege key a game chooses to embed
   client-side, never the same key that signs server-attested claims,
   recognized independently by consuming games' trust policies. Real offline
   attestations exist the moment they're recorded, at the cost of every
   participating game shipping a client-embedded key with a permanently
   bounded blast radius.
2. **Deferred requests, not deferred attestations** — the offline period
   queues an unsigned local record of intent, not a valid attestation. No
   issuer key exists client-side at all. On reconnect the request goes to
   the game's *own* server, which independently decides whether to believe
   its own client's record and only then issues a normal, fully-authoritative
   attestation through the existing online path.

Not yet decided which wins — see #112 for the full reasoning and a stated
(not yet ratified) recommendation.

## Mechanism

- **Local durable journal** ([#110](https://github.com/LunarVagabond/avalon-protocol/issues/110)) —
  a crash-safe local store for offline-capable operations, behind a trait so
  each SDK (Rust, C#, future) backs it with whatever's appropriate. Every
  entry gets a client-generated, stable id — the idempotency key everything
  downstream depends on.
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
- Not built yet. Every piece above is an open ticket; nothing in this
  document describes shipped behavior.

## Today in the repo

- No journal, no deferred submission, no sync status API exists in any SDK.
- `crates/sdk/src/lib.rs`'s `AvalonClient` methods either succeed against a
  live server or fail outright — there is no offline path today.
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
  decision: operation capability classification (open).
- [#110](https://github.com/LunarVagabond/avalon-protocol/issues/110) —
  SDK local event journal + persistence abstraction.
- [#111](https://github.com/LunarVagabond/avalon-protocol/issues/111) —
  deferred submission engine: retry, idempotency, ordering, reconciliation.
- [#112](https://github.com/LunarVagabond/avalon-protocol/issues/112) —
  decision: offline trust model, client-recorded vs server-attested (open).
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
