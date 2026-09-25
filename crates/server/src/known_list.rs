//! Witness known-list management — issue #946, the production promotion of
//! `avalon_protocol::known_list`'s design-spike prototype (anchor
//! reservation + per-prefix diversity cap): persistence across restarts,
//! real discovery integration, tenure-weighted refill and a probation
//! period before a newly-admitted slot counts toward the list. See
//! `docs/projects/backend-server/architecture/witness-cosigning.md`'s "The known list" section.
//!
//! **Boundary with `crate::nodes::PeerTable`.** The peer table is this
//! node's general, unbounded "who do I gossip/announce with" address book
//! and is deliberately never persisted (`crate::nodes`'s own module doc
//! comment — a node just re-announces after a restart). The known list is a
//! much smaller (5-slot by default), purpose-specific structure — which witnesses
//! this node currently trusts for cosigning-majority computation — and
//! *must* survive a restart, or a well-timed restart hands an attacker a
//! fresh eclipse attempt every time. This module never writes into
//! `PeerTable`; it only reads it (`PeerTable::list_all`) as a source of
//! refill candidates, and reuses `crate::outbound_policy::OutboundPolicy`
//! for address resolution rather than re-deriving IP prefixes by hand.
//!
//! **Slot identity is the witness's cosigning key.** A candidate's
//! `witness_key_id` is the hex Ed25519 verifying key the peer advertised in
//! `POST /nodes/announce` or gossip, admitted only with a verified proof of
//! possession (`crate::nodes::WitnessAdvert`). A peer advertising no proven
//! key stays in the peer table but is never a candidate.
//!
//! **Scope note on "misbehaving."** Automatic replacement here is driven
//! entirely by the freshness window (a slot not observed recently is
//! stale) — connectivity, not cryptographic equivocation detection, which
//! is #932/#947's job.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::nodes::PeerTable;
use crate::outbound_policy::OutboundPolicy;

/// Default known-list capacity (`Y` in the design doc).
const DEFAULT_CAPACITY: usize = 5;
/// Default reserved anchor slots (2 of `Y`).
const DEFAULT_ANCHOR_CAPACITY: usize = 2;
/// Default per-prefix diversity cap, applied to anchors too.
const DEFAULT_MAX_PER_PREFIX: usize = 2;
/// Default lower bound on the freshness scale factor, see
/// [`KnownListInner::effective_freshness_window`].
const DEFAULT_FRESHNESS_FLOOR: f64 = 0.2;
/// Default freshness window: a slot unobserved this long is stale and
/// eligible for replacement — same 10-minute default the design doc gives.
const DEFAULT_FRESHNESS_SECS: u64 = 10 * 60;
/// Default probation window: how long a newly-admitted, non-anchor slot
/// must be continuously observed before it counts toward the list.
/// Not specified by the design doc (left to this ticket) — chosen as 3x
/// the freshness window: long enough that a burst of freshly-registered
/// Sybil candidates timed right before an attack can't buy majority
/// weight in one refill cycle, short enough that a genuine new witness
/// isn't kept out for an unreasonable stretch. Revisit alongside #932/#947
/// once real cosigning traffic gives an empirical basis for this number.
const DEFAULT_PROBATION_SECS: u64 = 30 * 60;
/// Default interval between refill ticks — independent of, and slower
/// than, the announce worker's own cadence: this is membership maintenance
/// for a small list, not peer discovery itself.
const DEFAULT_REFILL_INTERVAL_SECS: u64 = 120;

/// Whether a slot's occupant already counts toward this list's majority
/// computation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotStatus {
    /// Admitted, but not yet continuously observed for the probation
    /// window — present in the list, but excluded from
    /// [`KnownListInner::confirmed_witness_key_ids`].
    Probationary,
    /// Past probation (or an anchor, admitted confirmed outright — see
    /// [`KnownListInner::try_admit`]) and counted toward majority.
    Confirmed,
}

/// One slot's occupant, with enough bookkeeping to enforce diversity,
/// freshness and probation, and to round-trip through persistence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownSlot {
    pub witness_key_id: String,
    /// Coarse diversity key — an IPv4 /24, an IPv6 /64, or (for a bundled
    /// anchor with no resolvable address at refill time) the anchor's own
    /// seed-node URL. See [`diversity_prefix`].
    pub prefix: String,
    pub is_anchor: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub admitted_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub last_observed_at: OffsetDateTime,
    pub status: SlotStatus,
}

/// A refill candidate: a peer (or bundled anchor) not yet in the list,
/// offered to [`KnownListInner::refill`] this tick.
#[derive(Debug, Clone)]
pub struct DiscoveredCandidate {
    pub witness_key_id: String,
    pub prefix: String,
    pub is_anchor: bool,
}

/// Tuning knobs — resolved once from the environment at startup
/// ([`KnownListConfig::from_env`]) and never persisted: only slot
/// membership survives a restart, not these parameters, so a config change
/// takes effect on the very next boot rather than being pinned by a stale
/// file.
#[derive(Debug, Clone, Copy)]
pub struct KnownListConfig {
    pub capacity: usize,
    pub anchor_capacity: usize,
    pub max_per_prefix: usize,
    pub freshness_window: Duration,
    pub probation_window: Duration,
    /// Smallest fraction of `freshness_window` a slot may be held to when the
    /// list's failure tolerance is low; in `(0, 1]`.
    pub freshness_floor: f64,
}

impl Default for KnownListConfig {
    fn default() -> Self {
        Self {
            capacity: DEFAULT_CAPACITY,
            anchor_capacity: DEFAULT_ANCHOR_CAPACITY,
            max_per_prefix: DEFAULT_MAX_PER_PREFIX,
            freshness_window: Duration::from_secs(DEFAULT_FRESHNESS_SECS),
            probation_window: Duration::from_secs(DEFAULT_PROBATION_SECS),
            freshness_floor: DEFAULT_FRESHNESS_FLOOR,
        }
    }
}

fn env_usize(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

fn env_secs(var: &str, default: u64) -> Duration {
    Duration::from_secs(
        std::env::var(var)
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(default),
    )
}

fn env_fraction(var: &str, default: f64) -> f64 {
    std::env::var(var)
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|f| *f > 0.0 && *f <= 1.0)
        .unwrap_or(default)
}

impl KnownListConfig {
    /// `AVALON_KNOWN_LIST_CAPACITY` / `_ANCHOR_CAPACITY` / `_MAX_PER_PREFIX`
    /// / `_FRESHNESS_SECS` / `_PROBATION_SECS` / `_FRESHNESS_FLOOR`, each
    /// defaulting as above. The anchor count is clamped to the capacity.
    pub fn from_env() -> Self {
        let capacity = env_usize("AVALON_KNOWN_LIST_CAPACITY", DEFAULT_CAPACITY);
        Self {
            capacity,
            anchor_capacity: env_usize(
                "AVALON_KNOWN_LIST_ANCHOR_CAPACITY",
                DEFAULT_ANCHOR_CAPACITY,
            )
            .min(capacity),
            max_per_prefix: env_usize("AVALON_KNOWN_LIST_MAX_PER_PREFIX", DEFAULT_MAX_PER_PREFIX),
            freshness_window: env_secs("AVALON_KNOWN_LIST_FRESHNESS_SECS", DEFAULT_FRESHNESS_SECS),
            probation_window: env_secs("AVALON_KNOWN_LIST_PROBATION_SECS", DEFAULT_PROBATION_SECS),
            freshness_floor: env_fraction(
                "AVALON_KNOWN_LIST_FRESHNESS_FLOOR",
                DEFAULT_FRESHNESS_FLOOR,
            ),
        }
    }
}

/// `AVALON_KNOWN_LIST_REFILL_INTERVAL_SECS`, default 120.
pub fn refill_interval_from_env() -> Duration {
    env_secs(
        "AVALON_KNOWN_LIST_REFILL_INTERVAL_SECS",
        DEFAULT_REFILL_INTERVAL_SECS,
    )
}

/// `AVALON_DATA_DIR` — this node's local, non-secret persistent-state
/// directory. No such convention existed in this crate before this ticket;
/// `sth.rs`'s signing keys stay environment-loaded and are never written
/// here (see that module's own doc comment) — this directory is for
/// node-local state that isn't secret material, starting with the known
/// list. Defaults to `./data`, relative to the process's working
/// directory, matching how `AVALON_BOOTSTRAP_PEERS`/friends default to
/// relative, deployment-chosen paths elsewhere in this crate.
pub fn data_dir_from_env() -> PathBuf {
    std::env::var("AVALON_DATA_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data"))
}

/// The known list's own persistence file within a data directory.
pub fn known_list_path(data_dir: &Path) -> PathBuf {
    data_dir.join("known_list.json")
}

/// Bumped whenever slot identity semantics change; files of any other
/// version are discarded on load.
const PERSISTED_FORMAT_VERSION: u32 = 2;

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedKnownList {
    #[serde(default)]
    version: u32,
    slots: Vec<KnownSlot>,
}

/// A coarse diversity key for `ip` — an IPv4 /24 or an IPv6 /64, matching
/// the granularity `docs/projects/backend-server/architecture/witness-cosigning.md` describes.
/// `to_canonical()` first, so an IPv4-mapped IPv6 address is judged as its
/// embedded IPv4 /24, the same normalization `crate::outbound_policy`
/// already applies before any policy check.
pub fn diversity_prefix(ip: IpAddr) -> String {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            format!("v4:{}.{}.{}.0/24", o[0], o[1], o[2])
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            format!("v6:{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
    }
}

/// Plain, non-thread-safe list logic — mirrors
/// `avalon_protocol::known_list::KnownList`'s admission rules (contains /
/// anchor-cap / capacity / diversity-cap, in that order) but is kept as a
/// separate type in this crate rather than literally reused, since every
/// slot here also carries tenure/probation/freshness state the pure
/// design-spike prototype has no use for. The admission *rule* itself is
/// duplicated on purpose — kept in sync by this module's own tests, which
/// mirror the prototype's eclipse-resistance test directly.
#[derive(Debug, Clone)]
pub struct KnownListInner {
    capacity: usize,
    anchor_capacity: usize,
    max_per_prefix: usize,
    freshness_window: Duration,
    probation_window: Duration,
    freshness_floor: f64,
    slots: Vec<KnownSlot>,
    /// First-observed time per not-yet-admitted candidate — the tenure
    /// signal [`Self::refill`] ranks candidates by. Deliberately owned
    /// here rather than read off `PeerTable` (whose `PeerInfo` tracks only
    /// `last_announced_at`, not a first-seen time) — see this module's own
    /// doc comment on the `PeerTable` boundary. Forgotten for a candidate
    /// once it's no longer offered, so tenure can't be claimed for a peer
    /// that dropped out and later reappeared.
    candidate_first_seen: HashMap<String, OffsetDateTime>,
}

impl KnownListInner {
    pub fn new(cfg: KnownListConfig) -> Self {
        Self {
            capacity: cfg.capacity,
            anchor_capacity: cfg.anchor_capacity.min(cfg.capacity),
            max_per_prefix: cfg.max_per_prefix,
            freshness_window: cfg.freshness_window,
            probation_window: cfg.probation_window,
            freshness_floor: if cfg.freshness_floor > 0.0 && cfg.freshness_floor <= 1.0 {
                cfg.freshness_floor
            } else {
                DEFAULT_FRESHNESS_FLOOR
            },
            slots: Vec::new(),
            candidate_first_seen: HashMap::new(),
        }
    }

    fn from_persisted(cfg: KnownListConfig, slots: Vec<KnownSlot>) -> Self {
        let mut list = Self::new(cfg);
        list.slots = slots;
        list
    }

    fn anchor_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_anchor).count()
    }

    fn prefix_count(&self, prefix: &str) -> usize {
        self.slots.iter().filter(|s| s.prefix == prefix).count()
    }

    pub fn contains(&self, witness_key_id: &str) -> bool {
        self.slots
            .iter()
            .any(|s| s.witness_key_id == witness_key_id)
    }

    /// Same refusal order as the prototype's `try_admit`: already present,
    /// anchor cap, list capacity, diversity cap (checked last so it applies
    /// to anchors too). An admitted anchor starts [`SlotStatus::Confirmed`]
    /// outright — bundled anchors are PR-reviewed and long-lived by
    /// convention (design doc), so there's no burst-Sybil risk to guard
    /// against by probating them; a non-anchor always starts
    /// [`SlotStatus::Probationary`].
    pub fn try_admit(
        &mut self,
        witness_key_id: String,
        prefix: String,
        is_anchor: bool,
        now: OffsetDateTime,
    ) -> bool {
        if self.contains(&witness_key_id) {
            return false;
        }
        if is_anchor && self.anchor_count() >= self.anchor_capacity {
            return false;
        }
        if self.slots.len() >= self.capacity {
            return false;
        }
        if self.prefix_count(&prefix) >= self.max_per_prefix {
            return false;
        }
        self.slots.push(KnownSlot {
            witness_key_id,
            prefix,
            is_anchor,
            admitted_at: now,
            last_observed_at: now,
            status: if is_anchor {
                SlotStatus::Confirmed
            } else {
                SlotStatus::Probationary
            },
        });
        true
    }

    /// Removes a slot outright — an anchor can be removed too, the same
    /// way the prototype's `remove` allows it (see that type's own doc
    /// comment): an anchor going stale is handled by ordinary refill, not
    /// treated as a special failure.
    pub fn remove(&mut self, witness_key_id: &str) -> bool {
        let before = self.slots.len();
        self.slots.retain(|s| s.witness_key_id != witness_key_id);
        self.candidate_first_seen.remove(witness_key_id);
        self.slots.len() != before
    }

    /// Refreshes an already-admitted slot's `last_observed_at`. Returns
    /// `false` when `witness_key_id` isn't currently a slot.
    pub fn observe(&mut self, witness_key_id: &str, now: OffsetDateTime) -> bool {
        if let Some(slot) = self
            .slots
            .iter_mut()
            .find(|s| s.witness_key_id == witness_key_id)
        {
            slot.last_observed_at = now;
            true
        } else {
            false
        }
    }

    /// Promotes every `Probationary` slot that has both cleared the
    /// probation window since admission *and* is currently fresh (observed
    /// within the freshness window) — a slot that went stale during its own
    /// probation stays probationary rather than being confirmed on a
    /// technicality; [`Self::prune_stale`] will remove it if it stays dark.
    /// Returns the promoted witness key ids.
    pub fn confirm_due(&mut self, now: OffsetDateTime) -> Vec<String> {
        let mut promoted = Vec::new();
        for slot in self.slots.iter_mut() {
            if slot.status == SlotStatus::Probationary
                && now - slot.admitted_at >= self.probation_window
                && now - slot.last_observed_at <= self.freshness_window
            {
                slot.status = SlotStatus::Confirmed;
                promoted.push(slot.witness_key_id.clone());
            }
        }
        promoted
    }

    /// Failures a list of `n` confirmed witnesses tolerates while a strict
    /// majority of `n` can still be reached.
    fn tolerance(n: usize) -> usize {
        n.saturating_sub(avalon_protocol::witness::majority_threshold(n))
    }

    /// The freshness window applied to confirmed slots: the base window scaled
    /// by `max(floor, tolerance(n) / tolerance(capacity))`. Lists with zero or
    /// one confirmed witness keep the full window.
    pub fn effective_freshness_window(&self, confirmed: usize) -> Duration {
        let max_tolerance = Self::tolerance(self.capacity);
        if confirmed <= 1 || max_tolerance == 0 {
            return self.freshness_window;
        }
        let ratio = Self::tolerance(confirmed) as f64 / max_tolerance as f64;
        self.freshness_window
            .mul_f64(ratio.clamp(self.freshness_floor, 1.0))
    }

    /// Removes every slot (anchor or not) not observed within the freshness
    /// window (scaled for confirmed slots, see
    /// [`Self::effective_freshness_window`]); probationary slots use the base
    /// window. The window is computed once from the confirmed count before
    /// any removal, so one pass never tightens as it evicts. Returns the
    /// removed slots.
    pub fn prune_stale(&mut self, now: OffsetDateTime) -> Vec<KnownSlot> {
        let confirmed = self
            .slots
            .iter()
            .filter(|s| s.status == SlotStatus::Confirmed)
            .count();
        let confirmed_window = self.effective_freshness_window(confirmed);
        let base_window = self.freshness_window;
        let mut removed = Vec::new();
        self.slots.retain(|s| {
            let window = if s.status == SlotStatus::Confirmed {
                confirmed_window
            } else {
                base_window
            };
            let stale = now - s.last_observed_at > window;
            if stale {
                removed.push(s.clone());
            }
            !stale
        });
        for slot in &removed {
            self.candidate_first_seen.remove(&slot.witness_key_id);
        }
        removed
    }

    /// Fills open slots from `candidates`, longest-tenured first (oldest
    /// [`Self::candidate_first_seen`] timestamp — ties broken by
    /// `witness_key_id` for determinism), under the same admission rule
    /// [`Self::try_admit`] enforces. A candidate already a slot is instead
    /// just freshness-refreshed via [`Self::observe`] (called for every
    /// candidate up front, admitted or not — refill doubles as this tick's
    /// liveness signal for existing members). Candidates no longer offered
    /// have their tracked tenure forgotten, so a peer that drops out and
    /// later reappears starts its tenure ranking over. Returns the newly
    /// admitted witness key ids.
    pub fn refill(
        &mut self,
        candidates: &[DiscoveredCandidate],
        now: OffsetDateTime,
    ) -> Vec<String> {
        for c in candidates {
            self.observe(&c.witness_key_id, now);
            self.candidate_first_seen
                .entry(c.witness_key_id.clone())
                .or_insert(now);
        }
        let offered: std::collections::HashSet<&str> = candidates
            .iter()
            .map(|c| c.witness_key_id.as_str())
            .collect();
        let slot_ids: std::collections::HashSet<String> = self
            .slots
            .iter()
            .map(|s| s.witness_key_id.clone())
            .collect();
        self.candidate_first_seen
            .retain(|k, _| offered.contains(k.as_str()) || slot_ids.contains(k));

        let mut ranked: Vec<&DiscoveredCandidate> = candidates
            .iter()
            .filter(|c| !self.contains(&c.witness_key_id))
            .collect();
        ranked.sort_by(|a, b| {
            let ta = self
                .candidate_first_seen
                .get(&a.witness_key_id)
                .copied()
                .unwrap_or(now);
            let tb = self
                .candidate_first_seen
                .get(&b.witness_key_id)
                .copied()
                .unwrap_or(now);
            ta.cmp(&tb)
                .then_with(|| a.witness_key_id.cmp(&b.witness_key_id))
        });

        let mut admitted = Vec::new();
        for c in ranked {
            if self.try_admit(c.witness_key_id.clone(), c.prefix.clone(), c.is_anchor, now) {
                admitted.push(c.witness_key_id.clone());
            }
        }
        admitted
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn witness_key_ids(&self) -> Vec<&str> {
        self.slots
            .iter()
            .map(|s| s.witness_key_id.as_str())
            .collect()
    }

    /// Only `Confirmed` slots — what a future majority computation
    /// (#932/#947) is expected to feed `majority_threshold`, so a
    /// probationary slot never contributes weight it hasn't earned yet.
    pub fn confirmed_witness_key_ids(&self) -> Vec<&str> {
        self.slots
            .iter()
            .filter(|s| s.status == SlotStatus::Confirmed)
            .map(|s| s.witness_key_id.as_str())
            .collect()
    }

    #[cfg(test)]
    fn status_of(&self, witness_key_id: &str) -> Option<SlotStatus> {
        self.slots
            .iter()
            .find(|s| s.witness_key_id == witness_key_id)
            .map(|s| s.status)
    }
}

/// Thread-safe, cloneable handle around a [`KnownListInner`], with an
/// optional on-disk path it persists slot membership to. `Arc<RwLock<_>>`
/// around plain logic, same shape `crate::nodes::PeerTable` already
/// establishes for shared in-process state — the difference here is the
/// optional persistence, not the concurrency shape.
#[derive(Clone)]
pub struct KnownListHandle {
    inner: Arc<RwLock<KnownListInner>>,
    path: Option<PathBuf>,
}

/// What changed on one [`KnownListHandle::tick`] call, for the worker's own
/// logging.
#[derive(Debug, Default)]
pub struct TickReport {
    pub admitted: Vec<String>,
    pub promoted: Vec<String>,
    pub removed: Vec<String>,
}

impl TickReport {
    fn changed(&self) -> bool {
        !self.admitted.is_empty() || !self.promoted.is_empty() || !self.removed.is_empty()
    }
}

impl KnownListHandle {
    /// Loads slot membership from `path` if it exists and parses; starts
    /// empty otherwise (first boot, or a corrupt/missing file — logged,
    /// never fatal, since the list rebuilds itself from ordinary refill
    /// either way). `path: None` disables persistence entirely (used by
    /// tests): the list still works in-process, it just never survives a
    /// restart.
    pub fn load_or_new(cfg: KnownListConfig, path: Option<PathBuf>) -> Self {
        let slots = path.as_deref().map(load_slots).unwrap_or_default();
        if !slots.is_empty() {
            tracing::info!(
                event = "known_list_loaded",
                slots = slots.len(),
                "loaded known list from disk"
            );
        }
        Self {
            inner: Arc::new(RwLock::new(KnownListInner::from_persisted(cfg, slots))),
            path,
        }
    }

    fn lock_read(&self) -> std::sync::RwLockReadGuard<'_, KnownListInner> {
        self.inner.read().expect("known list lock poisoned")
    }

    fn lock_write(&self) -> std::sync::RwLockWriteGuard<'_, KnownListInner> {
        self.inner.write().expect("known list lock poisoned")
    }

    pub fn len(&self) -> usize {
        self.lock_read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock_read().is_empty()
    }

    pub fn witness_key_ids(&self) -> Vec<String> {
        self.lock_read()
            .witness_key_ids()
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    pub fn confirmed_witness_key_ids(&self) -> Vec<String> {
        self.lock_read()
            .confirmed_witness_key_ids()
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    /// One full maintenance pass: confirm slots past probation, prune
    /// stale (unresponsive) ones, then refill open slots from `candidates`
    /// under the tenure-weighted, diversity-capped rule. Persists to disk
    /// (when a path is configured) only when something actually changed.
    pub fn tick(&self, candidates: &[DiscoveredCandidate], now: OffsetDateTime) -> TickReport {
        let report = {
            let mut inner = self.lock_write();
            let promoted = inner.confirm_due(now);
            let removed = inner.prune_stale(now);
            let admitted = inner.refill(candidates, now);
            TickReport {
                admitted,
                promoted,
                removed: removed.into_iter().map(|s| s.witness_key_id).collect(),
            }
        };
        if report.changed() {
            self.save();
        }
        report
    }

    /// Directly exposed for tests exercising admission without going
    /// through a full [`Self::tick`]. `pub(crate)` (not module-private) so
    /// other modules' own `#[cfg(test)]` code — `crate::cosign_verify`,
    /// `crate::mirror_watcher` — can build a known list with specific
    /// membership directly, rather than only through `tick`'s
    /// discovery-candidate shape.
    #[cfg(test)]
    pub(crate) fn try_admit(
        &self,
        witness_key_id: &str,
        prefix: &str,
        is_anchor: bool,
        now: OffsetDateTime,
    ) -> bool {
        let admitted = self.lock_write().try_admit(
            witness_key_id.to_string(),
            prefix.to_string(),
            is_anchor,
            now,
        );
        if admitted {
            self.save();
        }
        admitted
    }

    #[cfg(test)]
    pub(crate) fn remove(&self, witness_key_id: &str) -> bool {
        let removed = self.lock_write().remove(witness_key_id);
        if removed {
            self.save();
        }
        removed
    }

    #[cfg(test)]
    fn status_of(&self, witness_key_id: &str) -> Option<SlotStatus> {
        self.lock_read().status_of(witness_key_id)
    }

    /// Blocking file write — the persisted file is a handful of slots
    /// (kilobytes at most) and a save happens at most once per refill
    /// tick (minutes apart, see [`refill_interval_from_env`]), so this
    /// stays inline rather than dispatched to a blocking pool.
    fn save(&self) {
        let Some(path) = &self.path else { return };
        let slots = self.lock_read().slots.clone();
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(event = "known_list_save_failed", error = %e, "could not create data directory");
                return;
            }
        }
        match serde_json::to_vec_pretty(&PersistedKnownList {
            version: PERSISTED_FORMAT_VERSION,
            slots,
        }) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(path, bytes) {
                    tracing::warn!(event = "known_list_save_failed", error = %e, "could not write known list to disk");
                }
            }
            Err(e) => {
                tracing::warn!(event = "known_list_save_failed", error = %e, "could not serialize known list");
            }
        }
    }
}

fn load_slots(path: &Path) -> Vec<KnownSlot> {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<PersistedKnownList>(&bytes) {
            Ok(persisted) if persisted.version == PERSISTED_FORMAT_VERSION => persisted.slots,
            Ok(_) => {
                tracing::info!(
                    event = "known_list_discarded",
                    "known list file is from an older format — starting empty"
                );
                Vec::new()
            }
            Err(e) => {
                tracing::warn!(
                    event = "known_list_load_failed",
                    error = %e,
                    "known list file did not parse — starting empty and rebuilding from refill"
                );
                Vec::new()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            tracing::warn!(
                event = "known_list_load_failed",
                error = %e,
                "could not read known list file — starting empty and rebuilding from refill"
            );
            Vec::new()
        }
    }
}

/// Resolves `base_url`'s address via `policy` (reusing the exact same
/// outbound address checks/resolution every other peer-supplied URL in
/// this crate goes through — see `crate::outbound_policy`'s own module doc
/// comment) and derives its diversity prefix. `None` when the URL is
/// malformed, doesn't resolve, or resolves to an address outbound policy
/// refuses — the same candidate is simply skipped this tick rather than
/// admitted with a fabricated prefix.
async fn prefix_for(policy: &OutboundPolicy, base_url: &str) -> Option<String> {
    policy
        .check_base_url(base_url)
        .await
        .ok()
        .map(|checked| diversity_prefix(checked.addr.ip()))
}

/// The witness key `info`'s own endpoint vouched for, only while its proof
/// is fresh; gossiped or inbound adverts never qualify.
fn fresh_witness_key(info: &crate::nodes::PeerInfo, now: OffsetDateTime) -> Option<&str> {
    let advert = info.witness.as_ref().filter(|a| a.direct)?;
    ((now - advert.announced_at).abs() <= avalon_protocol::witness::WITNESS_ANNOUNCE_MAX_SKEW)
        .then_some(advert.key_id.as_str())
}

/// Builds this tick's candidate list, each keyed by a peer's proven witness
/// key. A bundled anchor (`docs/trusted-networks.json`'s `seed_nodes`) is
/// keyed by the witness key it announced under its seed URL; then every
/// other peer in `peers` with a fresh proven key. A peer without a key is
/// not a witness candidate. Address resolution (and therefore the diversity
/// prefix) goes through `policy` for both.
pub(crate) async fn build_candidates(
    policy: &OutboundPolicy,
    peers: &PeerTable,
    network_id: &str,
    now: OffsetDateTime,
) -> Vec<DiscoveredCandidate> {
    let mut candidates = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let by_url: HashMap<String, crate::nodes::PeerInfo> = peers
        .list_unverified()
        .into_iter()
        .chain(peers.list_all())
        .map(|p| (p.base_url.clone(), p))
        .collect();

    for anchor in avalon_protocol::network_trust::bundled_trust_anchors()
        .iter()
        .filter(|a| a.network_id == network_id)
    {
        for seed in &anchor.seed_nodes {
            let Some(key) = by_url
                .get(&crate::nodes::normalized_base_url(seed))
                .and_then(|p| fresh_witness_key(p, now))
            else {
                continue;
            };
            if let Some(prefix) = prefix_for(policy, seed).await {
                if seen.insert(key.to_string()) {
                    candidates.push(DiscoveredCandidate {
                        witness_key_id: key.to_string(),
                        prefix,
                        is_anchor: true,
                    });
                }
            }
        }
    }

    for info in peers.list_all() {
        let Some(key) = fresh_witness_key(&info, now) else {
            continue;
        };
        if !seen.insert(key.to_string()) {
            continue;
        }
        if let Some(prefix) = prefix_for(policy, &info.base_url).await {
            candidates.push(DiscoveredCandidate {
                witness_key_id: key.to_string(),
                prefix,
                is_anchor: false,
            });
        }
    }

    candidates
}

/// Spawned unconditionally at startup, same posture `crate::nodes::run_worker`
/// takes: even a node with no peers yet still has its bundled anchors to
/// try to admit, and needs its probation/freshness maintenance running
/// regardless. Never returns.
pub async fn run_worker(
    peers: PeerTable,
    network_id: String,
    handle: KnownListHandle,
    interval: Duration,
) {
    let policy = OutboundPolicy::from_env();
    loop {
        let now = OffsetDateTime::now_utc();
        let candidates = build_candidates(&policy, &peers, &network_id, now).await;
        let report = handle.tick(&candidates, now);
        if report.changed() {
            tracing::info!(
                event = "known_list_tick",
                admitted = ?report.admitted,
                promoted = ?report.promoted,
                removed = ?report.removed,
                list_size = handle.len(),
                "known list membership changed",
            );
        }
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use avalon_protocol::witness::majority_threshold;

    fn cfg() -> KnownListConfig {
        KnownListConfig {
            capacity: 10,
            anchor_capacity: 2,
            max_per_prefix: 2,
            freshness_window: Duration::from_secs(600),
            probation_window: Duration::from_secs(1800),
            freshness_floor: 0.2,
        }
    }

    fn candidate(id: &str, prefix: &str) -> DiscoveredCandidate {
        DiscoveredCandidate {
            witness_key_id: id.to_string(),
            prefix: prefix.to_string(),
            is_anchor: false,
        }
    }

    #[test]
    fn a_persisted_file_without_the_current_format_version_is_discarded() {
        let dir = std::env::temp_dir().join(format!("kl-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = known_list_path(&dir);
        std::fs::write(&path, br#"{"slots":[{"witness_key_id":"http://x","prefix":"p","is_anchor":false,"admitted_at":"2026-01-01T00:00:00Z","last_observed_at":"2026-01-01T00:00:00Z","status":"confirmed"}]}"#).unwrap();
        assert!(load_slots(&path).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn anchors_fill_their_reserved_slots_and_no_more() {
        let mut list = KnownListInner::new(cfg());
        let now = OffsetDateTime::now_utc();
        assert!(list.try_admit("anchor-1".into(), "anchor-net-a".into(), true, now));
        assert!(list.try_admit("anchor-2".into(), "anchor-net-b".into(), true, now));
        assert!(!list.try_admit("anchor-3".into(), "anchor-net-c".into(), true, now));
        assert_eq!(list.len(), 2);
        // Anchors are admitted confirmed outright, unlike ordinary peers.
        assert_eq!(list.status_of("anchor-1"), Some(SlotStatus::Confirmed));
    }

    /// Eclipse resistance, extended to the production refill path: a flood
    /// of same-prefix candidates offered to `refill` (not just `try_admit`
    /// directly) still cannot exceed the diversity cap, and cannot displace
    /// slots already admitted from other prefixes.
    #[test]
    fn a_flood_of_same_prefix_candidates_cannot_exceed_the_diversity_cap_via_refill() {
        let mut list = KnownListInner::new(cfg());
        let now = OffsetDateTime::now_utc();
        // Seed two established, diverse slots first.
        assert!(list.try_admit("established-0".into(), "net-0".into(), false, now));
        assert!(list.try_admit("established-1".into(), "net-1".into(), false, now));

        let flood: Vec<DiscoveredCandidate> = (0..50)
            .map(|i| candidate(&format!("flood-{i}"), "attacker-net"))
            .collect();
        let admitted = list.refill(&flood, now);

        assert_eq!(
            admitted.len(),
            2,
            "diversity cap should have refused the rest"
        );
        assert_eq!(list.prefix_count("attacker-net"), 2);
        assert!(list.contains("established-0"));
        assert!(list.contains("established-1"));
    }

    /// Extends the prototype's refill-after-loss test to the production
    /// type: fill to capacity, lose several non-anchor slots, refill from
    /// fresh candidates, and check `majority_threshold` recomputes
    /// correctly throughout using only confirmed witnesses.
    #[test]
    fn the_list_refills_to_capacity_after_witness_loss_and_stays_diverse() {
        let mut list = KnownListInner::new(cfg());
        let now = OffsetDateTime::now_utc();
        assert!(list.try_admit("anchor-1".into(), "anchor-net-a".into(), true, now));
        assert!(list.try_admit("anchor-2".into(), "anchor-net-b".into(), true, now));
        for i in 0..8 {
            assert!(list.try_admit(format!("peer-{i}"), format!("net-{}", i % 4), false, now));
        }
        assert_eq!(list.len(), 10);
        // Every non-anchor slot just admitted is still probationary, but
        // the count that matters for this check is total list size.
        assert_eq!(majority_threshold(list.len()), 6);

        for i in 0..3 {
            assert!(list.remove(&format!("peer-{i}")));
        }
        assert_eq!(list.len(), 7);
        assert_eq!(majority_threshold(list.len()), 4);

        let fresh: Vec<DiscoveredCandidate> = (8..14)
            .map(|i| candidate(&format!("peer-{i}"), &format!("net-{}", i % 4)))
            .collect();
        list.refill(&fresh, now);
        assert_eq!(list.len(), 10);
        assert_eq!(majority_threshold(list.len()), 6);
        assert!(list.contains("anchor-1"));
        assert!(list.contains("anchor-2"));
        for prefix in ["net-0", "net-1", "net-2", "net-3"] {
            assert!(
                list.prefix_count(prefix) <= 2,
                "diversity cap held for {prefix}"
            );
        }
    }

    /// Anchor loss: an anchor slot going stale past the freshness window is
    /// removed by `prune_stale` like any other slot, and the freed anchor
    /// slot is then filled by ordinary ranked refill (not re-reserved
    /// specially) — matching the design doc's "an anchor going offline is
    /// handled by ordinary refill once it's gone."
    #[test]
    fn a_stale_anchor_is_pruned_and_its_slot_refilled_by_ordinary_refill() {
        let mut list = KnownListInner::new(cfg());
        let t0 = OffsetDateTime::now_utc();
        assert!(list.try_admit("anchor-1".into(), "anchor-net-a".into(), true, t0));
        assert!(list.try_admit("anchor-2".into(), "anchor-net-b".into(), true, t0));

        let past_freshness = t0 + Duration::from_secs(700);
        let removed = list.prune_stale(past_freshness);
        assert_eq!(removed.len(), 2, "both anchors went stale with no refresh");
        assert!(list.is_empty());

        // The freed slots (no longer anchor-reserved) admit an ordinary
        // candidate via ranked refill.
        let admitted = list.refill(&[candidate("peer-x", "net-x")], past_freshness);
        assert_eq!(admitted, vec!["peer-x".to_string()]);
        assert_eq!(list.status_of("peer-x"), Some(SlotStatus::Probationary));
    }

    /// Probation: a burst of brand-new candidates is admitted into open
    /// slots but stays `Probationary` — none of them count toward
    /// `confirmed_witness_key_ids` (the input a future majority computation
    /// uses) until the probation window has passed while observed fresh.
    #[test]
    fn a_burst_of_new_candidates_does_not_immediately_count_toward_the_list() {
        let mut list = KnownListInner::new(cfg());
        let t0 = OffsetDateTime::now_utc();
        let burst: Vec<DiscoveredCandidate> = (0..8)
            .map(|i| candidate(&format!("burst-{i}"), &format!("net-{}", i % 4)))
            .collect();
        let admitted = list.refill(&burst, t0);
        assert_eq!(admitted.len(), 8);
        assert_eq!(list.len(), 8);
        assert!(
            list.confirmed_witness_key_ids().is_empty(),
            "a fresh burst must not immediately count toward the list"
        );

        // Immediately after admission, confirm_due promotes nothing.
        assert!(list.confirm_due(t0).is_empty());

        // Still short of the probation window: nothing confirmed yet.
        let mid = t0 + Duration::from_secs(900);
        assert!(list.confirm_due(mid).is_empty());

        // Past the probation window, and freshly observed (refill also
        // calls `observe`), every burst member is confirmed.
        let refill_tick = t0 + Duration::from_secs(1800);
        list.refill(&burst, refill_tick);
        let past_probation = refill_tick + Duration::from_secs(1);
        let promoted = list.confirm_due(past_probation);
        assert_eq!(promoted.len(), 8);
        assert_eq!(list.confirmed_witness_key_ids().len(), 8);
    }

    /// A probationary slot that goes stale during its own probation is not
    /// confirmed on a technicality — it just ages out via `prune_stale`.
    #[test]
    fn a_probationary_slot_that_goes_stale_is_not_confirmed() {
        let mut list = KnownListInner::new(cfg());
        let t0 = OffsetDateTime::now_utc();
        assert!(list.try_admit("peer-1".into(), "net-0".into(), false, t0));

        let past_probation_and_freshness = t0 + Duration::from_secs(1900);
        assert!(list.confirm_due(past_probation_and_freshness).is_empty());
        let removed = list.prune_stale(past_probation_and_freshness);
        assert_eq!(removed.len(), 1);
    }

    /// Tenure-weighted refill: when more candidates are offered than there
    /// is room for, the longer-observed one wins the open slot.
    #[test]
    fn refill_prefers_longer_tenured_candidates_for_open_slots() {
        let mut single_slot = cfg();
        single_slot.capacity = 1;
        single_slot.anchor_capacity = 0;
        let mut list = KnownListInner::new(single_slot);
        let t0 = OffsetDateTime::now_utc();
        let t1 = t0 + Duration::from_secs(60);

        // "tenured" has been offered (and tracked) since t0; "fresh" only
        // shows up at t1, alongside it, competing for the same one slot.
        list.candidate_first_seen.insert("tenured".to_string(), t0);
        let admitted = list.refill(
            &[candidate("fresh", "net-b"), candidate("tenured", "net-a")],
            t1,
        );
        assert_eq!(admitted, vec!["tenured".to_string()]);
    }

    #[test]
    fn a_witness_already_present_is_not_admitted_twice() {
        let mut list = KnownListInner::new(cfg());
        let now = OffsetDateTime::now_utc();
        assert!(list.try_admit("peer-1".into(), "net-0".into(), false, now));
        assert!(!list.try_admit("peer-1".into(), "net-0".into(), false, now));
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn restart_keeps_the_list_persistence_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = known_list_path(dir.path());
        let now = OffsetDateTime::now_utc();

        let handle = KnownListHandle::load_or_new(cfg(), Some(path.clone()));
        assert!(handle.try_admit("anchor-1", "anchor-net-a", true, now));
        assert!(handle.try_admit("peer-1", "net-0", false, now));
        assert_eq!(handle.len(), 2);
        drop(handle);

        // A fresh handle, same path — simulates a process restart.
        let reloaded = KnownListHandle::load_or_new(cfg(), Some(path));
        assert_eq!(reloaded.len(), 2);
        let ids = reloaded.witness_key_ids();
        assert!(ids.contains(&"anchor-1".to_string()));
        assert!(ids.contains(&"peer-1".to_string()));
        assert_eq!(reloaded.status_of("anchor-1"), Some(SlotStatus::Confirmed));
        assert_eq!(reloaded.status_of("peer-1"), Some(SlotStatus::Probationary));
    }

    #[test]
    fn removing_via_the_handle_persists_the_removal_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = known_list_path(dir.path());
        let now = OffsetDateTime::now_utc();

        let handle = KnownListHandle::load_or_new(cfg(), Some(path.clone()));
        assert!(handle.try_admit("peer-1", "net-0", false, now));
        assert!(handle.remove("peer-1"));
        drop(handle);

        let reloaded = KnownListHandle::load_or_new(cfg(), Some(path));
        assert!(reloaded.is_empty());
    }

    #[test]
    fn missing_or_corrupt_persistence_file_starts_empty_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = known_list_path(dir.path());
        // No file yet.
        let handle = KnownListHandle::load_or_new(cfg(), Some(path.clone()));
        assert!(handle.is_empty());

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not json").unwrap();
        let handle = KnownListHandle::load_or_new(cfg(), Some(path));
        assert!(handle.is_empty());
    }

    #[test]
    fn diversity_prefix_groups_v4_by_24_and_v6_by_64() {
        assert_eq!(
            diversity_prefix("10.1.2.3".parse().unwrap()),
            "v4:10.1.2.0/24"
        );
        assert_eq!(
            diversity_prefix("10.1.2.200".parse().unwrap()),
            "v4:10.1.2.0/24"
        );
        assert_ne!(
            diversity_prefix("10.1.2.3".parse().unwrap()),
            diversity_prefix("10.1.3.3".parse().unwrap())
        );
        let a: IpAddr = "2001:db8:abcd:1::1".parse().unwrap();
        let b: IpAddr = "2001:db8:abcd:1::2".parse().unwrap();
        let c: IpAddr = "2001:db8:abcd:2::1".parse().unwrap();
        assert_eq!(diversity_prefix(a), diversity_prefix(b));
        assert_ne!(diversity_prefix(a), diversity_prefix(c));
    }

    fn list_with_capacity(capacity: usize) -> KnownListInner {
        let mut c = cfg();
        c.capacity = capacity;
        KnownListInner::new(c)
    }

    fn secs(list: &KnownListInner, n: usize) -> u64 {
        list.effective_freshness_window(n).as_secs()
    }

    #[test]
    fn defaults_are_capacity_five() {
        let d = KnownListConfig::default();
        assert_eq!(d.capacity, 5);
        assert!(d.anchor_capacity <= d.capacity);
        assert_eq!(d.freshness_floor, 0.2);
    }

    #[test]
    fn an_oversized_anchor_capacity_is_clamped_not_fatal() {
        let mut c = cfg();
        c.capacity = 1;
        c.anchor_capacity = 2;
        let mut list = KnownListInner::new(c);
        let now = OffsetDateTime::now_utc();
        assert!(list.try_admit("a1".into(), "n1".into(), true, now));
        assert!(!list.try_admit("a2".into(), "n2".into(), true, now));
    }

    #[test]
    fn effective_window_at_capacity_five() {
        let list = list_with_capacity(5);
        // tolerance: n=2 -> 0, 3 -> 1, 4 -> 1, 5 -> 2; max tolerance 2.
        assert_eq!(secs(&list, 0), 600);
        assert_eq!(secs(&list, 1), 600);
        assert_eq!(secs(&list, 2), 120);
        assert_eq!(secs(&list, 3), 300);
        assert_eq!(secs(&list, 4), 300);
        assert_eq!(secs(&list, 5), 600);
    }

    #[test]
    fn effective_window_at_capacity_ten() {
        let list = list_with_capacity(10);
        // max tolerance 4 (10 - 6).
        assert_eq!(secs(&list, 1), 600);
        assert_eq!(secs(&list, 2), 120);
        assert_eq!(secs(&list, 3), 150);
        assert_eq!(secs(&list, 4), 150);
        assert_eq!(secs(&list, 5), 300);
        assert_eq!(secs(&list, 10), 600);
    }

    #[test]
    fn the_floor_bounds_the_scale_factor() {
        let mut c = cfg();
        c.capacity = 5;
        c.freshness_floor = 0.7;
        let list = KnownListInner::new(c);
        assert_eq!(secs(&list, 2), 420);
        assert_eq!(secs(&list, 3), 420);
        assert_eq!(secs(&list, 5), 600);
        c.freshness_floor = 1.0;
        assert_eq!(secs(&KnownListInner::new(c), 3), 600);
    }

    fn confirmed_list(ids: &[&str], t0: OffsetDateTime) -> KnownListInner {
        let mut list = list_with_capacity(5);
        for (i, id) in ids.iter().enumerate() {
            assert!(list.try_admit((*id).into(), format!("net-{i}"), false, t0));
        }
        for slot in list.slots.iter_mut() {
            slot.status = SlotStatus::Confirmed;
        }
        list
    }

    #[test]
    fn a_two_slot_list_evicts_a_silent_slot_after_the_scaled_window_only() {
        let t0 = OffsetDateTime::now_utc();
        let mut list = confirmed_list(&["a", "b"], t0);
        list.observe("a", t0 + Duration::from_secs(200));
        // Scaled window is 120s: b (silent 110s) stays, then goes at 121s.
        assert!(list.prune_stale(t0 + Duration::from_secs(110)).is_empty());
        let removed = list.prune_stale(t0 + Duration::from_secs(121));
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].witness_key_id, "b");
    }

    #[test]
    fn a_full_list_keeps_the_full_window() {
        let t0 = OffsetDateTime::now_utc();
        let mut list = confirmed_list(&["a", "b", "c", "d", "e"], t0);
        assert!(list.prune_stale(t0 + Duration::from_secs(599)).is_empty());
        assert_eq!(list.prune_stale(t0 + Duration::from_secs(601)).len(), 5);
    }

    #[test]
    fn one_pass_does_not_cascade_with_a_tightening_window() {
        let t0 = OffsetDateTime::now_utc();
        let mut list = confirmed_list(&["a", "b", "c", "d", "e"], t0);
        // Four slots idle 400s, one fresh. Window from n=5 is 600s: nobody is
        // evicted, even though the shrinking list would allow 120s.
        list.observe("e", t0 + Duration::from_secs(400));
        assert!(list.prune_stale(t0 + Duration::from_secs(400)).is_empty());
        assert_eq!(list.len(), 5);
        // Next pass, still 5 confirmed: at 601s four go together; the fifth
        // survives the same pass and is judged on its own age afterwards.
        let removed = list.prune_stale(t0 + Duration::from_secs(601));
        assert_eq!(removed.len(), 4);
        assert!(list.contains("e"));
    }

    #[test]
    fn probationary_slots_keep_the_base_window() {
        let t0 = OffsetDateTime::now_utc();
        let mut list = list_with_capacity(5);
        assert!(list.try_admit("a1".into(), "n1".into(), true, t0));
        assert!(list.try_admit("a2".into(), "n2".into(), true, t0));
        assert!(list.try_admit("p".into(), "n3".into(), false, t0));
        list.observe("a1", t0 + Duration::from_secs(500));
        list.observe("a2", t0 + Duration::from_secs(500));
        // Two confirmed -> scaled 120s applies to them, not to the probationary slot.
        assert!(list.prune_stale(t0 + Duration::from_secs(500)).is_empty());
        assert!(list.contains("p"));
        let removed = list.prune_stale(t0 + Duration::from_secs(601));
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].witness_key_id, "p");
    }
}
