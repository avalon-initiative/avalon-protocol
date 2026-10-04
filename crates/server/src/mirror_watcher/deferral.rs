//! Scan scheduling and retry state for mirrored entries whose projection is waiting.
//!
//! One mutex guards everything, so the clean-scan flags and the waiting entries can never be
//! taken in conflicting orders. Every public function takes the lock once and returns.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use uuid::Uuid;

/// Most entries of one shard tracked as waiting on missing state. Entries past it are left
/// untouched until older ones resolve, so a shard cannot make the scan costly.
pub(super) const MAX_DEFERRED_PER_SHARD: usize = 256;
const BACKOFF_START: Duration = Duration::from_secs(30);
const BACKOFF_MAX: Duration = Duration::from_secs(600);
/// Waiting entries that are not about the identity just projected are re-armed at most this often
/// per shard.
const REARM_MIN_INTERVAL: Duration = Duration::from_secs(5);

struct Waiting {
    attempts: u32,
    next_retry: Instant,
    /// Later entries of the shard must wait behind this one.
    ordered: bool,
    /// The issuer identity of the entry, which is the identity whose key it usually waits for.
    identity: Option<String>,
    /// When the entry was first tracked.
    since: Instant,
}

#[derive(Default)]
struct ShardBook {
    waiting: HashMap<Uuid, Waiting>,
    last_rearm: Option<Instant>,
}

/// Which `(network, shard)` pairs need a scan, and the entries waiting per pair. This type never
/// expires a wait: an entry is retried with a growing delay, and sooner when the identity or key
/// it may be waiting for is projected. The caller ends a wait by its stored age and `forget`s it.
#[derive(Default)]
pub(super) struct Scheduler {
    clean: HashSet<(String, String)>,
    /// Pairs whose rows changed behind a running scan; its end must not mark them clean.
    rescan: HashSet<(String, String)>,
    shards: HashMap<(String, String), ShardBook>,
}

fn pair(network_id: &str, shard_id: &str) -> (String, String) {
    (network_id.to_string(), shard_id.to_string())
}

impl Scheduler {
    pub(super) fn scan_due(&self, network_id: &str, shard_id: &str, now: Instant) -> bool {
        let key = pair(network_id, shard_id);
        !self.clean.contains(&key)
            || self
                .shards
                .get(&key)
                .is_some_and(|b| b.waiting.values().any(|w| w.next_retry <= now))
    }

    pub(super) fn mark_clean(&mut self, network_id: &str, shard_id: &str) {
        let key = pair(network_id, shard_id);
        if !self.rescan.remove(&key) {
            self.clean.insert(key);
        }
    }

    /// Rows of the shard became projectable again (a refused entry was reopened): scan it again,
    /// even if a scan already running finishes first.
    pub(super) fn request_rescan(&mut self, network_id: &str, shard_id: &str) {
        let key = pair(network_id, shard_id);
        self.clean.remove(&key);
        self.rescan.insert(key);
    }

    /// Drops waiting entries first tracked before `scanned_from` that `seen` does not contain.
    /// After a complete scan these are entries whose stored row is gone (rolled back, pruned).
    pub(super) fn prune_unseen(
        &mut self,
        network_id: &str,
        shard_id: &str,
        seen: &HashSet<Uuid>,
        scanned_from: Instant,
    ) -> usize {
        let key = pair(network_id, shard_id);
        let Some(book) = self.shards.get_mut(&key) else {
            return 0;
        };
        let before = book.waiting.len();
        book.waiting
            .retain(|id, w| seen.contains(id) || w.since >= scanned_from);
        let pruned = before - book.waiting.len();
        if book.waiting.is_empty() {
            self.shards.remove(&key);
        }
        pruned
    }

    pub(super) fn mark_failed(&mut self, network_id: &str, shard_id: &str) {
        self.clean.remove(&pair(network_id, shard_id));
    }

    #[cfg(test)]
    pub(super) fn len(&self, network_id: &str, shard_id: &str) -> usize {
        self.shards
            .get(&pair(network_id, shard_id))
            .map_or(0, |b| b.waiting.len())
    }

    /// Records one more deferral; `false` when the shard is at its cap and the entry is left
    /// untracked.
    pub(super) fn note(
        &mut self,
        network_id: &str,
        shard_id: &str,
        event_id: Uuid,
        ordered: bool,
        identity: Option<String>,
        now: Instant,
    ) -> bool {
        let book = self.shards.entry(pair(network_id, shard_id)).or_default();
        if !book.waiting.contains_key(&event_id) && book.waiting.len() >= MAX_DEFERRED_PER_SHARD {
            return false;
        }
        let waiting = book.waiting.entry(event_id).or_insert_with(|| Waiting {
            attempts: 0,
            next_retry: now,
            ordered,
            identity: identity.clone(),
            since: now,
        });
        waiting.attempts += 1;
        waiting.next_retry = now
            + BACKOFF_START
                .saturating_mul(1u32 << (waiting.attempts - 1).min(10))
                .min(BACKOFF_MAX);
        waiting.ordered = ordered;
        waiting.identity = identity;
        true
    }

    /// `Some(ordered)` while the entry waits for its retry time, `None` when it may be tried.
    pub(super) fn holding(
        &self,
        network_id: &str,
        shard_id: &str,
        event_id: Uuid,
        now: Instant,
    ) -> Option<bool> {
        let waiting = self
            .shards
            .get(&pair(network_id, shard_id))?
            .waiting
            .get(&event_id)?;
        (waiting.next_retry > now).then_some(waiting.ordered)
    }

    /// The entry reached a final outcome: it no longer waits. Returns whether it was waiting.
    pub(super) fn forget(&mut self, network_id: &str, shard_id: &str, event_id: Uuid) -> bool {
        let key = pair(network_id, shard_id);
        let Some(book) = self.shards.get_mut(&key) else {
            return false;
        };
        let removed = book.waiting.remove(&event_id).is_some();
        if book.waiting.is_empty() {
            self.shards.remove(&key);
        }
        removed
    }

    /// An entry was projected. If it had been waiting, its shard is scanned again (entries past
    /// the cap may fit now). If it changed identity or key state, waiting entries about the same
    /// identity are retried now, and the others at most once per [`REARM_MIN_INTERVAL`] per shard.
    pub(super) fn applied(
        &mut self,
        network_id: &str,
        shard_id: &str,
        event_id: Uuid,
        changes_keys: bool,
        identity: Option<&str>,
        now: Instant,
    ) {
        if self.forget(network_id, shard_id, event_id) {
            self.clean.remove(&pair(network_id, shard_id));
        }
        if !changes_keys {
            return;
        }
        for (key, book) in &mut self.shards {
            if key.0 != network_id {
                continue;
            }
            let rate_ok = book
                .last_rearm
                .is_none_or(|at| now.duration_since(at) >= REARM_MIN_INTERVAL);
            let mut armed = false;
            let mut spent_rate = false;
            for waiting in book.waiting.values_mut() {
                let same = identity.is_some() && waiting.identity.as_deref() == identity;
                if same || rate_ok {
                    waiting.next_retry = now;
                    armed = true;
                    spent_rate |= !same;
                }
            }
            if spent_rate {
                book.last_rearm = Some(now);
            }
            if armed {
                self.clean.remove(key);
            }
        }
    }
}

static SCHEDULER: LazyLock<Mutex<Scheduler>> = LazyLock::new(Default::default);

fn with<T>(f: impl FnOnce(&mut Scheduler) -> T) -> T {
    f(&mut SCHEDULER.lock().unwrap_or_else(|e| e.into_inner()))
}

pub(super) fn scan_due(network_id: &str, shard_id: &str) -> bool {
    with(|s| s.scan_due(network_id, shard_id, Instant::now()))
}

pub(super) fn mark_scan_clean(network_id: &str, shard_id: &str) {
    with(|s| s.mark_clean(network_id, shard_id));
}

pub(super) fn mark_projection_failed(network_id: &str, shard_id: &str) {
    with(|s| s.mark_failed(network_id, shard_id));
}

pub(super) fn request_rescan(network_id: &str, shard_id: &str) {
    with(|s| s.request_rescan(network_id, shard_id));
}

pub(super) fn prune_unseen(
    network_id: &str,
    shard_id: &str,
    seen: &HashSet<Uuid>,
    scanned_from: Instant,
) -> usize {
    with(|s| s.prune_unseen(network_id, shard_id, seen, scanned_from))
}

pub(super) fn note_deferral(
    network_id: &str,
    shard_id: &str,
    event_id: Uuid,
    ordered: bool,
    identity: Option<String>,
) {
    with(|s| {
        s.note(
            network_id,
            shard_id,
            event_id,
            ordered,
            identity,
            Instant::now(),
        )
    });
}

pub(super) fn holding(network_id: &str, shard_id: &str, event_id: Uuid) -> Option<bool> {
    with(|s| s.holding(network_id, shard_id, event_id, Instant::now()))
}

pub(super) fn forget_waiting(network_id: &str, shard_id: &str, event_id: Uuid) {
    with(|s| s.forget(network_id, shard_id, event_id));
}

/// Whether projecting an entry of this kind can unblock entries that wait for an identity or key.
pub(super) fn changes_keys(kind: &str) -> bool {
    kind == "identity.created" || kind.starts_with("identity.signing_key_")
}

pub(super) fn entry_applied(
    network_id: &str,
    shard_id: &str,
    event_id: Uuid,
    kind: &str,
    identity: Option<&str>,
) {
    let changes_keys = changes_keys(kind);
    with(|s| {
        s.applied(
            network_id,
            shard_id,
            event_id,
            changes_keys,
            identity,
            Instant::now(),
        )
    });
}

#[cfg(test)]
pub(super) fn waiting_len(network_id: &str, shard_id: &str) -> usize {
    with(|s| s.len(network_id, shard_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(s: &mut Scheduler, shard: &str, id: Uuid, now: Instant) -> bool {
        s.note("net", shard, id, false, None, now)
    }

    #[test]
    fn deferral_state_is_keyed_by_network_shard_and_event_id() {
        let now = Instant::now();
        let mut s = Scheduler::default();
        let id = Uuid::new_v4();
        for _ in 0..8 {
            assert!(note(&mut s, "game:evil/1", id, now));
        }
        assert!(note(&mut s, "core", id, now));
        assert!(s.holding("other", "core", id, now).is_none());
        // The honest copy starts from its own first attempt, not the hostile one's backoff.
        assert_eq!(s.holding("net", "core", id, now), Some(false));
        let honest = &s.shards[&pair("net", "core")].waiting[&id];
        assert_eq!(
            (honest.attempts, honest.next_retry),
            (1, now + BACKOFF_START)
        );
        // Another network's shard of the same name shares nothing either.
        assert!(s.note("other", "core", id, false, None, now));
        assert!(s.forget("net", "core", id));
        assert!(s.holding("other", "core", id, now).is_some());
        assert!(s.holding("net", "game:evil/1", id, now).is_some());
        assert!(!s.forget("net", "core", id));
    }

    #[test]
    fn waits_back_off_without_expiring_and_an_applied_key_event_makes_them_due() {
        let now = Instant::now();
        let mut s = Scheduler::default();
        let (id, other) = (Uuid::new_v4(), Uuid::new_v4());
        let mut last = Duration::ZERO;
        for _ in 0..40 {
            note(&mut s, "core", id, now);
            let delay = s.shards[&pair("net", "core")].waiting[&id].next_retry - now;
            assert!(delay >= last && delay <= BACKOFF_MAX);
            last = delay;
        }
        assert_eq!(last, BACKOFF_MAX);
        s.mark_clean("net", "core");
        assert!(!s.scan_due("net", "core", now));
        s.applied("net", "game:y/1", other, true, None, now);
        assert!(s.holding("net", "core", id, now).is_none());
        assert!(s.scan_due("net", "core", now));
    }

    #[test]
    fn a_shard_at_its_cap_leaves_its_newest_entries_untracked() {
        let now = Instant::now();
        let mut s = Scheduler::default();
        let ids: Vec<Uuid> = (0..=MAX_DEFERRED_PER_SHARD)
            .map(|_| Uuid::new_v4())
            .collect();
        for id in &ids[..MAX_DEFERRED_PER_SHARD] {
            assert!(note(&mut s, "game:x/1", *id, now));
        }
        let newest = ids[MAX_DEFERRED_PER_SHARD];
        assert!(!note(&mut s, "game:x/1", newest, now));
        assert!(s.holding("net", "game:x/1", newest, now).is_none());
        assert!(note(&mut s, "core", newest, now));
        assert!(note(&mut s, "game:x/1", ids[0], now));
        assert_eq!(s.len("net", "game:x/1"), MAX_DEFERRED_PER_SHARD);
        assert!(s.forget("net", "game:x/1", ids[1]));
        assert!(note(&mut s, "game:x/1", newest, now));
    }

    #[test]
    fn a_final_outcome_frees_the_slot_and_stops_the_scan_firing() {
        let now = Instant::now();
        let mut s = Scheduler::default();
        let id = Uuid::new_v4();
        note(&mut s, "core", id, now);
        s.mark_clean("net", "core");
        let later = now + BACKOFF_MAX + Duration::from_secs(1);
        assert!(
            s.scan_due("net", "core", later),
            "a waiting entry comes due"
        );
        assert!(s.forget("net", "core", id));
        assert!(!s.scan_due("net", "core", later));
        assert_eq!(s.len("net", "core"), 0);
        assert!(s.shards.is_empty());
    }

    #[test]
    fn rearming_targets_the_applied_identity_and_rate_limits_the_rest() {
        let now = Instant::now();
        let mut s = Scheduler::default();
        let (mine, theirs, applied) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        s.note("net", "core", mine, false, Some("a".into()), now);
        s.note("net", "core", theirs, false, Some("b".into()), now);
        // The first key event re-arms the matching entry and, as the rate allows, the other.
        s.applied("net", "g/1", applied, true, Some("a"), now);
        assert!(s.holding("net", "core", mine, now).is_none());
        assert!(s.holding("net", "core", theirs, now).is_none());
        // Within the interval only the matching identity is re-armed again.
        s.note("net", "core", mine, false, Some("a".into()), now);
        s.note("net", "core", theirs, false, Some("b".into()), now);
        s.applied(
            "net",
            "g/1",
            applied,
            true,
            Some("a"),
            now + Duration::from_secs(1),
        );
        assert!(s
            .holding("net", "core", mine, now + Duration::from_secs(1))
            .is_none());
        assert!(s
            .holding("net", "core", theirs, now + Duration::from_secs(1))
            .is_some());
        // After the interval the others are re-armed too.
        s.applied(
            "net",
            "g/1",
            applied,
            true,
            Some("a"),
            now + Duration::from_secs(6),
        );
        assert!(s
            .holding("net", "core", theirs, now + Duration::from_secs(6))
            .is_none());
        // A projected entry that changes no keys re-arms nothing.
        s.note("net", "core", theirs, false, Some("b".into()), now);
        s.applied(
            "net",
            "g/1",
            applied,
            false,
            Some("b"),
            now + Duration::from_secs(20),
        );
        assert!(s
            .holding("net", "core", theirs, now + Duration::from_secs(20))
            .is_some());
    }

    #[test]
    fn rearming_is_scoped_to_the_network_of_the_applied_entry() {
        let now = Instant::now();
        let mut s = Scheduler::default();
        let (mine, foreign, applied) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        s.note("net", "core", mine, false, Some("a".into()), now);
        s.note("other", "core", foreign, false, Some("a".into()), now);
        s.applied("net", "g/1", applied, true, Some("a"), now);
        assert!(s.holding("net", "core", mine, now).is_none());
        assert!(s.holding("other", "core", foreign, now).is_some());
    }

    #[test]
    fn a_scan_drops_waits_it_did_not_see_but_keeps_newer_ones() {
        let start = Instant::now();
        let mut s = Scheduler::default();
        let (seen, gone, newer) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        note(&mut s, "core", seen, start);
        note(&mut s, "core", gone, start);
        note(&mut s, "core", newer, start + Duration::from_secs(1));
        let pruned = s.prune_unseen(
            "net",
            "core",
            &HashSet::from([seen]),
            start + Duration::from_secs(1),
        );
        assert_eq!(pruned, 1);
        assert!(s.holding("net", "core", gone, start).is_none());
        assert!(s.holding("net", "core", seen, start).is_some());
        assert!(s.holding("net", "core", newer, start).is_some());
    }

    #[test]
    fn a_rescan_request_survives_the_end_of_a_running_scan() {
        let mut s = Scheduler::default();
        s.mark_clean("net", "core");
        assert!(!s.scan_due("net", "core", Instant::now()));
        s.request_rescan("net", "core");
        s.mark_clean("net", "core");
        assert!(s.scan_due("net", "core", Instant::now()));
        s.mark_clean("net", "core");
        assert!(!s.scan_due("net", "core", Instant::now()));
    }
}
