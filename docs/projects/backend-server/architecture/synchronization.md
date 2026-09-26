# Synchronization: Offline and Deferred Protocol Participation

**Avalon connectivity should be eventually available, not continuously
required.** A game stays playable, and able to keep recording what its
user is doing, when Avalon is unreachable — a genuinely offline
single-player integrator, a handheld with no signal, bad rural internet, or a
temporary Avalon outage. The SDK owns this complexity, not each
developer.

**The other half is equally load-bearing: an offline client-generated event
must never carry the same trust guarantees as an authoritative online
issuer, unless the protocol explicitly says otherwise.** A user who can
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

— one call, online or offline. The developer never writes the branch
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
full stop. This table is the authoritative classification, referenced by every
operation rather than each re-deriving its own answer:

| Operation | Offline? | Behavior |
|---|---|---|
| Read cached profile / roster / friends list | Yes | Serve from local cache; label staleness |
| Integrator-local achievement earned | Yes, queued | Recorded locally; see below for what it's worth before submission |
| Chat / conversation message | Yes, queued | Stored `pending`; UI shows it, never silent loss |
| Friend request / accept | Deferred, queued | Submitted on reconnect; may be rejected on reconciliation |
| Guild join / leave request | Deferred, queued | A *request*, not a grant — nothing is unilaterally true until confirmed |
| Presence update | No | Meaningless without a live connection by definition |
| Voice | No | Requires a live connection, full stop |
| Asset transfer (future) | No / special | Contested shared state; not safe to defer without its own design |
| Permission grant/revoke | No | Authorization changes must be authoritative immediately |

## The hard problem: what does an offline claim actually prove

An offline single-player game has no live server to attest anything while
disconnected — but its *client* could still record "the user defeated the
dragon" locally. Treating that queued claim as equivalent to a normal
`Issuer::Game` attestation the moment it's submitted is a real hole: it
implies the integrator's live issuer signing key exists somewhere the client can
reach, and a key reachable from a shipped client is a key that can be
extracted — a solved, common reverse-engineering problem. Whoever extracts
it can forge arbitrary achievements for any user, retroactively, and
rotating the key afterward doesn't invalidate what already verifies against
its old validity window.

This is the trust model ([trust-model.md](./trust-model.md) —
authentic, valid, and recognized are separate) applied to a new axis: *how*
a claim came to exist changes what a receiving integrator should be willing to
believe, even when the signature checks out. The decided answer is
**deferred requests, not deferred attestations**: the
offline period queues an unsigned local record of intent, not a valid
attestation — no issuer key exists client-side at all. On reconnect the
request goes to the integrator's *own* server, which independently decides
whether to believe its own client's record and only then issues a normal,
fully-authoritative attestation through the existing online path.

This adds no new key material anywhere and doesn't expand what a
compromised client can forge — "authoritative attestation" keeps meaning
exactly what it already means everywhere else in the protocol; the offline
period is a queuing convenience for *requests*, never a new class of
cryptographic claim. The alternative considered and rejected as the
default — a distinct, lower-trust issuer variant with a separate,
explicitly lower-privilege key an integrator embeds client-side — is a real option
some integrators may still want later (a genuinely offline-only integrator with no
server of its own at all), but it's a strictly bigger, riskier addition and
isn't where this defaults.

## Mechanism

- **Local durable journal** — a crash-safe local store for offline-capable
  operations, behind a `SyncJournal` trait so each SDK (Rust, C#, future)
  backs it with whatever's appropriate. Every entry gets a
  client-generated, stable id (`EntryId`, a `Uuid`) — the idempotency key
  everything downstream depends on. Shipped in the Rust SDK
  (`avalon-sdks` repository, `rust/src/sync_journal.rs`) with a
  dependency-light reference implementation, `FileJournal`, backed by an
  append-only, `fsync`-per-write JSON-lines file rather than embedded
  SQLite: it adds no new dependencies (no C toolchain requirement from a
  bundled SQLite), crash safety only needs one property an `fsync`'d append
  gets directly (fully landed or didn't, no transaction machinery needed),
  and recovery is "replay the file, stop at the first line that doesn't
  parse" — trivial to reason about for what is deliberately not a hot path.
  The trade-off (no concurrent-writer story, O(n) replay on open) doesn't
  matter for a single integrator client's local journal; `SyncJournal` is a
  trait specifically so another SDK can swap in SQLite or platform storage
  instead.
- **Deferred submission** — `SubmissionEngine` (Rust SDK,
  `rust/src/submission.rs`) drains a `SyncJournal`'s `pending()` entries,
  grouped by `kind` and submitted oldest-first *within* each group —
  cross-kind ordering is never guaranteed or meaningful (a queued chat
  message and a queued friend request have no ordering relationship to
  each other). Each entry is submitted through a `Transport`, an async
  trait with one method (`submit`) that classifies the result into exactly
  one of: `Applied`, `Rejected { reason }` (terminal — a 4xx meaning the
  request itself is now invalid), a retryable failure (network error or
  5xx/429), or `AuthenticationRequired`, split out from the
  retryable/terminal buckets specifically for a `401`. A retryable failure
  schedules the next attempt using `BackoffPolicy` — exponential, capped,
  tracked in memory per `EntryId` (not persisted; a process restart resets
  backoff, which is fine). `SubmissionEngine` owns no journal or transport
  itself; an integrator calls `drain(&journal, &transport)` from whatever
  loop/timer/reconnect-hook it wants, so draining never blocks gameplay.
  - **A `401` is not a terminal rejection.** `HttpTransport` classifies a
    `401` (session token missing/unknown/expired,
    `crates/server/src/handlers.rs::authenticate_token`) as
    `SubmitError::AuthenticationRequired`, not `SubmitOutcome::Rejected`: a
    stale credential is recoverable by re-authenticating, unlike a request
    that's actually invalid. The entry is left pending — not marked
    submitted — with no backoff state scheduled (retrying against the same
    expired token would just fail again; only a fresh
    `AvalonClient::authenticate()` call, i.e. a new `Session` and
    `submission_transport()`, fixes it), and reported to the caller as
    `DrainOutcome::AuthenticationRequired` so it knows to re-authenticate
    before its next `drain()`. Without this, an integrator that queued
    messages offline and later drained with an expired token would have
    every one of those messages permanently discarded as `Rejected`.
  - A `403` on `POST /conversations/{id}/messages`, by contrast, stays a
    terminal `Rejected`: that endpoint only ever returns 403 from
    `require_unblocked_participant` (not a participant, or blocked — see
    `SdkError::NotConversationParticipant`'s doc comment for why those two
    cases are indistinguishable on purpose), and this endpoint has no
    separate capability/authorization layer that a 403 could also mean
    "re-grant and retry" for. A queued message's target-conversation/block
    state isn't expected to change on its own, so retrying it automatically
    wouldn't help.
  - A `404` from this same endpoint, with `client_entry_id` always set by
    `HttpTransport`, is treated as `Applied` rather than `Rejected`: the
    server's idempotency lookup (`find_message_by_client_entry_id`) only
    returns `MessageNotFound` when this retry lost the `client_entry_id`
    conflict to an earlier attempt whose row was then pruned by normal
    message-cap behavior before this lookup ran — the message really was
    applied once, it just isn't findable by that lookup anymore. A missing
    conversation or non-participant caller both surface as the 403 above
    instead, so a 404 here has no other cause.
- **Reconciliation** — a submission can come back rejected because the
  world moved on while it was pending (a guild disbanded, a target blocked
  the sender, an achievement definition retired). Surfaced as an explicit
  `DrainOutcome::Rejected { reason }`, never silently dropped and never
  retried forever — a rejected entry is marked submitted in the journal (so
  it leaves `pending()`) the same as an applied one, the difference being
  entirely in what's reported back to the caller.
- **Sync status** — a read-only, local-only API (`pending_count`,
  `status_of(entry_id)`, a subscription for transitions) so an integrator
  can render "🕓 Pending" for a queued message the same way it would for
  anything else, per [communication.md](./communication.md)'s direct-message
  example.

## What this is not

- Not a general-purpose offline mode for gameplay. Hot gameplay state (HP,
  position, combat) was never Avalon's concern online or offline; this is
  strictly about the durable, interoperable facts Avalon already cares about.
- Not a way around the trust model. Every offline-capable operation still
  answers authentic/valid/recognized — deferring *when* something reaches
  Avalon never changes *what* it's allowed to claim about itself.
- Not a one-call-whether-online-or-offline wrapper yet. `SubmissionEngine`
  is a drain/retry/reconciliation *mechanism* an integrator drives explicitly
  (construct a `FileJournal` and a `SubmissionEngine`, call `drain(...)`);
  no `AvalonClient`/`Session` method appends to the journal on its own or
  calls `drain` for you. Wiring that convenience in — so
  `avalon.achievements().issue(...)` really is one call either way — is
  future SDK polish.
- Sync status is Rust-only so far — `SyncJournal::status`/`status_of` and
  `SubmissionEngine::subscribe` exist in the Rust SDK; the C# mirror is
  deferred, same "settle the Rust surface first" posture the C# SDK
  has taken for other additions.

## Current implementation

The Rust reference SDK now lives in a separate repository
(`avalon-sdks`, `languages/rust/` — see
[`docs/projects/sdks/rust/README.md`](../../sdks/rust/README.md)), so the
SDK-side pieces below are described by module path within that repository.

- The local durable journal — `SyncJournal` trait and `FileJournal`
  reference implementation in the Rust SDK's `src/sync_journal.rs` —
  `append`/`pending`/`all`/`entry`/`mark_submitted`/`mark_rejected`/
  `mark_failed`, crash-recovery tested by dropping a `FileJournal`
  mid-session (no clean-shutdown method exists to call) and reopening it
  from the same path.
- Sync status (Rust only) — `SyncJournal::status() -> SyncStatus {
  pending_count, oldest_pending_at, last_synced_at }` and
  `SyncJournal::status_of(id) -> EntryStatus { Pending, Submitted, Rejected
  { reason } }`, both default trait methods built on `all`/`entry`, so
  every `SyncJournal` implementation gets them for free. `mark_rejected` is
  a genuine third terminal state distinct from `mark_submitted`, so a
  rejected entry and a truly-submitted one stay distinguishable after the
  fact. No subscription mechanism on the journal itself:
  `SubmissionEngine::subscribe` (Rust SDK's `src/submission.rs`) registers
  a synchronous, in-process callback that fires exactly once per entry,
  inline within `drain`, the moment it produces a terminal
  `DrainOutcome::Applied`/`Rejected` — no polling, no background thread, no
  async channel built in (an integrator wanting async delivery sends into
  its own channel from the callback).
- The deferred submission engine — Rust SDK's `src/submission.rs`:
  `SubmissionEngine::drain` (per-kind ordering, in-memory capped
  exponential backoff via `BackoffPolicy`), the `Transport` trait, and
  `HttpTransport` — the real transport, wired for exactly one journal
  `kind` end-to-end: `CONVERSATION_MESSAGE_KIND` (`"chat.message"`),
  submitted via `Session::conversation(id).send_with_client_entry_id(...)`
  — the same conversations code path an integrator's own direct
  `Session::conversation(id).send()` call uses for `POST
  /conversations/{id}/messages`, rather than `HttpTransport` building a
  second, parallel `reqwest` request of its own. `HttpTransport` borrows
  the `Session` it was built from (`Session::submission_transport()`) so it
  can call through it. One consequence: a missing `messages.send` grant now
  fails identically (an instant local `SdkError`/`SubmitOutcome::Rejected`,
  no request sent) whichever path an integrator uses, instead of the direct
  path rejecting locally and the submission-engine path only discovering
  the same problem after a round trip through the server. Every other
  `kind` gets `SubmitError::UnsupportedKind` from `HttpTransport` — left
  pending, untouched, not a failure — since only conversation messages are
  wired so far; friend requests and guild join requests are equally
  offline-capable per the table above but remain for a future pass to
  wire, one endpoint at a time, rather than bulk-adding idempotency
  handling to every endpoint speculatively.
- Idempotency for that one endpoint: `conversation_messages.client_entry_id`
  (migration `0037_conversation_message_idempotency`) carries the journal
  entry's `EntryId` through, with a partial unique index on
  `(conversation_id, client_entry_id) WHERE client_entry_id IS NOT NULL`.
  `crates/server/src/conversations.rs::send_message` inserts with
  `ON CONFLICT ... DO NOTHING` and, on a conflict, looks the already-landed
  row up and returns it instead of erroring or duplicating — the same
  "unique constraint is what actually prevents duplicates, the query just
  discovers which case it's in" shape `create_conversation` already uses
  for `participants_key`. A message sent directly online (not through the
  journal) never sets `client_entry_id` and never dedupes against anything.
- Reconciliation: `DrainOutcome::Rejected { reason }` — see above. No
  separate reconciliation-specific code path exists; it's the same `drain`
  call classifying `Transport::submit`'s result.
- The Rust SDK's `AvalonClient` methods either succeed against a live
  server or fail outright — no code path appends to the journal on its own
  yet, so there is no *automatic* end-to-end offline path today, even
  though the local storage and submission halves both now exist and are
  tested end-to-end when driven explicitly.
- `SubmissionEngine::drain` submits `SyncJournal::pending` entries in
  `recorded_at` order **within their own `kind`** — entries of different
  kinds have no ordering relationship to each other, so they're grouped by
  `kind` first (groups visited in a fixed, deterministic order — sorted by
  kind name) and each group submitted oldest-first.
- `Transport::submit` classifies every outcome into one of five cases:
  `Applied` (marked submitted, never retried), `Rejected { reason }` (a
  terminal 4xx meaning the request itself is now invalid — marked rejected
  via `SyncJournal::mark_rejected`, a genuine terminal state), `Retryable`
  (network error or 5xx — stays pending, scheduled via `BackoffPolicy`),
  `UnsupportedKind` (an engine-internal case: only
  `CONVERSATION_MESSAGE_KIND` is wired end-to-end so far, so any other kind
  is left pending, untouched, no backoff consumed — not a failure), and
  `AuthenticationRequired` (a stale/expired session token, not an invalid
  request — left pending with no backoff, since retrying against the same
  token would fail identically forever; the caller must re-authenticate
  via a fresh `Session` before its next `drain()`).
- `HttpTransport` maps HTTP status codes to those cases carefully: a `401`
  from `crates/server/src/handlers.rs::authenticate_token` becomes
  `AuthenticationRequired`, not `Rejected`, since the credential (not the
  request) is stale. A `403` from
  `crates/server/src/conversations.rs::require_unblocked_participant`
  stays a terminal `Rejected` — this endpoint has no separate capability/
  authorization layer a 403 could also mean "re-grant and retry" for, and
  a queued message's target conversation/block state isn't expected to
  change on its own. A `404` from `send_message`'s idempotency lookup
  (`find_message_by_client_entry_id`) is mapped to `Applied` rather than
  `Rejected`: on this endpoint the only way to hit it is a retry that lost
  the `client_entry_id` conflict to an earlier attempt whose row was
  already pruned (`prune_conversation`) — the message genuinely was
  applied once, by that earlier attempt.
- Idempotency plumbing: every submission passes the journal entry's
  `EntryId` through to `Transport::submit`, which includes it in the
  request so the receiving endpoint can dedupe — see the conversation-
  message row above.
- `crates/server/src/outbox.rs` is the *server-side* analog of the same
  pattern — durable local recording before a slower, retriable downstream
  step — applied to the settlement ledger rather than the SDK. Worth
  reading as a reference for the shape, not reusable code: the outbox
  lives in Postgres on a machine that's always online; the SDK's journal
  lives on a client that, by definition, sometimes isn't.
