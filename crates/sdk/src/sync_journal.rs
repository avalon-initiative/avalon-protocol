//! Local durable event journal (issue #110) — the storage half of offline
//! participation described in `docs/architecture/synchronization.md`. This
//! module records *intent* locally and durably; it does not submit
//! anything, dedupe anything, sign anything, or decide what a recorded
//! entry is worth. That is #111 (submission engine) and #112 (offline trust
//! model, already decided: deferred requests, never deferred attestations —
//! no issuer key exists client-side, full stop).
//!
//! ## Storage backend: append-only JSON-lines file, not embedded SQLite
//!
//! [`FileJournal`] is a flat, append-only file of newline-delimited JSON
//! records (an `Append`, `Submitted`, `Rejected`, or `Failed` op per line),
//! replayed in full on [`FileJournal::open`] to reconstitute in-memory
//! state. Chosen
//! over `rusqlite`/embedded SQLite for this reference implementation
//! because:
//!
//! - It adds zero new dependencies — `uuid`, `time`, and `serde_json` are
//!   already workspace dependencies this crate uses elsewhere. `rusqlite`
//!   (or a bundled `libsqlite3-sys`) pulls in a C dependency and a
//!   compiler-toolchain requirement onto every integrator that links this crate,
//!   which is a heavy ask for what's fundamentally a small local log.
//! - Crash safety only needs one property: an `fsync`'d append either fully
//!   landed or didn't. A flat file gets that directly (`write_all` the
//!   line, then `File::sync_all`) without needing a database engine's
//!   transaction machinery, WAL checkpointing, or page cache.
//! - Recovery is "replay the file, stop at the first line that doesn't
//!   parse" — trivial to reason about and to test deterministically, which
//!   matters more here than raw throughput; this journal is not a hot path
//!   (`docs/architecture/synchronization.md`: "local, fast, always-available
//!   operation", not "high volume").
//!
//! The trade-off: no concurrent-writer story (one `FileJournal` per path,
//! guarded by an internal mutex) and O(n) replay on open. Neither matters
//! for a single integrator client's local journal. `SyncJournal` is a trait
//! specifically so an integrator (or another SDK — C#, a future mobile
//! binding) can swap in SQLite, platform storage, or anything else without
//! the rest of the SDK caring.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Client-generated, stable, never server-assigned (issue #110's invariant —
/// the future submission engine, #111, depends on this for idempotency).
pub type EntryId = Uuid;

/// Everything a [`SyncJournal`] implementation can fail with.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    /// The underlying storage (e.g. a file) failed.
    #[error("journal io error: {0}")]
    Io(#[from] std::io::Error),
    /// A record couldn't be (de)serialized.
    #[error("failed to serialize journal record: {0}")]
    Serialize(#[from] serde_json::Error),
    /// The given [`EntryId`] has no entry in this journal.
    #[error("no journal entry with id {0}")]
    NotFound(Uuid),
}

/// A single recorded offline-capable operation. `id` is client-generated at
/// [`SyncJournal::append`] time and never changes. `submitted_at` is `None`
/// until [`SyncJournal::mark_submitted`], `rejected_at`/`rejection_reason`
/// are `None` until [`SyncJournal::mark_rejected`] — the journal itself
/// carries no opinion about *whether* an entry should be believed once
/// submitted; see the offline-trust-model decision (#112) for that.
/// `submitted_at` and `rejected_at` are mutually exclusive in practice (an
/// entry reaches exactly one terminal state) but both are plain `Option`s
/// rather than a combined enum so the on-disk/wire shape stays additive —
/// see issue #113's own scope note on why this wasn't a bigger refactor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Client-generated, stable identifier — see [`EntryId`].
    pub id: EntryId,
    /// What kind of operation this is (e.g.
    /// [`crate::submission::CONVERSATION_MESSAGE_KIND`]) — determines how a
    /// [`crate::submission::Transport`] submits it.
    pub kind: String,
    /// The operation's own data, shaped per its `kind`.
    pub payload: serde_json::Value,
    /// When this entry was appended.
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    /// When [`SyncJournal::mark_submitted`] was called, if it has been.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub submitted_at: Option<OffsetDateTime>,
    /// When [`SyncJournal::mark_rejected`] was called, if it has been.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub rejected_at: Option<OffsetDateTime>,
    /// Why, if [`SyncJournal::mark_rejected`] has been called.
    #[serde(default)]
    pub rejection_reason: Option<String>,
}

impl JournalEntry {
    /// Whether this entry has reached either terminal state — the same
    /// check [`SyncJournal::pending`]'s default backing logic and
    /// [`SyncJournal::status`]/[`SyncJournal::status_of`] all need, kept in
    /// one place so they can never drift against each other.
    fn is_terminal(&self) -> bool {
        self.submitted_at.is_some() || self.rejected_at.is_some()
    }
}

/// A snapshot of overall sync progress (issue #113) — the cheap, synchronous
/// local read a game renders "N pending" / "last synced 2 minutes ago" from,
/// never a network call itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncStatus {
    /// How many entries are currently pending.
    pub pending_count: usize,
    /// `recorded_at` of the oldest still-pending entry, if any — how long
    /// the longest-queued operation has been waiting.
    pub oldest_pending_at: Option<OffsetDateTime>,
    /// The most recent successful submission across *every* entry, pending
    /// or not — `None` only if nothing has ever been submitted through this
    /// journal.
    pub last_synced_at: Option<OffsetDateTime>,
}

/// One entry's status (issue #113) — what a game renders per-message/
/// per-request rather than re-rendering everything from [`SyncStatus`]
/// alone. [`Self::Rejected`] always carries the reason a game can surface
/// to its player, matching this ticket's own invariant: never a bare
/// failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryStatus {
    /// Not yet submitted, or submitted but not yet answered.
    Pending,
    /// The server accepted it.
    Submitted,
    /// Terminally rejected.
    Rejected {
        /// Why, safe to surface to the player.
        reason: String,
    },
}

/// A durable local queue of offline-capable operations, behind a trait so
/// each SDK (Rust, C#, future) can back it with whatever local storage is
/// appropriate — [`FileJournal`] is one reference implementation, not the
/// only one. Deliberately synchronous: append is meant to be a fast,
/// always-available local operation an integrator never has to `.await` a network
/// round trip for (`docs/architecture/synchronization.md`).
pub trait SyncJournal {
    /// Records `kind`/`payload` as a new pending entry and returns its
    /// client-generated [`EntryId`]. Two calls with identical `kind` and
    /// `payload` produce two distinct entries — the journal does not
    /// deduplicate; that's the submission engine's job (#111), not this
    /// ticket's.
    fn append(&self, kind: String, payload: serde_json::Value) -> Result<EntryId, JournalError>;

    /// All entries not yet reaching a terminal state, oldest first.
    fn pending(&self) -> Result<Vec<JournalEntry>, JournalError>;

    /// Every entry this journal has ever recorded, pending or terminal,
    /// oldest first — the backing read [`SyncJournal::status`] and
    /// [`SyncJournal::status_of`]'s default implementations need but
    /// [`SyncJournal::pending`] alone can't answer (it deliberately excludes
    /// terminal entries).
    fn all(&self) -> Result<Vec<JournalEntry>, JournalError>;

    /// One entry by id, if it exists — the read [`SyncJournal::status_of`]'s
    /// default implementation is built on.
    fn entry(&self, id: EntryId) -> Result<Option<JournalEntry>, JournalError>;

    /// Marks an entry submitted (a genuine, successful terminal outcome).
    /// Idempotent: calling it more than once for the same id is a no-op
    /// after the first call, never an error.
    fn mark_submitted(&self, id: EntryId) -> Result<(), JournalError>;

    /// Marks an entry rejected — the other terminal outcome
    /// ([`SubmitOutcome::Rejected`](crate::submission::SubmitOutcome), the
    /// server was reached and gave a final "no"). Idempotent, same posture
    /// as [`Self::mark_submitted`]: a second call for an already-terminal
    /// id is a no-op, never an error, and never overwrites an existing
    /// [`SyncJournal::mark_submitted`] outcome or vice versa.
    fn mark_rejected(&self, id: EntryId, reason: String) -> Result<(), JournalError>;

    /// Records that a submission attempt for `id` failed, with a reason for
    /// diagnostics. Does not remove the entry from [`SyncJournal::pending`]
    /// — retry/backoff policy belongs to the submission engine (#111), not
    /// the journal. Diagnostics-only: unlike [`SyncJournal::mark_submitted`]/
    /// [`SyncJournal::mark_rejected`], this does not check whether `id` is
    /// already terminal, and never touches `submitted_at`/`rejected_at`
    /// either way — calling it after a terminal outcome just appends a
    /// harmless, ignored `Failed` record. This is for a *retryable* failure
    /// (a network error, a 5xx); a *terminal* rejection is
    /// [`Self::mark_rejected`], not this.
    fn mark_failed(&self, id: EntryId, reason: String) -> Result<(), JournalError>;

    /// A cheap, synchronous snapshot of overall progress (issue #113) —
    /// never a network call, reflects only what this journal already knows.
    fn status(&self) -> Result<SyncStatus, JournalError> {
        let all = self.all()?;
        let pending: Vec<&JournalEntry> = all.iter().filter(|e| !e.is_terminal()).collect();
        let oldest_pending_at = pending.iter().map(|e| e.recorded_at).min();
        let last_synced_at = all.iter().filter_map(|e| e.submitted_at).max();
        Ok(SyncStatus {
            pending_count: pending.len(),
            oldest_pending_at,
            last_synced_at,
        })
    }

    /// One entry's status (issue #113), by id.
    fn status_of(&self, id: EntryId) -> Result<EntryStatus, JournalError> {
        let entry = self.entry(id)?.ok_or(JournalError::NotFound(id))?;
        Ok(if let Some(reason) = entry.rejection_reason {
            EntryStatus::Rejected { reason }
        } else if entry.submitted_at.is_some() {
            EntryStatus::Submitted
        } else {
            EntryStatus::Pending
        })
    }
}

/// One line of the on-disk log. `Append` carries a full entry; `Submitted`/
/// `Rejected`/`Failed` are small follow-up records referencing an id
/// already appended earlier in the file. Replaying the file in order
/// reconstitutes state — this is intentionally *not* a random-access
/// format.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum LogRecord {
    Append {
        id: Uuid,
        kind: String,
        payload: serde_json::Value,
        #[serde(with = "time::serde::rfc3339")]
        recorded_at: OffsetDateTime,
    },
    Submitted {
        id: Uuid,
        #[serde(with = "time::serde::rfc3339")]
        at: OffsetDateTime,
    },
    Rejected {
        id: Uuid,
        #[serde(with = "time::serde::rfc3339")]
        at: OffsetDateTime,
        reason: String,
    },
    Failed {
        id: Uuid,
        reason: String,
    },
}

struct State {
    file: File,
    /// Insertion order, oldest first — `HashMap` alone wouldn't preserve it.
    order: Vec<Uuid>,
    entries: HashMap<Uuid, JournalEntry>,
}

/// The default reference [`SyncJournal`] implementation: an append-only
/// JSON-lines file with an `fsync` after every write. See the module docs
/// for why this backend was chosen over embedded SQLite.
///
/// Limitation, accepted for this first pass: submitted entries are never
/// compacted out of the file. The on-disk log only ever grows for the
/// lifetime of a journal path — fine for the offline-participation volumes
/// this is designed for (see the module docs), but something a long-lived
/// journal will eventually want (compaction/rotation), not addressed here.
pub struct FileJournal {
    path: PathBuf,
    state: Mutex<State>,
}

impl FileJournal {
    /// Opens (creating if needed) the journal at `path` and replays it to
    /// reconstitute pending/submitted state. Safe to call again after a
    /// crash — see the module-level docs and the `crash_recovery` test.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        // Opened read+write, but never used for writing directly — replay
        // reads from it, then a fresh append-mode handle below is what
        // append()/mark_submitted()/mark_failed() actually write through.
        let mut read_handle = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;

        let mut order = Vec::new();
        let mut entries: HashMap<Uuid, JournalEntry> = HashMap::new();

        // Replay byte-by-byte (not via `BufReader::lines`) because we need
        // to know exactly how many bytes make up the last known-good,
        // newline-terminated record — `good_end` — so we can truncate the
        // file back to that offset below. Without this, a trailing
        // truncated/malformed line left over from an interrupted write
        // would still be sitting on disk after `open()` returns, and the
        // next `append()` would write its record directly onto the end of
        // that leftover garbage with no separating newline, producing one
        // glued, unparseable line — silently losing the new entry on the
        // *next* reopen even though `append()` itself reported success.
        let mut bytes = Vec::new();
        read_handle.read_to_end(&mut bytes)?;

        let mut good_end: usize = 0;
        let mut pos: usize = 0;
        while pos < bytes.len() {
            let Some(rel_nl) = bytes[pos..].iter().position(|&b| b == b'\n') else {
                // No trailing newline: the last line in the file was never
                // fully written (the process died mid-`write`, before the
                // newline landed). It was never fsynced as complete, so it
                // was never acknowledged to a caller — stop replay without
                // advancing `good_end` past it.
                break;
            };
            let line_end = pos + rel_nl;
            let next_pos = line_end + 1;
            let line = match std::str::from_utf8(&bytes[pos..line_end]) {
                Ok(l) => l,
                // Invalid UTF-8 can only come from a write that was itself
                // never fully flushed/fsynced — same treatment as a parse
                // failure below: stop replay, don't advance `good_end`.
                Err(_) => break,
            };
            if line.trim().is_empty() {
                good_end = next_pos;
                pos = next_pos;
                continue;
            }
            let record: LogRecord = match serde_json::from_str(line) {
                Ok(r) => r,
                // A truncated/malformed trailing line is exactly what a
                // crash looks like on disk. Safe, and correct, to drop it
                // rather than treat the whole journal as corrupt — but
                // don't advance `good_end` past it, so it gets truncated
                // away below instead of being glued onto by the next write.
                Err(_) => break,
            };
            match record {
                LogRecord::Append {
                    id,
                    kind,
                    payload,
                    recorded_at,
                } => {
                    order.push(id);
                    entries.insert(
                        id,
                        JournalEntry {
                            id,
                            kind,
                            payload,
                            recorded_at,
                            submitted_at: None,
                            rejected_at: None,
                            rejection_reason: None,
                        },
                    );
                }
                LogRecord::Submitted { id, at } => {
                    if let Some(entry) = entries.get_mut(&id) {
                        entry.submitted_at = Some(at);
                    }
                }
                LogRecord::Rejected { id, at, reason } => {
                    if let Some(entry) = entries.get_mut(&id) {
                        entry.rejected_at = Some(at);
                        entry.rejection_reason = Some(reason);
                    }
                }
                LogRecord::Failed { .. } => {
                    // Recorded on disk for diagnostics only; a failed
                    // submission stays pending (see mark_failed's doc
                    // comment) so nothing to change in memory here.
                }
            }
            good_end = next_pos;
            pos = next_pos;
        }

        // If replay stopped early because of a trailing truncated/
        // malformed line, drop it from disk now, before any future
        // `append()`/`mark_submitted()`/`mark_failed()` call can write a
        // new record onto the end of it. This is the fix for the
        // glued-garbage bug described above.
        if (good_end as u64) < bytes.len() as u64 {
            read_handle.set_len(good_end as u64)?;
        }
        drop(read_handle);

        let write_handle = OpenOptions::new().append(true).open(&path)?;

        Ok(Self {
            path,
            state: Mutex::new(State {
                file: write_handle,
                order,
                entries,
            }),
        })
    }

    /// The file this journal is backed by.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn write_record(file: &mut File, record: &LogRecord) -> Result<(), JournalError> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        file.write_all(&line)?;
        // The whole point: durable before we return control to the caller,
        // not durable "eventually" via OS write-back.
        file.sync_all()?;
        Ok(())
    }
}

impl SyncJournal for FileJournal {
    fn append(&self, kind: String, payload: serde_json::Value) -> Result<EntryId, JournalError> {
        let id = Uuid::new_v4();
        let recorded_at = OffsetDateTime::now_utc();
        let record = LogRecord::Append {
            id,
            kind: kind.clone(),
            payload: payload.clone(),
            recorded_at,
        };

        let mut state = self.state.lock().expect("journal mutex poisoned");
        Self::write_record(&mut state.file, &record)?;
        state.order.push(id);
        state.entries.insert(
            id,
            JournalEntry {
                id,
                kind,
                payload,
                recorded_at,
                submitted_at: None,
                rejected_at: None,
                rejection_reason: None,
            },
        );
        Ok(id)
    }

    fn pending(&self) -> Result<Vec<JournalEntry>, JournalError> {
        let state = self.state.lock().expect("journal mutex poisoned");
        Ok(state
            .order
            .iter()
            .filter_map(|id| state.entries.get(id))
            .filter(|entry| !entry.is_terminal())
            .cloned()
            .collect())
    }

    fn all(&self) -> Result<Vec<JournalEntry>, JournalError> {
        let state = self.state.lock().expect("journal mutex poisoned");
        Ok(state
            .order
            .iter()
            .filter_map(|id| state.entries.get(id))
            .cloned()
            .collect())
    }

    fn entry(&self, id: EntryId) -> Result<Option<JournalEntry>, JournalError> {
        let state = self.state.lock().expect("journal mutex poisoned");
        Ok(state.entries.get(&id).cloned())
    }

    fn mark_submitted(&self, id: EntryId) -> Result<(), JournalError> {
        let mut state = self.state.lock().expect("journal mutex poisoned");
        let Some(existing) = state.entries.get(&id) else {
            return Err(JournalError::NotFound(id));
        };
        // Idempotent: already terminal (submitted or rejected), nothing to
        // do — no error, no second log record, no state change. Never
        // overwrites an existing rejection with a later submission.
        if existing.is_terminal() {
            return Ok(());
        }

        let at = OffsetDateTime::now_utc();
        Self::write_record(&mut state.file, &LogRecord::Submitted { id, at })?;
        if let Some(entry) = state.entries.get_mut(&id) {
            entry.submitted_at = Some(at);
        }
        Ok(())
    }

    fn mark_rejected(&self, id: EntryId, reason: String) -> Result<(), JournalError> {
        let mut state = self.state.lock().expect("journal mutex poisoned");
        let Some(existing) = state.entries.get(&id) else {
            return Err(JournalError::NotFound(id));
        };
        // Idempotent, same posture as `mark_submitted` — including never
        // overwriting an existing submission with a later rejection.
        if existing.is_terminal() {
            return Ok(());
        }

        let at = OffsetDateTime::now_utc();
        Self::write_record(
            &mut state.file,
            &LogRecord::Rejected {
                id,
                at,
                reason: reason.clone(),
            },
        )?;
        if let Some(entry) = state.entries.get_mut(&id) {
            entry.rejected_at = Some(at);
            entry.rejection_reason = Some(reason);
        }
        Ok(())
    }

    fn mark_failed(&self, id: EntryId, reason: String) -> Result<(), JournalError> {
        let mut state = self.state.lock().expect("journal mutex poisoned");
        if !state.entries.contains_key(&id) {
            return Err(JournalError::NotFound(id));
        }
        Self::write_record(&mut state.file, &LogRecord::Failed { id, reason })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_journal_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "avalon-sync-journal-{label}-{}.jsonl",
            Uuid::new_v4()
        ))
    }

    #[test]
    fn two_appends_with_identical_payload_get_distinct_ids() {
        let path = temp_journal_path("dup-payload");
        let journal = FileJournal::open(&path).unwrap();

        let payload = serde_json::json!({ "achievement": "dragon_slayer" });
        let id_a = journal
            .append("achievement.earned".to_string(), payload.clone())
            .unwrap();
        let id_b = journal
            .append("achievement.earned".to_string(), payload.clone())
            .unwrap();

        assert_ne!(id_a, id_b, "the journal must not deduplicate by payload");

        let pending = journal.pending().unwrap();
        assert_eq!(
            pending.len(),
            2,
            "both entries must be recorded, not merged"
        );
        assert!(pending.iter().any(|e| e.id == id_a));
        assert!(pending.iter().any(|e| e.id == id_b));

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn mark_submitted_is_idempotent() {
        let path = temp_journal_path("idempotent-submit");
        let journal = FileJournal::open(&path).unwrap();

        let id = journal
            .append(
                "chat.message".to_string(),
                serde_json::json!({ "body": "hi" }),
            )
            .unwrap();

        journal.mark_submitted(id).unwrap();
        // Second call: must not error, must not corrupt state.
        journal.mark_submitted(id).unwrap();
        journal.mark_submitted(id).unwrap();

        assert!(
            journal.pending().unwrap().is_empty(),
            "a submitted entry must not still be pending"
        );

        // Reopening replays the log — a duplicate Submitted record (if we'd
        // written one on the second/third call) would still be harmless,
        // but we don't write one at all; confirm state survives intact
        // either way.
        drop(journal);
        let reopened = FileJournal::open(&path).unwrap();
        assert!(reopened.pending().unwrap().is_empty());

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn crash_recovery_pending_entries_survive_an_unclean_shutdown() {
        let path = temp_journal_path("crash-recovery");

        let (id_pending, id_submitted) = {
            let journal = FileJournal::open(&path).unwrap();
            let id_pending = journal
                .append(
                    "guild.join_request".to_string(),
                    serde_json::json!({ "guild": "The Round Table" }),
                )
                .unwrap();
            let id_submitted = journal
                .append(
                    "chat.message".to_string(),
                    serde_json::json!({ "body": "already sent before the crash" }),
                )
                .unwrap();
            journal.mark_submitted(id_submitted).unwrap();

            // Simulate a crash: `append`/`mark_submitted` already fsync'd
            // every write above, so this is the durable state a real
            // process death would leave on disk. There is no `close()` or
            // other shutdown method on `FileJournal` to call — dropping
            // `journal` here (end of scope, no flush/checkpoint logic
            // exists anywhere in this type) is exactly what happens when an
            // integrator process is killed: no extra cleanup ever runs.
            (id_pending, id_submitted)
        };

        // Reopen from the same path, as the next launch of the integrator would.
        let recovered = FileJournal::open(&path).unwrap();
        let pending = recovered.pending().unwrap();

        assert_eq!(
            pending.len(),
            1,
            "exactly the one never-submitted entry must survive"
        );
        assert_eq!(pending[0].id, id_pending);
        assert_eq!(pending[0].kind, "guild.join_request");
        assert_eq!(
            pending[0].payload,
            serde_json::json!({ "guild": "The Round Table" })
        );
        assert!(
            pending.iter().all(|e| e.id != id_submitted),
            "the already-submitted entry must not come back as pending"
        );

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn mark_failed_records_reason_without_removing_from_pending() {
        let path = temp_journal_path("mark-failed");
        let journal = FileJournal::open(&path).unwrap();

        let id = journal
            .append(
                "friend.request".to_string(),
                serde_json::json!({ "to": "player-2" }),
            )
            .unwrap();

        journal
            .mark_failed(id, "server rejected: rate limited".to_string())
            .unwrap();

        let pending = journal.pending().unwrap();
        assert_eq!(
            pending.len(),
            1,
            "a failed submission stays pending — retry policy is the submission engine's job"
        );
        assert_eq!(pending[0].id, id);

        let _ = fs::remove_file(&path);
    }

    /// Manually writes a valid `Append` record followed by a garbage
    /// partial line (no trailing newline) directly to the file, bypassing
    /// `FileJournal` entirely — this is what's on disk right after a
    /// process dies mid-`write_all`, before the interrupted write's
    /// newline (or any of it) ever landed.
    fn write_one_valid_entry_then_garbage_tail(path: &Path) -> Uuid {
        let id = Uuid::new_v4();
        let record = LogRecord::Append {
            id,
            kind: "guild.join_request".to_string(),
            payload: serde_json::json!({ "guild": "The Round Table" }),
            recorded_at: OffsetDateTime::now_utc(),
        };
        let mut line = serde_json::to_vec(&record).unwrap();
        line.push(b'\n');
        // Malformed, no trailing newline — an interrupted write caught
        // mid-flight, e.g. a truncated JSON object.
        line.extend_from_slice(br#"{"op":"append","id":"not-fini"#);

        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .unwrap();
        file.write_all(&line).unwrap();
        file.sync_all().unwrap();

        id
    }

    #[test]
    fn opening_with_a_truncated_trailing_line_recovers_prior_complete_entries() {
        // The narrower, read-only claim: a truncated tail doesn't lose
        // entries that were already durably recorded before it.
        let path = temp_journal_path("truncated-tail-readonly");
        let valid_id = write_one_valid_entry_then_garbage_tail(&path);

        let journal = FileJournal::open(&path).unwrap();
        let pending = journal.pending().unwrap();

        assert_eq!(pending.len(), 1, "the prior complete entry must survive");
        assert_eq!(pending[0].id, valid_id);
        assert_eq!(pending[0].kind, "guild.join_request");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn opening_with_a_truncated_trailing_line_then_writing_again_does_not_corrupt_future_appends() {
        // The critical regression case: after `open()` recovers from a
        // truncated tail, a *subsequent* append must not glue itself onto
        // the leftover garbage bytes. Without truncating the file back to
        // the last known-good record on open, `append()` below would
        // report `Ok(id)` (its own fsync succeeds) while silently writing
        // an unparseable line — and the entry would vanish on the next
        // reopen even though the caller was told it was durable.
        let path = temp_journal_path("truncated-tail-then-append");
        let valid_id = write_one_valid_entry_then_garbage_tail(&path);

        let new_id = {
            let journal = FileJournal::open(&path).unwrap();
            journal
                .append(
                    "chat.message".to_string(),
                    serde_json::json!({ "body": "written after recovery" }),
                )
                .unwrap()
        };

        // Reopen again, as the next launch would — this is the step that
        // exposes the bug: without the fix, replay hits the glued
        // garbage+new-record line, fails to parse it, and drops it.
        let reopened = FileJournal::open(&path).unwrap();
        let pending = reopened.pending().unwrap();

        assert_eq!(
            pending.len(),
            2,
            "both the recovered entry and the newly-appended one must survive a reopen"
        );
        assert!(
            pending.iter().any(|e| e.id == valid_id),
            "the entry recovered from before the crash must still be present"
        );
        assert!(
            pending.iter().any(|e| e.id == new_id),
            "the entry appended after recovery must not be silently lost"
        );

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn mark_submitted_on_unknown_id_errors() {
        let path = temp_journal_path("unknown-id");
        let journal = FileJournal::open(&path).unwrap();

        let err = journal.mark_submitted(Uuid::new_v4()).unwrap_err();
        assert!(matches!(err, JournalError::NotFound(_)));

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn status_of_reflects_pending_submitted_and_rejected() {
        let path = temp_journal_path("status-of");
        let journal = FileJournal::open(&path).unwrap();

        let pending_id = journal
            .append("chat.message".to_string(), serde_json::json!({}))
            .unwrap();
        let submitted_id = journal
            .append("chat.message".to_string(), serde_json::json!({}))
            .unwrap();
        let rejected_id = journal
            .append("chat.message".to_string(), serde_json::json!({}))
            .unwrap();

        journal.mark_submitted(submitted_id).unwrap();
        journal
            .mark_rejected(rejected_id, "conversation is gone".to_string())
            .unwrap();

        assert_eq!(journal.status_of(pending_id).unwrap(), EntryStatus::Pending);
        assert_eq!(
            journal.status_of(submitted_id).unwrap(),
            EntryStatus::Submitted
        );
        assert_eq!(
            journal.status_of(rejected_id).unwrap(),
            EntryStatus::Rejected {
                reason: "conversation is gone".to_string()
            }
        );

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn status_of_unknown_id_errors() {
        let path = temp_journal_path("status-of-unknown");
        let journal = FileJournal::open(&path).unwrap();

        let err = journal.status_of(Uuid::new_v4()).unwrap_err();
        assert!(matches!(err, JournalError::NotFound(_)));

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn status_counts_pending_and_tracks_oldest_pending_and_last_synced() {
        let path = temp_journal_path("status-summary");
        let journal = FileJournal::open(&path).unwrap();

        let first_pending = journal
            .append("chat.message".to_string(), serde_json::json!({}))
            .unwrap();
        let submitted = journal
            .append("chat.message".to_string(), serde_json::json!({}))
            .unwrap();
        let _second_pending = journal
            .append("chat.message".to_string(), serde_json::json!({}))
            .unwrap();
        journal.mark_submitted(submitted).unwrap();

        let status = journal.status().unwrap();
        assert_eq!(status.pending_count, 2);
        let first_pending_recorded_at = journal.entry(first_pending).unwrap().unwrap().recorded_at;
        assert_eq!(status.oldest_pending_at, Some(first_pending_recorded_at));
        assert!(status.last_synced_at.is_some());

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn status_with_nothing_recorded_is_all_zero() {
        let path = temp_journal_path("status-empty");
        let journal = FileJournal::open(&path).unwrap();

        let status = journal.status().unwrap();
        assert_eq!(status.pending_count, 0);
        assert_eq!(status.oldest_pending_at, None);
        assert_eq!(status.last_synced_at, None);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn mark_rejected_is_idempotent_and_removes_the_entry_from_pending() {
        let path = temp_journal_path("mark-rejected-idempotent");
        let journal = FileJournal::open(&path).unwrap();

        let id = journal
            .append("chat.message".to_string(), serde_json::json!({}))
            .unwrap();
        journal
            .mark_rejected(id, "first reason".to_string())
            .unwrap();
        // Second call: must not error, must not overwrite the first reason.
        journal
            .mark_rejected(id, "second reason".to_string())
            .unwrap();

        assert!(journal.pending().unwrap().is_empty());
        assert_eq!(
            journal.status_of(id).unwrap(),
            EntryStatus::Rejected {
                reason: "first reason".to_string()
            }
        );

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn mark_rejected_never_overwrites_an_existing_submission() {
        let path = temp_journal_path("mark-rejected-after-submitted");
        let journal = FileJournal::open(&path).unwrap();

        let id = journal
            .append("chat.message".to_string(), serde_json::json!({}))
            .unwrap();
        journal.mark_submitted(id).unwrap();
        journal.mark_rejected(id, "too late".to_string()).unwrap();

        assert_eq!(journal.status_of(id).unwrap(), EntryStatus::Submitted);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn all_returns_both_pending_and_terminal_entries() {
        let path = temp_journal_path("all-entries");
        let journal = FileJournal::open(&path).unwrap();

        let pending_id = journal
            .append("chat.message".to_string(), serde_json::json!({}))
            .unwrap();
        let submitted_id = journal
            .append("chat.message".to_string(), serde_json::json!({}))
            .unwrap();
        journal.mark_submitted(submitted_id).unwrap();

        let all = journal.all().unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|e| e.id == pending_id));
        assert!(all.iter().any(|e| e.id == submitted_id));

        let _ = fs::remove_file(&path);
    }
}
