//! Deferred submission engine (issue #111) — the drain half of offline
//! participation described in `docs/architecture/synchronization.md`. The
//! journal (#110, `crate::sync_journal`) records intent locally; this module
//! submits it once connectivity is available, safely retryable and honest
//! about failures the integrator needs to know about.
//!
//! ## Ordering
//!
//! [`SubmissionEngine::drain`] submits [`SyncJournal::pending`] entries in
//! `recorded_at` order **within their own `kind`** — a queued chat message
//! and a queued friend request have no ordering relationship to each other,
//! so entries are grouped by `kind` first (groups visited in a fixed,
//! deterministic order — sorted by kind name — so a given drain call's
//! observable ordering is stable, though that ordering carries no
//! meaning across kinds) and each group is submitted oldest-first.
//!
//! ## Retry vs. terminal failure
//!
//! [`Transport::submit`] classifies every outcome into exactly one of three
//! buckets, matching this ticket's design:
//!
//! - `Ok(SubmitOutcome::Applied)` — the server accepted it. The entry is
//!   marked submitted in the journal and never attempted again.
//! - `Ok(SubmitOutcome::Rejected { reason })` — the request reached the
//!   server and the server made a final decision *not* to apply it (a 4xx
//!   that means the request itself is now invalid — the target conversation
//!   is gone, the caller is blocked, the payload no longer parses, etc.).
//!   This is terminal: the entry is marked rejected in the journal (issue
//!   #113's [`SyncJournal::mark_rejected`] — a genuine terminal state
//!   distinct from a real submission, not the "diagnostic record then mark
//!   submitted" workaround this module used before #113) so it's never
//!   attempted again, and the rejection is surfaced to the caller via
//!   [`DrainOutcome::Rejected`] — never silently dropped, never retried.
//! - `Err(SubmitError::Retryable(reason))` — a network error or 5xx. The
//!   entry stays pending; the engine records a failure (for diagnostics,
//!   via [`SyncJournal::mark_failed`]) and schedules the next attempt using
//!   [`BackoffPolicy`], capped and exponential.
//!
//! `Err(SubmitError::UnsupportedKind)` is a fourth, engine-internal case —
//! not one of the ticket's two outcomes at all: this ticket wires up
//! exactly one operation kind end-to-end ([`CONVERSATION_MESSAGE_KIND`]).
//! An entry of any other kind is left pending, untouched, with no retry
//! state consumed and no journal write — there's nothing wrong with it,
//! this engine just doesn't yet know how to submit it. That's different
//! from "the server rejected it" and must not be confused with a terminal
//! failure.
//!
//! `Err(SubmitError::AuthenticationRequired)` is a fifth case, added
//! alongside the 401/403 handling below: the session token itself was
//! refused, not the request's content. Also left pending, also no backoff —
//! see that variant's doc comment.
//!
//! ## A stale session token must not discard queued work
//!
//! [`HttpTransport`] classifies `401` — `crates/server/src/handlers.rs::authenticate_token`'s
//! response to a missing, unknown, or expired session token, via
//! `crates/server/src/error.rs`'s `AppError::Unauthorized` — as
//! [`SubmitError::AuthenticationRequired`], not [`SubmitOutcome::Rejected`].
//! A 401 means the *credential* is stale, not that the request is
//! permanently invalid; an integrator that queues messages offline and later
//! drains with an expired token must get the chance to retry after
//! re-authenticating, not have every queued entry permanently discarded.
//! It's deliberately its own [`SubmitError`] variant rather than folded
//! into [`SubmitError::Retryable`]: scheduling an automatic backoff retry
//! against the *same* token would just keep failing identically forever
//! (waiting never fixes an expired token, only a fresh
//! `AvalonClient::authenticate` call does) — so the engine leaves the entry
//! pending without consuming backoff state, and reports
//! [`DrainOutcome::AuthenticationRequired`] so the caller knows to
//! re-authenticate (which yields a new `Session`/`HttpTransport`, see
//! [`crate::Session::submission_transport`]) before its next `drain()`.
//!
//! A `403` on this endpoint, by contrast, is deliberately **not**
//! reclassified the same way. `POST /conversations/{id}/messages` only ever
//! returns 403 from `crates/server/src/conversations.rs::require_unblocked_participant`
//! (the caller isn't, or is no longer, a participant — see
//! [`crate::SdkError::NotConversationParticipant`]'s doc comment for why a
//! block and a never-was-a-participant look identical on purpose); this
//! endpoint has no separate capability/authorization layer that a 403 could
//! also mean "re-grant and retry" for (server-side capability enforcement,
//! #26–#28, isn't wired up yet). Since a queued message's target
//! conversation/block state isn't expected to change on its own, that 403
//! stays a terminal [`SubmitOutcome::Rejected`], same as before.
//!
//! ## A retried submission that lost the race and was already pruned isn't a real rejection
//!
//! `crates/server/src/conversations.rs::send_message`'s idempotency lookup
//! (`find_message_by_client_entry_id`) can itself miss and return
//! `AppError::MessageNotFound` (404) — only reachable when this retry lost
//! the `client_entry_id` conflict to an earlier attempt whose row was then
//! pruned (`prune_conversation`) before this lookup ran. On this specific
//! endpoint that 404 has no other cause (a missing conversation, or a
//! caller who was never a participant, both surface as the 403 above
//! instead) — so [`HttpTransport`] treats it as
//! [`SubmitOutcome::Applied`] rather than [`SubmitOutcome::Rejected`]: the
//! message genuinely was applied once, by the earlier attempt, before
//! ordinary cap pruning removed it.
//!
//! ## Idempotency
//!
//! Every submission passes the journal entry's [`EntryId`] through to
//! [`Transport::submit`], which is expected to include it in the request so
//! the receiving endpoint can dedupe (see `HttpTransport`'s conversation
//! message submission, which routes through
//! [`crate::conversations::ConversationHandle::send_with_client_entry_id`],
//! and `crates/server/src/conversations.rs::send_message`'s
//! `client_entry_id` handling — the one endpoint this ticket wires
//! idempotency into end-to-end).

use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::sync_journal::{EntryId, JournalEntry, JournalError, SyncJournal};
use crate::{SdkError, Session};

/// The one journal `kind` this ticket wires end-to-end: a queued
/// conversation message (`docs/architecture/synchronization.md`'s
/// "Chat / conversation message" row), submitted to
/// `POST /conversations/{id}/messages`. Matches the `kind` string already
/// used by `sync_journal`'s own doc examples and tests.
pub const CONVERSATION_MESSAGE_KIND: &str = "chat.message";

/// What [`SyncJournal::append`]'s `payload` must deserialize to for a
/// [`CONVERSATION_MESSAGE_KIND`] entry — the two fields
/// `POST /conversations/{id}/messages` needs beyond the body it already
/// carries: which conversation, and what to say.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessagePayload {
    pub conversation_id: Uuid,
    pub body: String,
}

/// A definitive, terminal result for a submission attempt — the server was
/// reached and gave a final answer either way. See the module docs' "Retry
/// vs. terminal failure" section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitOutcome {
    Applied,
    /// The world moved on while this was pending, or the request is
    /// otherwise permanently invalid — reconciliation surfaces this as
    /// [`DrainOutcome::Rejected`], never retried.
    Rejected {
        reason: String,
    },
}

/// Why [`Transport::submit`] could not produce a [`SubmitOutcome`] this
/// attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitError {
    /// A network error or 5xx — safe, and expected, to retry with backoff.
    Retryable(String),
    /// This transport has no submission logic for the entry's `kind` yet.
    /// Not a failure of the entry itself — see the module docs. Left
    /// pending, no retry state consumed.
    UnsupportedKind,
    /// The server rejected the session token itself (401) rather than the
    /// request's content — see the module docs' "A stale session token
    /// must not discard queued work" section. Left pending, no backoff
    /// state consumed (retrying against the same stale token would just
    /// fail identically); the caller should re-authenticate and drain again
    /// with a fresh [`crate::Session::submission_transport`].
    AuthenticationRequired,
}

/// Submits one journal entry and classifies the result. Implemented for
/// real submission by [`HttpTransport`]; tests implement it directly with a
/// scripted/fake transport — no live server required for anything except
/// the one `#[ignore]`d end-to-end dedupe test.
#[async_trait]
pub trait Transport: Send + Sync {
    async fn submit(&self, entry: &JournalEntry) -> Result<SubmitOutcome, SubmitError>;
}

/// The real transport: submits [`CONVERSATION_MESSAGE_KIND`] entries via
/// [`crate::conversations::ConversationHandle::send_with_client_entry_id`],
/// passing the journal entry's [`EntryId`] as `client_entry_id`, the
/// idempotency key `crates/server/src/conversations.rs::send_message`
/// dedupes on. Every other `kind` is [`SubmitError::UnsupportedKind`] — see
/// the module docs.
///
/// Deliberately does *not* build its own `reqwest` request: routing through
/// `Session`/`ConversationHandle` means this transport and an integrator calling
/// `Session::conversation(id).send()` directly share exactly one
/// request-building path (see `conversations.rs`'s module doc comment) and
/// therefore classify the same failure — e.g. a missing `messages.send`
/// grant — identically, rather than one path checking capabilities locally
/// and the other only discovering the problem after a round trip.
///
/// Built via [`crate::Session::submission_transport`], which borrows the
/// session — an integrator never constructs this directly.
pub struct HttpTransport<'a> {
    session: &'a Session,
}

impl<'a> HttpTransport<'a> {
    pub(crate) fn new(session: &'a Session) -> Self {
        Self { session }
    }

    async fn submit_conversation_message(
        &self,
        entry: &JournalEntry,
    ) -> Result<SubmitOutcome, SubmitError> {
        let payload: ConversationMessagePayload =
            match serde_json::from_value(entry.payload.clone()) {
                Ok(p) => p,
                // A journal entry whose payload doesn't even parse can never
                // succeed no matter how many times it's retried — terminal, not
                // retryable, same as a server-side 4xx.
                Err(e) => {
                    return Ok(SubmitOutcome::Rejected {
                        reason: format!(
                            "malformed journal payload for {CONVERSATION_MESSAGE_KIND}: {e}"
                        ),
                    })
                }
            };

        let result = self
            .session
            .conversation(payload.conversation_id)
            .send_with_client_entry_id(&payload.body, Some(entry.id))
            .await;

        classify_send_result(result)
    }
}

/// Classifies the outcome of a `ConversationHandle::send_with_client_entry_id`
/// call into a [`SubmitOutcome`]/[`SubmitError`] — split out from
/// [`HttpTransport::submit_conversation_message`] as a pure function purely
/// so it's unit-testable without a live server (every case here comes from
/// an `SdkError` value, which is cheap to construct directly). See the
/// module docs for the reasoning behind each case.
fn classify_send_result(
    result: Result<avalon_protocol::social::ConversationMessage, SdkError>,
) -> Result<SubmitOutcome, SubmitError> {
    match result {
        Ok(_message) => Ok(SubmitOutcome::Applied),

        // A capability check failure is a local, instant rejection —
        // `ConversationHandle::send_with_client_entry_id` never made a
        // request at all. Terminal: retrying without a re-granted
        // capability can't succeed. See the module docs.
        Err(SdkError::CapabilityNotGranted(capability)) => Ok(SubmitOutcome::Rejected {
            reason: format!("capability not granted: {capability}"),
        }),

        // 403 on this endpoint — see the module docs' "A stale session
        // token must not discard queued work" section for why this is
        // *not* reclassified the way 401 is.
        Err(SdkError::NotConversationParticipant) => Ok(SubmitOutcome::Rejected {
            reason: "not a participant in this conversation".to_string(),
        }),

        // See the module docs' "A stale session token must not discard
        // queued work" section.
        Err(SdkError::Unauthorized) => Err(SubmitError::AuthenticationRequired),

        // A network-level failure, a timeout, or a 502/503/504 —
        // `crate::http::send` itself already retried this once
        // (`send_with_client_entry_id` always sets `client_entry_id`, so
        // this call is idempotent), so this is what's left after those
        // retries were exhausted. The request itself was fine; try again
        // later at the journal level.
        Err(SdkError::Unavailable { detail, .. }) => Err(SubmitError::Retryable(format!(
            "avalon-server unavailable: {detail}"
        ))),

        // See the module docs' "A retried submission that lost the race
        // and was already pruned isn't a real rejection" section: on this
        // endpoint, with `client_entry_id` always set by this transport, a
        // 404 can only mean the message was already applied by an earlier
        // attempt and then pruned before this retry's idempotency lookup
        // found it.
        Err(SdkError::NotFound(_)) => Ok(SubmitOutcome::Applied),

        // Any other mapped outcome (`Conflict`, `Rejected`, `Protocol`,
        // and anything else not explicitly reachable through this call
        // today) — the request is now invalid or was answered in a way
        // this transport doesn't special-case: terminal, not retried.
        // Handled conservatively rather than matched away, so a future
        // variant added to that path doesn't silently misclassify.
        Err(other) => Ok(SubmitOutcome::Rejected {
            reason: other.to_string(),
        }),
    }
}

#[async_trait]
impl Transport for HttpTransport<'_> {
    async fn submit(&self, entry: &JournalEntry) -> Result<SubmitOutcome, SubmitError> {
        match entry.kind.as_str() {
            CONVERSATION_MESSAGE_KIND => self.submit_conversation_message(entry).await,
            _ => Err(SubmitError::UnsupportedKind),
        }
    }
}

/// Exponential backoff, capped. `delay_for_attempt(n)` is the delay before
/// the `n`th retry (1-indexed: the delay after the *first* failure is
/// `delay_for_attempt(1)`).
#[derive(Debug, Clone)]
pub struct BackoffPolicy {
    pub base: Duration,
    pub multiplier: f64,
    pub max: Duration,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            base: Duration::seconds(1),
            multiplier: 2.0,
            max: Duration::seconds(60),
        }
    }
}

impl BackoffPolicy {
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let attempt = attempt.max(1);
        let factor = self.multiplier.powi((attempt - 1) as i32);
        let secs = (self.base.as_seconds_f64() * factor).min(self.max.as_seconds_f64());
        Duration::seconds_f64(secs)
    }
}

/// A wall-clock source, abstracted so backoff scheduling is testable
/// without a real sleep. [`SystemClock`] is what every real
/// [`SubmissionEngine`] uses; tests substitute a fake that advances on
/// command.
pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

struct RetryState {
    attempt: u32,
    next_attempt_at: OffsetDateTime,
}

/// The per-entry outcome of one [`SubmissionEngine::drain`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainOutcome {
    Applied,
    /// Terminal — see the module docs. Never retried again.
    Rejected {
        reason: String,
    },
    /// Still pending: either backoff hasn't elapsed yet, this attempt just
    /// failed with a retryable error, or this transport doesn't (yet) know
    /// how to submit this entry's `kind`.
    Deferred,
    /// Still pending, same as [`Self::Deferred`], but for a distinct reason
    /// worth surfacing separately: the session token itself was rejected
    /// (401). No backoff was scheduled — see [`SubmitError::AuthenticationRequired`]'s
    /// doc comment. The caller should re-authenticate before its next
    /// `drain()` call rather than simply calling `drain()` again.
    AuthenticationRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainReport {
    pub id: EntryId,
    pub kind: String,
    pub outcome: DrainOutcome,
}

/// A terminal status transition (issue #113) — pushed to every listener
/// registered via [`SubmissionEngine::subscribe`] the moment [`drain`](SubmissionEngine::drain)
/// produces a terminal [`DrainOutcome`] for an entry, so a caller doesn't
/// have to poll [`SyncJournal::status_of`] to notice one. Fires exactly
/// once per entry: once an entry reaches a terminal state it leaves
/// [`SyncJournal::pending`] and [`SubmissionEngine::drain`] never attempts
/// (and so never reports) it again. `Deferred`/`AuthenticationRequired`
/// outcomes are not transitions — the entry is still pending, nothing
/// changed from a caller's point of view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusTransition {
    Submitted {
        id: EntryId,
        kind: String,
    },
    Rejected {
        id: EntryId,
        kind: String,
        reason: String,
    },
}

/// A callback registered via [`SubmissionEngine::subscribe`]. Boxed rather
/// than generic over the engine so an integrator can register more than one
/// (a UI update *and* a metrics hook, say) without the engine's own type
/// growing a listener-count type parameter.
type StatusListener = Box<dyn Fn(&StatusTransition) + Send + Sync>;

/// Drains a [`SyncJournal`], submitting each pending entry through a
/// [`Transport`]. Deliberately owns no journal or transport itself — both
/// are passed to [`drain`](SubmissionEngine::drain) each call, so an
/// integrator can drive it from whatever loop/timer/reconnect-hook makes
/// sense for it (`docs/architecture/synchronization.md`: draining never
/// blocks gameplay). The only state this engine keeps across calls is in-memory
/// backoff bookkeeping (per [`EntryId`]) — not persisted, so a process
/// restart resets backoff, which is fine: a restart is itself as good a
/// reason as any to try again immediately.
pub struct SubmissionEngine<C: Clock = SystemClock> {
    backoff: BackoffPolicy,
    clock: C,
    retry_state: HashMap<EntryId, RetryState>,
    listeners: Vec<StatusListener>,
}

impl SubmissionEngine<SystemClock> {
    pub fn new() -> Self {
        Self::with_backoff(BackoffPolicy::default())
    }

    pub fn with_backoff(backoff: BackoffPolicy) -> Self {
        Self {
            backoff,
            clock: SystemClock,
            retry_state: HashMap::new(),
            listeners: Vec::new(),
        }
    }
}

impl Default for SubmissionEngine<SystemClock> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: Clock> SubmissionEngine<C> {
    #[cfg(test)]
    fn with_clock(backoff: BackoffPolicy, clock: C) -> Self {
        Self {
            backoff,
            clock,
            retry_state: HashMap::new(),
            listeners: Vec::new(),
        }
    }

    /// Registers `listener` to be called synchronously, in-process, on
    /// every subsequent terminal transition a [`drain`](Self::drain) call
    /// produces (issue #113) — no polling, no background thread, no async
    /// channel: `drain` calls it directly, inline, before returning. An
    /// integrator wanting async delivery instead composes this with
    /// whatever channel type its own runtime prefers (send into it from the
    /// closure) rather than this crate committing to one.
    pub fn subscribe(&mut self, listener: impl Fn(&StatusTransition) + Send + Sync + 'static) {
        self.listeners.push(Box::new(listener));
    }

    fn notify(&self, transition: &StatusTransition) {
        for listener in &self.listeners {
            listener(transition);
        }
    }

    /// Submits every [`SyncJournal::pending`] entry, per-kind ordered by
    /// `recorded_at`, applying backoff for entries whose next scheduled
    /// attempt hasn't arrived yet. Returns one [`DrainReport`] per pending
    /// entry — including [`DrainOutcome::Deferred`] ones a caller may want
    /// to ignore, but never silently drops an entry from the report.
    pub async fn drain(
        &mut self,
        journal: &dyn SyncJournal,
        transport: &dyn Transport,
    ) -> Result<Vec<DrainReport>, JournalError> {
        let pending = journal.pending()?;

        let mut by_kind: BTreeMap<String, Vec<JournalEntry>> = BTreeMap::new();
        for entry in pending {
            by_kind.entry(entry.kind.clone()).or_default().push(entry);
        }

        let mut reports = Vec::new();
        for (_, mut entries) in by_kind {
            entries.sort_by_key(|e| e.recorded_at);
            for entry in entries {
                reports.push(self.drain_one(journal, transport, entry).await?);
            }
        }
        Ok(reports)
    }

    async fn drain_one(
        &mut self,
        journal: &dyn SyncJournal,
        transport: &dyn Transport,
        entry: JournalEntry,
    ) -> Result<DrainReport, JournalError> {
        let id = entry.id;
        let kind = entry.kind.clone();
        let now = self.clock.now();

        if let Some(state) = self.retry_state.get(&id) {
            if state.next_attempt_at > now {
                return Ok(DrainReport {
                    id,
                    kind,
                    outcome: DrainOutcome::Deferred,
                });
            }
        }

        let outcome = match transport.submit(&entry).await {
            Ok(SubmitOutcome::Applied) => {
                journal.mark_submitted(id)?;
                self.retry_state.remove(&id);
                self.notify(&StatusTransition::Submitted {
                    id,
                    kind: kind.clone(),
                });
                DrainOutcome::Applied
            }
            Ok(SubmitOutcome::Rejected { reason }) => {
                // Terminal: a genuine rejection (issue #113), distinct from
                // a real submission — never attempted again either way.
                journal.mark_rejected(id, reason.clone())?;
                self.retry_state.remove(&id);
                self.notify(&StatusTransition::Rejected {
                    id,
                    kind: kind.clone(),
                    reason: reason.clone(),
                });
                DrainOutcome::Rejected { reason }
            }
            Err(SubmitError::Retryable(reason)) => {
                journal.mark_failed(id, reason)?;
                let attempt = self.retry_state.get(&id).map_or(1, |s| s.attempt + 1);
                let delay = self.backoff.delay_for_attempt(attempt);
                self.retry_state.insert(
                    id,
                    RetryState {
                        attempt,
                        next_attempt_at: now + delay,
                    },
                );
                DrainOutcome::Deferred
            }
            Err(SubmitError::UnsupportedKind) => DrainOutcome::Deferred,
            Err(SubmitError::AuthenticationRequired) => {
                // Recorded for diagnostics, same as a retryable failure, but
                // no backoff state — see the module docs and
                // `SubmitError::AuthenticationRequired`'s doc comment for
                // why retrying against the same token wouldn't help.
                journal.mark_failed(id, "session token rejected (401)".to_string())?;
                DrainOutcome::AuthenticationRequired
            }
        };

        Ok(DrainReport { id, kind, outcome })
    }

    /// Test/diagnostic introspection: how many consecutive retryable
    /// failures have been recorded in-memory for `id`, if any.
    #[cfg(test)]
    fn attempt_count(&self, id: EntryId) -> Option<u32> {
        self.retry_state.get(&id).map(|s| s.attempt)
    }

    #[cfg(test)]
    fn next_attempt_at(&self, id: EntryId) -> Option<OffsetDateTime> {
        self.retry_state.get(&id).map(|s| s.next_attempt_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    use crate::sync_journal::FileJournal;

    fn temp_journal_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "avalon-submission-engine-{label}-{}.jsonl",
            Uuid::new_v4()
        ))
    }

    fn conversation_payload() -> serde_json::Value {
        serde_json::json!({
            "conversation_id": Uuid::new_v4(),
            "body": "hello",
        })
    }

    struct FakeClock {
        now: Mutex<OffsetDateTime>,
    }

    impl FakeClock {
        fn new(now: OffsetDateTime) -> Self {
            Self {
                now: Mutex::new(now),
            }
        }

        fn advance(&self, delta: Duration) {
            *self.now.lock().unwrap() += delta;
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> OffsetDateTime {
            *self.now.lock().unwrap()
        }
    }

    /// A scripted transport: `outcomes` is consumed in order, one entry per
    /// call to `submit`, regardless of which journal entry is asking — that
    /// granularity is all these tests need (each test only ever has one
    /// entry pending).
    struct ScriptedTransport {
        outcomes: Mutex<Vec<Result<SubmitOutcome, SubmitError>>>,
        call_count: Mutex<u32>,
    }

    impl ScriptedTransport {
        fn new(outcomes: Vec<Result<SubmitOutcome, SubmitError>>) -> Self {
            Self {
                outcomes: Mutex::new(outcomes),
                call_count: Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl Transport for ScriptedTransport {
        async fn submit(&self, _entry: &JournalEntry) -> Result<SubmitOutcome, SubmitError> {
            *self.call_count.lock().unwrap() += 1;
            let mut outcomes = self.outcomes.lock().unwrap();
            if outcomes.is_empty() {
                panic!("ScriptedTransport ran out of scripted outcomes");
            }
            outcomes.remove(0)
        }
    }

    /// A transport that models server-side idempotency: submitting the same
    /// `EntryId` twice only ever "applies" once, exactly like
    /// `crates/server/src/conversations.rs::send_message`'s dedupe-on-
    /// `client_entry_id` behavior. Used to prove the engine always threads
    /// the entry's `EntryId` through in a way a real dedupe key could use.
    struct DedupingTransport {
        applied_ids: Mutex<HashSet<EntryId>>,
        apply_attempts: Mutex<u32>,
    }

    impl DedupingTransport {
        fn new() -> Self {
            Self {
                applied_ids: Mutex::new(HashSet::new()),
                apply_attempts: Mutex::new(0),
            }
        }

        fn applied_count(&self) -> usize {
            self.applied_ids.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl Transport for DedupingTransport {
        async fn submit(&self, entry: &JournalEntry) -> Result<SubmitOutcome, SubmitError> {
            *self.apply_attempts.lock().unwrap() += 1;
            // Simulate: the first attempt for this EntryId "landed" on the
            // server (row inserted) but its response was dropped before the
            // client saw it — the engine sees this as Retryable and retries.
            // A retried request for the *same* EntryId dedupes server-side
            // and is Applied without creating a second row.
            let mut applied = self.applied_ids.lock().unwrap();
            if applied.contains(&entry.id) {
                return Ok(SubmitOutcome::Applied);
            }
            if *self.apply_attempts.lock().unwrap() == 1 {
                // First-ever attempt: pretend the response was dropped.
                return Err(SubmitError::Retryable(
                    "simulated dropped response".to_string(),
                ));
            }
            applied.insert(entry.id);
            Ok(SubmitOutcome::Applied)
        }
    }

    #[tokio::test]
    async fn a_retried_submission_after_a_dropped_response_does_not_double_apply() {
        // Lower-level dedupe-logic test (the live end-to-end equivalent is
        // `crates/server/tests/conversations.rs`'s `#[ignore]`d test against
        // the real endpoint). This proves the engine's own behavior: an
        // entry that failed retryably and is retried never results in more
        // than one logical "applied" outcome, because the journal entry
        // keeps the same `EntryId` across every attempt and the engine
        // keeps retrying the *same* pending entry rather than creating a
        // new one.
        let path = temp_journal_path("dedupe");
        let journal = FileJournal::open(&path).unwrap();
        let id = journal
            .append(
                CONVERSATION_MESSAGE_KIND.to_string(),
                conversation_payload(),
            )
            .unwrap();

        let transport = DedupingTransport::new();
        let clock = FakeClock::new(OffsetDateTime::now_utc());
        let mut engine = SubmissionEngine::with_clock(BackoffPolicy::default(), clock);

        // First drain: the transport's first-ever call is scripted to look
        // like a dropped response (Retryable).
        let reports = engine.drain(&journal, &transport).await.unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].id, id);
        assert!(matches!(reports[0].outcome, DrainOutcome::Deferred));
        assert_eq!(
            transport.applied_count(),
            0,
            "must not be recorded as applied yet"
        );

        // Force the backoff window open and drain again — this is the
        // retry. The entry must still be the *same* pending entry (no
        // duplicate was appended), and the transport must apply it exactly
        // once.
        engine
            .clock
            .advance(engine.backoff.max + Duration::seconds(1));
        let reports = engine.drain(&journal, &transport).await.unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].id, id);
        assert_eq!(reports[0].outcome, DrainOutcome::Applied);
        assert_eq!(
            transport.applied_count(),
            1,
            "exactly one resulting applied row for this EntryId"
        );

        // A third drain has nothing left pending — the entry is submitted.
        let reports = engine.drain(&journal, &transport).await.unwrap();
        assert!(reports.is_empty());
        assert_eq!(transport.applied_count(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn a_permanently_rejected_entry_surfaces_rejected_and_is_not_retried() {
        let path = temp_journal_path("rejected");
        let journal = FileJournal::open(&path).unwrap();
        let id = journal
            .append(
                CONVERSATION_MESSAGE_KIND.to_string(),
                conversation_payload(),
            )
            .unwrap();

        let transport = ScriptedTransport::new(vec![Ok(SubmitOutcome::Rejected {
            reason: "target conversation no longer exists".to_string(),
        })]);
        let mut engine = SubmissionEngine::new();

        let reports = engine.drain(&journal, &transport).await.unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].id, id);
        match &reports[0].outcome {
            DrainOutcome::Rejected { reason } => {
                assert_eq!(reason, "target conversation no longer exists")
            }
            other => panic!("expected Rejected, got {other:?}"),
        }

        // Next drain cycle: nothing pending, transport not called again —
        // ScriptedTransport would panic if it were, since it only has one
        // scripted outcome.
        let reports = engine.drain(&journal, &transport).await.unwrap();
        assert!(reports.is_empty());
        assert!(
            journal.pending().unwrap().is_empty(),
            "a terminally-rejected entry must leave pending()"
        );
        assert_eq!(
            journal.status_of(id).unwrap(),
            crate::sync_journal::EntryStatus::Rejected {
                reason: "target conversation no longer exists".to_string()
            },
            "issue #113: a terminal rejection must be readable back through status_of, \
             distinct from a real submission"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn subscribe_fires_exactly_once_on_a_terminal_applied_transition() {
        let path = temp_journal_path("subscribe-applied");
        let journal = FileJournal::open(&path).unwrap();
        let id = journal
            .append(
                CONVERSATION_MESSAGE_KIND.to_string(),
                conversation_payload(),
            )
            .unwrap();

        let transport = ScriptedTransport::new(vec![Ok(SubmitOutcome::Applied)]);
        let mut engine = SubmissionEngine::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_for_listener = Arc::clone(&seen);
        engine.subscribe(move |transition| {
            seen_for_listener.lock().unwrap().push(transition.clone());
        });

        engine.drain(&journal, &transport).await.unwrap();
        // A second drain: nothing pending, listener must not fire again —
        // ScriptedTransport would also panic if called a second time,
        // since it only has one scripted outcome.
        engine.drain(&journal, &transport).await.unwrap();

        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.len(),
            1,
            "must fire exactly once, not once per drain call"
        );
        assert_eq!(
            seen[0],
            StatusTransition::Submitted {
                id,
                kind: CONVERSATION_MESSAGE_KIND.to_string()
            }
        );

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn subscribe_fires_exactly_once_on_a_terminal_rejected_transition() {
        let path = temp_journal_path("subscribe-rejected");
        let journal = FileJournal::open(&path).unwrap();
        let id = journal
            .append(
                CONVERSATION_MESSAGE_KIND.to_string(),
                conversation_payload(),
            )
            .unwrap();

        let transport = ScriptedTransport::new(vec![Ok(SubmitOutcome::Rejected {
            reason: "target conversation no longer exists".to_string(),
        })]);
        let mut engine = SubmissionEngine::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_for_listener = Arc::clone(&seen);
        engine.subscribe(move |transition| {
            seen_for_listener.lock().unwrap().push(transition.clone());
        });

        engine.drain(&journal, &transport).await.unwrap();
        engine.drain(&journal, &transport).await.unwrap();

        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.len(),
            1,
            "must fire exactly once, not once per drain call"
        );
        assert_eq!(
            seen[0],
            StatusTransition::Rejected {
                id,
                kind: CONVERSATION_MESSAGE_KIND.to_string(),
                reason: "target conversation no longer exists".to_string(),
            }
        );

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn subscribe_does_not_fire_for_a_non_terminal_deferred_outcome() {
        let path = temp_journal_path("subscribe-deferred");
        let journal = FileJournal::open(&path).unwrap();
        journal
            .append(
                CONVERSATION_MESSAGE_KIND.to_string(),
                conversation_payload(),
            )
            .unwrap();

        let transport = ScriptedTransport::new(vec![Err(SubmitError::Retryable(
            "connection reset".to_string(),
        ))]);
        let mut engine = SubmissionEngine::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_for_listener = Arc::clone(&seen);
        engine.subscribe(move |transition| {
            seen_for_listener.lock().unwrap().push(transition.clone());
        });

        let reports = engine.drain(&journal, &transport).await.unwrap();
        assert_eq!(reports.len(), 1);
        assert!(matches!(reports[0].outcome, DrainOutcome::Deferred));
        assert!(
            seen.lock().unwrap().is_empty(),
            "a still-pending entry must not notify subscribers"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn authentication_required_leaves_the_entry_pending_with_no_backoff_scheduled() {
        // Regression test for the fix: a stale session token must not
        // permanently discard queued work, and (unlike a retryable
        // failure) must not schedule a pointless backoff retry against the
        // same doomed token either.
        let path = temp_journal_path("auth-required");
        let journal = FileJournal::open(&path).unwrap();
        let id = journal
            .append(
                CONVERSATION_MESSAGE_KIND.to_string(),
                conversation_payload(),
            )
            .unwrap();

        let transport = ScriptedTransport::new(vec![Err(SubmitError::AuthenticationRequired)]);
        let mut engine = SubmissionEngine::new();

        let reports = engine.drain(&journal, &transport).await.unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].id, id);
        assert_eq!(reports[0].outcome, DrainOutcome::AuthenticationRequired);

        assert_eq!(
            journal.pending().unwrap().len(),
            1,
            "still pending — never marked submitted, unlike a terminal rejection"
        );
        assert!(
            engine.attempt_count(id).is_none(),
            "no backoff state recorded — retrying against the same token wouldn't help"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn backoff_delays_increase_and_cap_under_a_forced_failure_sequence() {
        let path = temp_journal_path("backoff");
        let journal = FileJournal::open(&path).unwrap();
        let id = journal
            .append(
                CONVERSATION_MESSAGE_KIND.to_string(),
                conversation_payload(),
            )
            .unwrap();

        let backoff = BackoffPolicy {
            base: Duration::seconds(1),
            multiplier: 2.0,
            max: Duration::seconds(10),
        };
        let clock = FakeClock::new(OffsetDateTime::now_utc());
        let mut engine = SubmissionEngine::with_clock(backoff.clone(), clock);

        // Fail forever — every attempt is retryable.
        let transport = ScriptedTransport::new(vec![
            Err(SubmitError::Retryable("1".to_string())),
            Err(SubmitError::Retryable("2".to_string())),
            Err(SubmitError::Retryable("3".to_string())),
            Err(SubmitError::Retryable("4".to_string())),
            Err(SubmitError::Retryable("5".to_string())),
        ]);

        let mut observed_delays = Vec::new();
        for _ in 0..5 {
            let before = engine.clock.now();
            let reports = engine.drain(&journal, &transport).await.unwrap();
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].outcome, DrainOutcome::Deferred);
            let scheduled = engine.next_attempt_at(id).expect("retry state recorded");
            observed_delays.push(scheduled - before);
            // Jump straight to (past) the scheduled retry so the next
            // drain() call actually attempts again instead of deferring on
            // the backoff window.
            engine
                .clock
                .advance(scheduled - before + Duration::milliseconds(1));
        }

        assert_eq!(*transport.call_count.lock().unwrap(), 5);

        // 1s, 2s, 4s, 8s, capped at 10s.
        let expected = [
            Duration::seconds(1),
            Duration::seconds(2),
            Duration::seconds(4),
            Duration::seconds(8),
            Duration::seconds(10),
        ];
        for (i, (observed, expected)) in observed_delays.iter().zip(expected.iter()).enumerate() {
            // Allow slack for the millisecond nudge added above bleeding
            // into the *next* delay's baseline — compare against the
            // expected delay to within a second.
            let diff = (*observed - *expected).abs();
            assert!(
                diff < Duration::seconds(1),
                "delay #{i}: expected ~{expected}, got {observed}"
            );
        }

        // Strictly increasing until the cap, then flat at the cap.
        for i in 1..observed_delays.len() {
            assert!(
                observed_delays[i] >= observed_delays[i - 1],
                "delay #{i} ({}) must not be smaller than delay #{} ({})",
                observed_delays[i],
                i - 1,
                observed_delays[i - 1]
            );
        }
        assert!(
            observed_delays.last().unwrap() <= &backoff.max,
            "final delay must not exceed the cap"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn backoff_policy_delay_for_attempt_is_pure_and_monotonic_up_to_the_cap() {
        let policy = BackoffPolicy {
            base: Duration::seconds(1),
            multiplier: 3.0,
            max: Duration::seconds(20),
        };
        assert_eq!(policy.delay_for_attempt(1), Duration::seconds(1));
        assert_eq!(policy.delay_for_attempt(2), Duration::seconds(3));
        assert_eq!(policy.delay_for_attempt(3), Duration::seconds(9));
        // 27s would exceed the 20s cap.
        assert_eq!(policy.delay_for_attempt(4), Duration::seconds(20));
        assert_eq!(policy.delay_for_attempt(5), Duration::seconds(20));
        // attempt 0 treated the same as attempt 1 (never negative/zero delay
        // growth from a bogus attempt count).
        assert_eq!(policy.delay_for_attempt(0), policy.delay_for_attempt(1));
    }

    #[tokio::test]
    async fn an_unsupported_kind_is_left_pending_and_is_not_treated_as_a_failure() {
        let path = temp_journal_path("unsupported-kind");
        let journal = FileJournal::open(&path).unwrap();
        let id = journal
            .append(
                "guild.join_request".to_string(),
                serde_json::json!({ "guild": "The Round Table" }),
            )
            .unwrap();

        // Never gets called — HttpTransport itself would return
        // UnsupportedKind for this kind without a network call, but here a
        // ScriptedTransport with zero outcomes proves it's never invoked at
        // all… actually the engine always calls `transport.submit`, so
        // script exactly one UnsupportedKind response.
        let transport = ScriptedTransport::new(vec![Err(SubmitError::UnsupportedKind)]);
        let mut engine = SubmissionEngine::new();

        let reports = engine.drain(&journal, &transport).await.unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].id, id);
        assert_eq!(reports[0].outcome, DrainOutcome::Deferred);
        assert!(
            engine.attempt_count(id).is_none(),
            "no retry/backoff state should be created for an unsupported kind"
        );
        assert_eq!(
            journal.pending().unwrap().len(),
            1,
            "still pending — untouched, not marked submitted or failed"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn groups_are_submitted_in_per_kind_recorded_at_order() {
        let path = temp_journal_path("ordering");
        let journal = FileJournal::open(&path).unwrap();

        // Two kinds, interleaved append order, to prove grouping+ordering
        // is by (kind, recorded_at) and not just append order.
        let a1 = journal
            .append(
                CONVERSATION_MESSAGE_KIND.to_string(),
                conversation_payload(),
            )
            .unwrap();
        let b1 = journal
            .append("achievement.earned".to_string(), serde_json::json!({}))
            .unwrap();
        let a2 = journal
            .append(
                CONVERSATION_MESSAGE_KIND.to_string(),
                conversation_payload(),
            )
            .unwrap();

        let transport = ScriptedTransport::new(vec![
            Ok(SubmitOutcome::Applied),
            Ok(SubmitOutcome::Applied),
            Err(SubmitError::UnsupportedKind),
        ]);
        let mut engine = SubmissionEngine::new();

        let reports = engine.drain(&journal, &transport).await.unwrap();
        assert_eq!(reports.len(), 3);

        // "achievement.earned" < "chat.message" lexicographically, so that
        // kind's single entry is visited first; within chat.message, a1
        // (recorded first) precedes a2.
        assert_eq!(reports[0].id, b1);
        assert_eq!(reports[1].id, a1);
        assert_eq!(reports[2].id, a2);

        let _ = std::fs::remove_file(&path);
    }

    fn sample_message() -> avalon_protocol::social::ConversationMessage {
        avalon_protocol::social::ConversationMessage {
            id: Uuid::new_v4(),
            conversation_id: Uuid::new_v4(),
            author: avalon_protocol::ids::IdentityId(Uuid::new_v4()),
            body: "hi".to_string(),
            sent_at: OffsetDateTime::now_utc(),
        }
    }

    #[test]
    fn a_401_is_classified_as_authentication_required_not_a_terminal_rejection() {
        // The regression this fix targets: an expired/invalid session token
        // must leave the entry pending for a later retry, never permanently
        // discard it as `Rejected`.
        let result = classify_send_result(Err(SdkError::Unauthorized));
        assert!(matches!(result, Err(SubmitError::AuthenticationRequired)));
    }

    #[test]
    fn a_403_not_a_participant_stays_a_terminal_rejection() {
        // Deliberately not reclassified the way 401 is — see the module
        // docs for why this endpoint's 403 is application state, not a
        // stale credential.
        let result = classify_send_result(Err(SdkError::NotConversationParticipant));
        assert!(matches!(result, Ok(SubmitOutcome::Rejected { .. })));
    }

    #[test]
    fn a_missing_capability_is_a_terminal_rejection_not_a_retry() {
        let result = classify_send_result(Err(SdkError::CapabilityNotGranted(
            "messages.send".to_string(),
        )));
        assert!(matches!(result, Ok(SubmitOutcome::Rejected { .. })));
    }

    #[test]
    fn a_404_on_this_endpoint_is_treated_as_already_applied() {
        // See the module docs' "A retried submission that lost the race and
        // was already pruned isn't a real rejection" section.
        let result = classify_send_result(Err(SdkError::NotFound("message".to_string())));
        assert!(matches!(result, Ok(SubmitOutcome::Applied)));
    }

    #[test]
    fn a_429_and_5xx_stay_retryable() {
        let result = classify_send_result(Err(SdkError::Unavailable {
            retried: 0,
            detail: "rate limited or server error".to_string(),
        }));
        assert!(matches!(result, Err(SubmitError::Retryable(_))));
    }

    #[test]
    fn any_other_4xx_stays_a_terminal_rejection() {
        let result = classify_send_result(Err(SdkError::Rejected {
            reason: "bad request".to_string(),
        }));
        assert!(matches!(result, Ok(SubmitOutcome::Rejected { .. })));
    }

    #[test]
    fn a_success_is_applied() {
        let result = classify_send_result(Ok(sample_message()));
        assert!(matches!(result, Ok(SubmitOutcome::Applied)));
    }
}
