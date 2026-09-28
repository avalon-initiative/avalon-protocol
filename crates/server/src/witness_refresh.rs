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

/// Per-witness failure backoff, in memory only.
#[derive(Debug, Default)]
pub struct Backoff {
    entries: HashMap<String, (u32, OffsetDateTime)>,
}

impl Backoff {
    pub fn ready(&self, key_id: &str, now: OffsetDateTime) -> bool {
        self.entries
            .get(key_id)
            .is_none_or(|(_, next)| *next <= now)
    }

    /// Records an attempt; failures double the wait from 30 s up to 30 min, success clears it.
    pub fn record(&mut self, key_id: &str, ok: bool, now: OffsetDateTime) {
        if ok {
            self.entries.remove(key_id);
            return;
        }
        let failures = self
            .entries
            .get(key_id)
            .map_or(0, |(n, _)| *n)
            .saturating_add(1);
        let wait = (BACKOFF_BASE * 2i32.pow(failures.saturating_sub(1).min(6))).min(BACKOFF_CAP);
        self.entries
            .insert(key_id.to_string(), (failures, now + wait));
    }

    /// Forgets witnesses no longer in the directory, and bounds the map.
    pub fn retain_live(&mut self, live: &HashSet<&str>) {
        self.entries.retain(|k, _| live.contains(k.as_str()));
        if self.entries.len() > MAX_BACKOFF_ENTRIES {
            self.entries.clear();
        }
    }
}

/// Which directory witnesses to ask for one head this tick.
///
/// Witnesses already holding a stored cosignature for the head come first, stalest first, so a
/// limit smaller than the set still rotates through all of them. Witnesses without one follow,
/// most recently announced first, and are admitted only while the stored count of cosignatures
/// from outside the known list stays under the row cap. Witnesses in backoff are skipped without
/// using a slot.
pub fn select(
    candidates: &[DirectoryWitness],
    stored: &HashMap<String, OffsetDateTime>,
    stored_outside_known_list: usize,
    backoff: &Backoff,
    max_per_tick: usize,
    now: OffsetDateTime,
) -> Vec<DirectoryWitness> {
    let ready: Vec<&DirectoryWitness> = candidates
        .iter()
        .filter(|c| backoff.ready(&c.key_id, now))
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
    let mut fresh: Vec<&DirectoryWitness> = ready
        .iter()
        .copied()
        .filter(|c| !stored.contains_key(&c.key_id))
        .collect();
    fresh.sort_by(|a, b| {
        b.announced_at
            .cmp(&a.announced_at)
            .then_with(|| a.key_id.cmp(&b.key_id))
    });
    let room = (max_per_tick * ROW_CAP_FACTOR).saturating_sub(stored_outside_known_list);
    held.into_iter()
        .chain(fresh.into_iter().take(room))
        .take(max_per_tick)
        .cloned()
        .collect()
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
    fn backoff_doubles_caps_and_clears_on_success() {
        let n = now();
        let mut b = Backoff::default();
        assert!(b.ready("w", n));
        b.record("w", false, n);
        assert!(!b.ready("w", n + Duration::seconds(29)));
        assert!(b.ready("w", n + Duration::seconds(31)));
        b.record("w", false, n);
        assert!(!b.ready("w", n + Duration::seconds(59)));
        for _ in 0..20 {
            b.record("w", false, n);
        }
        assert!(!b.ready("w", n + Duration::minutes(29)));
        assert!(b.ready("w", n + Duration::minutes(31)));
        b.record("w", true, n);
        assert!(b.ready("w", n));
    }

    #[test]
    fn backoff_forgets_departed_witnesses() {
        let n = now();
        let mut b = Backoff::default();
        b.record("gone", false, n);
        b.record("here", false, n);
        b.retain_live(&HashSet::from(["here"]));
        assert!(b.ready("gone", n));
        assert!(!b.ready("here", n));
    }

    #[test]
    fn selection_is_bounded_by_the_per_tick_limit() {
        let candidates: Vec<_> = (1..=40u8).map(|n| dw(n, n as i64)).collect();
        let got = select(
            &candidates,
            &HashMap::new(),
            0,
            &Backoff::default(),
            16,
            now(),
        );
        assert_eq!(got.len(), 16);
    }

    #[test]
    fn stored_witnesses_come_first_stalest_first_then_newest_announced() {
        let n = now();
        let candidates = vec![dw(1, 100), dw(2, 50), dw(3, 10), dw(4, 5)];
        let stored = HashMap::from([
            (candidates[0].key_id.clone(), n - Duration::seconds(30)),
            (candidates[1].key_id.clone(), n - Duration::seconds(900)),
        ]);
        let got = select(&candidates, &stored, 2, &Backoff::default(), 3, n);
        let ids: Vec<_> = got.iter().map(|g| g.key_id.clone()).collect();
        assert_eq!(
            ids,
            vec![
                candidates[1].key_id.clone(),
                candidates[0].key_id.clone(),
                candidates[3].key_id.clone(),
            ]
        );
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
            let got = select(&candidates, &stored, 3, &Backoff::default(), 1, n);
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
        let got = select(
            &candidates,
            &HashMap::new(),
            cap,
            &Backoff::default(),
            4,
            now(),
        );
        assert!(got.is_empty());
        let got = select(
            &candidates,
            &HashMap::new(),
            cap - 2,
            &Backoff::default(),
            4,
            now(),
        );
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn witnesses_in_backoff_are_skipped_without_using_a_slot() {
        let n = now();
        let candidates = vec![dw(1, 1), dw(2, 2), dw(3, 3)];
        let mut b = Backoff::default();
        b.record(&candidates[0].key_id, false, n);
        let got = select(&candidates, &HashMap::new(), 0, &b, 2, n);
        let ids: Vec<_> = got.iter().map(|g| g.key_id.clone()).collect();
        assert_eq!(
            ids,
            vec![candidates[1].key_id.clone(), candidates[2].key_id.clone()]
        );
    }
}
