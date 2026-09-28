//! Selection, bounds and backoff for refreshing the cosignatures of witnesses a mirror knows from
//! its peer directory but does not hold in its confirmed known list.
//!
//! A witness is only ever asked for its own cosignature over a head this node already observed,
//! at the base URL whose own endpoint vouched for the witness key. Limits here shape how much a
//! node fetches and stores; they never change what a client accepts.

use std::collections::{HashMap, HashSet};

use ed25519_dalek::VerifyingKey;
use time::{Duration, OffsetDateTime};

use crate::nodes::PeerInfo;

/// Witnesses outside the known list asked per shard head each tick, unless overridden.
pub const DEFAULT_MAX_PER_TICK: usize = 16;
/// Hard ceiling on the per-tick setting.
pub const MAX_PER_TICK_CEILING: usize = 64;
/// Stored cosignatures per head from witnesses outside the known list, as a multiple of the
/// per-tick limit.
pub const ROW_CAP_FACTOR: usize = 2;
const BACKOFF_BASE: Duration = Duration::seconds(30);
const BACKOFF_CAP: Duration = Duration::minutes(30);
const MAX_BACKOFF_ENTRIES: usize = 1024;

/// `AVALON_WITNESS_REFRESH_MAX_PER_TICK`: 0 disables the extra refresh, values above
/// [`MAX_PER_TICK_CEILING`] are clamped, unset or unparseable falls back to the default.
pub fn max_per_tick_from_env() -> usize {
    parse_max_per_tick(
        std::env::var("AVALON_WITNESS_REFRESH_MAX_PER_TICK")
            .ok()
            .as_deref(),
    )
}

fn parse_max_per_tick(raw: Option<&str>) -> usize {
    raw.and_then(|v| v.trim().parse::<usize>().ok())
        .map_or(DEFAULT_MAX_PER_TICK, |n| n.min(MAX_PER_TICK_CEILING))
}

/// A witness known from the peer directory that is not in the confirmed known list.
#[derive(Debug, Clone)]
pub struct DirectoryWitness {
    pub key_id: String,
    pub key: VerifyingKey,
    pub base_url: String,
    pub announced_at: OffsetDateTime,
}

impl DirectoryWitness {
    pub fn source(&self) -> crate::cosign_gather::WitnessSource {
        crate::cosign_gather::WitnessSource {
            key_id: self.key_id.clone(),
            base_url: self.base_url.clone(),
        }
    }
}

/// Witnesses in `peers` whose own endpoint proved possession of the key recently, minus the
/// known list and `exclude_key`. One entry per key (the most recent advert wins), sorted by key.
pub fn directory_witnesses(
    peers: &[PeerInfo],
    known_list: &[(String, VerifyingKey)],
    exclude_key: Option<&str>,
    now: OffsetDateTime,
) -> Vec<DirectoryWitness> {
    let mut by_key: HashMap<String, DirectoryWitness> = HashMap::new();
    for peer in peers {
        let Some(advert) = peer.witness.as_ref().filter(|a| a.direct) else {
            continue;
        };
        if (now - advert.announced_at).abs() > avalon_protocol::witness::WITNESS_ANNOUNCE_MAX_SKEW
            || Some(advert.key_id.as_str()) == exclude_key
            || known_list.iter().any(|(id, _)| *id == advert.key_id)
        {
            continue;
        }
        let Some(key) = crate::cosign_verify::parse_hex_verifying_key(&advert.key_id) else {
            continue;
        };
        let candidate = DirectoryWitness {
            key_id: advert.key_id.clone(),
            key,
            base_url: peer.base_url.clone(),
            announced_at: advert.announced_at,
        };
        match by_key.get(&advert.key_id) {
            Some(existing) if existing.announced_at >= candidate.announced_at => {}
            _ => {
                by_key.insert(advert.key_id.clone(), candidate);
            }
        }
    }
    let mut out: Vec<_> = by_key.into_values().collect();
    out.sort_by(|a, b| a.key_id.cmp(&b.key_id));
    out
}

/// How an attempt to fetch a witness's own cosignature ended, as far as backoff is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt {
    Ok,
    /// The witness could not be reached or answered unusably: applies to every shard.
    Transport,
    /// The witness answered but has nothing usable for this shard's head (lagging, does not
    /// cosign this shard, out-of-range timestamp): a short retry for this shard only.
    NotAvailable,
}

/// Failure backoff, in memory only. Transport failures are per witness and double from 30 s up
/// to 30 min; "not available for this shard" is per (witness, shard) and a flat 30 s.
#[derive(Debug, Default)]
pub struct Backoff {
    entries: HashMap<(String, String), (u32, OffsetDateTime)>,
}

impl Backoff {
    pub fn ready(&self, key_id: &str, shard_id: &str, now: OffsetDateTime) -> bool {
        [(key_id, ""), (key_id, shard_id)].iter().all(|(k, s)| {
            self.entries
                .get(&(k.to_string(), s.to_string()))
                .is_none_or(|(_, next)| *next <= now)
        })
    }

    pub fn record(&mut self, key_id: &str, shard_id: &str, attempt: Attempt, now: OffsetDateTime) {
        match attempt {
            Attempt::Ok => {
                self.entries.remove(&(key_id.to_string(), String::new()));
                self.entries
                    .remove(&(key_id.to_string(), shard_id.to_string()));
            }
            Attempt::Transport => {
                let slot = (key_id.to_string(), String::new());
                let failures = self
                    .entries
                    .get(&slot)
                    .map_or(0, |(n, _)| *n)
                    .saturating_add(1);
                let wait =
                    (BACKOFF_BASE * 2i32.pow(failures.saturating_sub(1).min(6))).min(BACKOFF_CAP);
                self.entries.insert(slot, (failures, now + wait));
            }
            Attempt::NotAvailable => {
                self.entries.insert(
                    (key_id.to_string(), shard_id.to_string()),
                    (1, now + BACKOFF_BASE),
                );
            }
        }
    }

    /// Forgets witnesses no longer in the directory, and bounds the map.
    pub fn retain_live(&mut self, live: &HashSet<&str>) {
        self.entries.retain(|(k, _), _| live.contains(k.as_str()));
        if self.entries.len() > MAX_BACKOFF_ENTRIES {
            self.entries.clear();
        }
    }
}

/// Per-node memory that shapes selection: backoff, when each (witness, shard) was last asked, and
/// which witnesses last delivered a cosignature for each shard.
#[derive(Debug, Default)]
pub struct RefreshState {
    pub backoff: Backoff,
    last_asked: HashMap<(String, String), OffsetDateTime>,
    previous: HashMap<String, HashSet<String>>,
    salt: std::collections::hash_map::RandomState,
}

impl RefreshState {
    pub fn asked(&mut self, key_id: &str, shard_id: &str, now: OffsetDateTime) {
        if self.last_asked.len() >= MAX_BACKOFF_ENTRIES {
            self.last_asked.clear();
        }
        self.last_asked
            .insert((key_id.to_string(), shard_id.to_string()), now);
    }

    /// Notes that `key_id` delivered a cosignature for `shard_id`; the set is bounded by `cap`.
    pub fn delivered(&mut self, key_id: &str, shard_id: &str, cap: usize) {
        let set = self.previous.entry(shard_id.to_string()).or_default();
        if set.len() < cap {
            set.insert(key_id.to_string());
        }
    }

    pub fn retain_live(&mut self, live: &HashSet<&str>) {
        self.backoff.retain_live(live);
        self.last_asked
            .retain(|(k, _), _| live.contains(k.as_str()));
        for set in self.previous.values_mut() {
            set.retain(|k| live.contains(k.as_str()));
        }
    }

    fn salted(&self, key_id: &str) -> u64 {
        use std::hash::BuildHasher;
        self.salt.hash_one(key_id)
    }
}

/// Which directory witnesses to ask for one head of `shard_id` this tick.
///
/// Witnesses already holding a stored cosignature for the head come first, stalest first, so a
/// limit smaller than the set still rotates through all of them. Witnesses without one follow,
/// only while the cap on stored cosignatures from outside the known list has room: those that
/// delivered for this shard before first, then the ones asked longest ago (never asked first),
/// ties broken by a per-node random salt. Announcement recency is deliberately not used, so a
/// participant that re-announces often under many keys cannot take every new slot. Witnesses in
/// backoff are skipped without using a slot.
pub fn select(
    candidates: &[DirectoryWitness],
    shard_id: &str,
    stored: &HashMap<String, OffsetDateTime>,
    stored_outside_known_list: usize,
    state: &RefreshState,
    max_per_tick: usize,
    now: OffsetDateTime,
) -> Vec<DirectoryWitness> {
    let ready: Vec<&DirectoryWitness> = candidates
        .iter()
        .filter(|c| state.backoff.ready(&c.key_id, shard_id, now))
        .collect();
    let mut held: Vec<&DirectoryWitness> = ready
        .iter()
        .copied()
        .filter(|c| stored.contains_key(&c.key_id))
        .collect();
    held.sort_by(|a, b| {
        stored[&a.key_id]
            .cmp(&stored[&b.key_id])
            .then_with(|| a.key_id.cmp(&b.key_id))
    });
    let previous = state.previous.get(shard_id);
    let mut fresh: Vec<&DirectoryWitness> = ready
        .iter()
        .copied()
        .filter(|c| !stored.contains_key(&c.key_id))
        .collect();
    fresh.sort_by_cached_key(|c| {
        let was_delivering = previous.is_some_and(|p| p.contains(&c.key_id));
        let asked = state
            .last_asked
            .get(&(c.key_id.clone(), shard_id.to_string()))
            .copied();
        (!was_delivering, asked, state.salted(&c.key_id))
    });
    let room = (max_per_tick * ROW_CAP_FACTOR).saturating_sub(stored_outside_known_list);
    held.into_iter()
        .chain(fresh.into_iter().take(room))
        .take(max_per_tick)
        .cloned()
        .collect()
}

/// Classifies a gather outcome for backoff: only transport-level failures are held against the
/// witness as a whole.
pub fn attempt_for(outcome: &crate::cosign_gather::GatherOutcome, now: OffsetDateTime) -> Attempt {
    use crate::cosign_gather::GatherOutcome as G;
    match outcome {
        G::Fetched(c) if observed_at_in_range(c.observed_at, now) => Attempt::Ok,
        G::Fetched(_) | G::HeadMismatch | G::NoOwnCosignature | G::InvalidSignature => {
            Attempt::NotAvailable
        }
        G::NoSource | G::Blocked | G::Unreachable(_) | G::BadStatus(_) | G::Malformed => {
            Attempt::Transport
        }
    }
}

/// How far in the future a cosignature's `observed_at` may be (clock skew between nodes); a
/// verifier rejects anything after its own clock, so more than this is never useful to store.
pub const MAX_FUTURE_SKEW: Duration = Duration::seconds(60);

/// Whether `observed_at` is within the freshness window before `now` and the skew allowance
/// after it.
pub fn observed_at_in_range(observed_at: OffsetDateTime, now: OffsetDateTime) -> bool {
    let window =
        Duration::seconds(crate::cosign_verify::COSIGNATURE_FRESHNESS_WINDOW.as_secs() as i64);
    observed_at >= now - window && observed_at <= now + MAX_FUTURE_SKEW
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::WitnessAdvert;
    use ed25519_dalek::SigningKey;

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    fn key(n: u8) -> (String, VerifyingKey) {
        let k = SigningKey::from_bytes(&[n; 32]).verifying_key();
        (hex::encode(k.to_bytes()), k)
    }

    fn peer(url: &str, key_id: &str, direct: bool, announced_at: OffsetDateTime) -> PeerInfo {
        let mut p: PeerInfo = serde_json::from_value(serde_json::json!({
            "base_url": url,
            "roles": [],
            "protocol_version": "1",
            "network_id": "n",
            "last_announced_at": "2026-01-01T00:00:00Z",
        }))
        .expect("peer info");
        p.witness = Some(WitnessAdvert {
            key_id: key_id.to_string(),
            announced_at,
            proof: String::new(),
            direct,
        });
        p
    }

    fn dw(n: u8, age_secs: i64) -> DirectoryWitness {
        let (id, k) = key(n);
        DirectoryWitness {
            key_id: id,
            key: k,
            base_url: format!("http://w{n}"),
            announced_at: now() - Duration::seconds(age_secs),
        }
    }

    fn pick(
        c: &[DirectoryWitness],
        stored: &HashMap<String, OffsetDateTime>,
        outside: usize,
        st: &RefreshState,
        max: usize,
        n: OffsetDateTime,
    ) -> Vec<DirectoryWitness> {
        select(c, "core", stored, outside, st, max, n)
    }

    #[test]
    fn env_value_defaults_clamps_and_can_disable() {
        assert_eq!(parse_max_per_tick(None), DEFAULT_MAX_PER_TICK);
        assert_eq!(parse_max_per_tick(Some("junk")), DEFAULT_MAX_PER_TICK);
        assert_eq!(parse_max_per_tick(Some("0")), 0);
        assert_eq!(parse_max_per_tick(Some("5")), 5);
        assert_eq!(parse_max_per_tick(Some("100000")), MAX_PER_TICK_CEILING);
    }

    #[test]
    fn directory_keeps_only_direct_fresh_unknown_witnesses() {
        let (known_id, known_key) = key(1);
        let (w2, _) = key(2);
        let (w3, _) = key(3);
        let (w4, _) = key(4);
        let (own, _) = key(5);
        let n = now();
        let peers = vec![
            peer("http://known", &known_id, true, n),
            peer("http://direct", &w2, true, n),
            peer("http://gossiped", &w3, false, n),
            peer("http://stale", &w4, true, n - Duration::hours(2)),
            peer("http://self", &own, true, n),
            peer("http://bad", "not-hex", true, n),
        ];
        let got = directory_witnesses(&peers, &[(known_id, known_key)], Some(&own), n);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].key_id, w2);
        assert_eq!(got[0].base_url, "http://direct");
    }

    #[test]
    fn one_key_at_many_urls_is_one_candidate_at_its_newest_advert() {
        let (id, _) = key(2);
        let n = now();
        let peers = vec![
            peer("http://old", &id, true, n - Duration::minutes(10)),
            peer("http://new", &id, true, n - Duration::minutes(1)),
        ];
        let got = directory_witnesses(&peers, &[], None, n);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].base_url, "http://new");
    }

    #[test]
    fn transport_backoff_doubles_caps_and_clears_on_success() {
        let n = now();
        let mut b = Backoff::default();
        assert!(b.ready("w", "core", n));
        b.record("w", "core", Attempt::Transport, n);
        assert!(!b.ready("w", "core", n + Duration::seconds(29)));
        assert!(b.ready("w", "core", n + Duration::seconds(31)));
        b.record("w", "core", Attempt::Transport, n);
        assert!(!b.ready("w", "core", n + Duration::seconds(59)));
        for _ in 0..20 {
            b.record("w", "core", Attempt::Transport, n);
        }
        assert!(!b.ready("w", "core", n + Duration::minutes(29)));
        assert!(b.ready("w", "core", n + Duration::minutes(31)));
        b.record("w", "core", Attempt::Ok, n);
        assert!(b.ready("w", "core", n));
    }

    #[test]
    fn transport_failure_holds_back_every_shard() {
        let n = now();
        let mut b = Backoff::default();
        b.record("w", "core", Attempt::Transport, n);
        assert!(!b.ready("w", "game:x", n + Duration::seconds(1)));
    }

    #[test]
    fn not_available_on_one_shard_does_not_hold_back_another() {
        let n = now();
        let mut b = Backoff::default();
        b.record("w", "shard-a", Attempt::NotAvailable, n);
        assert!(!b.ready("w", "shard-a", n + Duration::seconds(29)));
        assert!(b.ready("w", "shard-a", n + Duration::seconds(31)));
        assert!(b.ready("w", "shard-b", n + Duration::seconds(1)));
        b.record("w", "shard-a", Attempt::NotAvailable, n);
        b.record("w", "shard-a", Attempt::NotAvailable, n);
        assert!(
            b.ready("w", "shard-a", n + Duration::seconds(31)),
            "flat, not doubling"
        );
    }

    #[test]
    fn backoff_forgets_departed_witnesses() {
        let n = now();
        let mut b = Backoff::default();
        b.record("gone", "core", Attempt::Transport, n);
        b.record("here", "core", Attempt::Transport, n);
        b.retain_live(&HashSet::from(["here"]));
        assert!(b.ready("gone", "core", n));
        assert!(!b.ready("here", "core", n));
    }

    #[test]
    fn selection_is_bounded_by_the_per_tick_limit() {
        let candidates: Vec<_> = (1..=40u8).map(|n| dw(n, n as i64)).collect();
        let got = pick(
            &candidates,
            &HashMap::new(),
            0,
            &RefreshState::default(),
            16,
            now(),
        );
        assert_eq!(got.len(), 16);
    }

    #[test]
    fn stored_witnesses_come_first_stalest_first() {
        let n = now();
        let candidates = vec![dw(1, 100), dw(2, 50), dw(3, 10), dw(4, 5)];
        let stored = HashMap::from([
            (candidates[0].key_id.clone(), n - Duration::seconds(30)),
            (candidates[1].key_id.clone(), n - Duration::seconds(900)),
        ]);
        let got = pick(&candidates, &stored, 2, &RefreshState::default(), 3, n);
        assert_eq!(got[0].key_id, candidates[1].key_id);
        assert_eq!(got[1].key_id, candidates[0].key_id);
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn a_small_limit_rotates_through_the_stored_set() {
        let n = now();
        let candidates: Vec<_> = (1..=3u8).map(|i| dw(i, 1)).collect();
        let mut stored: HashMap<String, OffsetDateTime> = candidates
            .iter()
            .enumerate()
            .map(|(i, c)| (c.key_id.clone(), n - Duration::seconds(100 - i as i64)))
            .collect();
        let mut seen = HashSet::new();
        for step in 0..3 {
            let got = pick(&candidates, &stored, 3, &RefreshState::default(), 1, n);
            assert_eq!(got.len(), 1);
            seen.insert(got[0].key_id.clone());
            stored.insert(got[0].key_id.clone(), n + Duration::seconds(step));
        }
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn new_witnesses_are_refused_once_the_row_cap_is_reached() {
        let candidates: Vec<_> = (1..=10u8).map(|n| dw(n, 1)).collect();
        let cap = 4 * ROW_CAP_FACTOR;
        let st = RefreshState::default();
        assert!(pick(&candidates, &HashMap::new(), cap, &st, 4, now()).is_empty());
        assert_eq!(
            pick(&candidates, &HashMap::new(), cap - 2, &st, 4, now()).len(),
            2
        );
    }

    #[test]
    fn witnesses_in_backoff_are_skipped_without_using_a_slot() {
        let n = now();
        let candidates = vec![dw(1, 1), dw(2, 2), dw(3, 3)];
        let mut st = RefreshState::default();
        st.backoff
            .record(&candidates[0].key_id, "core", Attempt::Transport, n);
        let got = pick(&candidates, &HashMap::new(), 0, &st, 2, n);
        let ids: HashSet<_> = got.iter().map(|g| g.key_id.clone()).collect();
        assert_eq!(
            ids,
            HashSet::from([candidates[1].key_id.clone(), candidates[2].key_id.clone()])
        );
    }

    #[test]
    fn frequent_reannouncers_cannot_starve_witnesses_that_delivered_before() {
        let n = now();
        let legit = dw(200, 3000);
        let mut candidates = vec![legit.clone()];
        candidates.extend((1..=60u8).map(|i| dw(i, 1)));
        let mut st = RefreshState::default();
        st.delivered(&legit.key_id, "core", 32);
        let got = pick(&candidates, &HashMap::new(), 0, &st, 4, n);
        assert!(got.iter().any(|g| g.key_id == legit.key_id));
    }

    #[test]
    fn never_asked_witnesses_are_admitted_before_recently_asked_ones() {
        let n = now();
        let candidates: Vec<_> = (1..=6u8).map(|i| dw(i, 1)).collect();
        let mut st = RefreshState::default();
        for c in &candidates[..3] {
            st.asked(&c.key_id, "core", n - Duration::seconds(5));
        }
        let got = pick(&candidates, &HashMap::new(), 0, &st, 3, n);
        let ids: HashSet<_> = got.iter().map(|g| g.key_id.clone()).collect();
        let expected: HashSet<_> = candidates[3..].iter().map(|c| c.key_id.clone()).collect();
        assert_eq!(ids, expected);
    }

    #[test]
    fn rotation_reaches_every_witness_even_under_a_flood() {
        let candidates: Vec<_> = (1..=40u8).map(|i| dw(i, 1)).collect();
        let mut st = RefreshState::default();
        let mut seen = HashSet::new();
        let mut n = now();
        for _ in 0..10 {
            for g in pick(&candidates, &HashMap::new(), 0, &st, 4, n) {
                st.asked(&g.key_id, "core", n);
                seen.insert(g.key_id);
            }
            n += Duration::seconds(3);
        }
        assert_eq!(seen.len(), 40);
    }

    #[test]
    fn observed_at_must_be_within_the_window_and_skew() {
        let n = now();
        assert!(observed_at_in_range(n, n));
        assert!(observed_at_in_range(n - Duration::seconds(599), n));
        assert!(!observed_at_in_range(n - Duration::seconds(601), n));
        assert!(observed_at_in_range(n + Duration::seconds(59), n));
        assert!(!observed_at_in_range(n + Duration::seconds(61), n));
        assert!(!observed_at_in_range(n + Duration::days(3650), n));
    }
}
