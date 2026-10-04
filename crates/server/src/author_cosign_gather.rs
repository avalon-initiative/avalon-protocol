//! Keeps the known-list witnesses' cosignatures over this node's own newest head stored and
//! fresh. An authoring node never mirrors its own shard, so without this it would serve only its
//! own cosignature while mirrors serve the full set (#1199).

use std::collections::HashMap;
use std::time::Duration;

use avalon_chain::PostgresSettlementProvider;
use ed25519_dalek::VerifyingKey;
use time::OffsetDateTime;

use crate::cosign_gather::{self, GatherOutcome};
use crate::cosign_verify::COSIGNATURE_FRESHNESS_WINDOW;
use crate::nodes::PeerInfo;
use crate::outbound_policy::OutboundPolicy;
use crate::witness_refresh::{self, refresh_after, Attempt, RefreshState};

/// How often the worker looks at its own newest head.
pub const AUTHOR_GATHER_INTERVAL: Duration = Duration::from_secs(5);
/// Witnesses asked per tick.
pub const MAX_ASKED_PER_TICK: usize = 16;

/// In-memory backoff and last logged outcome per witness.
#[derive(Debug, Default)]
pub struct AuthorGatherState {
    refresh: RefreshState,
    last_outcome: HashMap<String, String>,
}

/// Known-list witnesses to ask this tick: reachable, not in backoff, and without a stored
/// cosignature or with one at least [`refresh_after`] old.
pub fn select_wanted(
    known_list: &[(String, VerifyingKey)],
    sources: &[cosign_gather::WitnessSource],
    held: &HashMap<String, OffsetDateTime>,
    state: &AuthorGatherState,
    shard_id: &str,
    now: OffsetDateTime,
) -> Vec<(String, VerifyingKey)> {
    let mut wanted: Vec<&(String, VerifyingKey)> = known_list
        .iter()
        .filter(|(id, _)| sources.iter().any(|s| s.key_id == *id))
        .filter(|(id, _)| state.refresh.backoff.ready(id, shard_id, now))
        .filter(|(id, _)| held.get(id).is_none_or(|at| now - *at >= refresh_after()))
        .collect();
    // Never-held first, then stalest; ties go to whoever was asked longest ago so witnesses
    // stuck in short retries cannot hold every slot.
    wanted.sort_by(|a, b| {
        held.get(&a.0)
            .cmp(&held.get(&b.0))
            .then_with(|| {
                state
                    .refresh
                    .last_asked(&a.0, shard_id)
                    .cmp(&state.refresh.last_asked(&b.0, shard_id))
            })
            .then(a.0.cmp(&b.0))
    });
    wanted
        .into_iter()
        .take(MAX_ASKED_PER_TICK)
        .cloned()
        .collect()
}

/// Whether the read-back holds `signature` for `key_id` over `sth`. No read-back (the read
/// failed) trusts the store's own success.
fn confirmed_stored(
    readback: Option<&[avalon_protocol::witness::WitnessCosignature]>,
    key_id: &str,
    sth: &avalon_protocol::sth::SignedTreeHead,
    signature: &str,
) -> bool {
    readback.is_none_or(|rows| {
        rows.iter().any(|c| {
            c.witness_key_id == key_id
                && c.root_hash == sth.root_hash
                && c.author_created_at == sth.created_at
                && c.signature == signature
        })
    })
}

/// A witness's outcome awaiting read-back: (key id, attempt, label, stored time and signature).
type Staged = (String, Attempt, String, Option<(OffsetDateTime, String)>);

/// One pass: fetches and stores cosignatures for this node's newest head. Returns how many were
/// stored or refreshed. Every failure is logged and skipped.
pub async fn gather_once(
    chain: &PostgresSettlementProvider,
    policy: OutboundPolicy,
    known_list: &[(String, VerifyingKey)],
    peers: &[PeerInfo],
    own_shard_id: &str,
    state: &mut AuthorGatherState,
    now: OffsetDateTime,
) -> usize {
    let sth = match chain.latest_signed_tree_head().await {
        Ok(Some(sth)) => sth,
        Ok(None) => return 0,
        Err(err) => {
            tracing::error!(error = %err, "author-cosign-gather: failed to read own latest head");
            return 0;
        }
    };
    let stored = match chain
        .list_witness_cosignatures(chain.network_id(), own_shard_id, sth.tree_size)
        .await
    {
        Ok(list) => list,
        Err(err) => {
            tracing::error!(error = %err, "author-cosign-gather: failed to read stored cosignatures");
            return 0;
        }
    };
    let held: HashMap<String, OffsetDateTime> = stored
        .iter()
        .filter(|c| c.root_hash == sth.root_hash && c.author_created_at == sth.created_at)
        .map(|c| (c.witness_key_id.clone(), c.observed_at))
        .collect();
    // A witness holding the author's own key is credited without a cosignature.
    let others: Vec<(String, VerifyingKey)> = known_list
        .iter()
        .filter(|(_, key)| !avalon_protocol::sth::verify_tree_head(key, &sth))
        .cloned()
        .collect();
    let mut sources = cosign_gather::witness_sources(&others, peers);
    let mut wanted = select_wanted(&others, &sources, &held, state, own_shard_id, now);
    // Witnesses known from the peer directory but not yet in the confirmed known list (a new
    // node's list stays short through probation) are asked too, within the mirror-side caps.
    let max_extra = witness_refresh::max_per_tick_from_env();
    let directory_cap = max_extra * witness_refresh::ROW_CAP_FACTOR;
    let directory: Vec<_> = witness_refresh::directory_witnesses(peers, known_list, None, now)
        .into_iter()
        .filter(|w| !avalon_protocol::sth::verify_tree_head(&w.key, &sth))
        .filter(|w| {
            held.get(&w.key_id)
                .is_none_or(|at| now - *at >= refresh_after())
        })
        .collect();
    let outside_known_list = stored
        .iter()
        .filter(|c| !known_list.iter().any(|(id, _)| *id == c.witness_key_id))
        .count();
    let held_directory: HashMap<String, OffsetDateTime> = held
        .iter()
        .filter(|(id, _)| !known_list.iter().any(|(k, _)| k == *id))
        .map(|(id, at)| (id.clone(), *at))
        .collect();
    for w in witness_refresh::select(
        &directory,
        own_shard_id,
        &held_directory,
        outside_known_list,
        &state.refresh,
        max_extra,
        now,
    ) {
        wanted.push((w.key_id.clone(), w.key));
        sources.push(w.source());
    }
    if wanted.is_empty() {
        report_shortfall(state, known_list, &sth, &held, &HashMap::new(), now);
        return 0;
    }
    let results =
        cosign_gather::gather_own_cosignatures(policy, &wanted, &sources, &sth, own_shard_id).await;
    // Phase 1: store what verified and is newer than what is held.
    let mut staged: Vec<Staged> = Vec::new();
    for (key_id, outcome) in results {
        let mut attempt = witness_refresh::attempt_for(&outcome, now);
        let mut applied = None;
        let label = match &outcome {
            GatherOutcome::Fetched(cosig) if attempt == Attempt::Ok => {
                if held.get(&key_id).is_some_and(|at| *at >= cosig.observed_at) {
                    attempt = Attempt::NotAvailable;
                    "not newer".to_string()
                } else {
                    match chain.store_witness_cosignature(own_shard_id, cosig).await {
                        Ok(()) => {
                            applied = Some(cosig.clone());
                            "stored".to_string()
                        }
                        Err(err) => {
                            attempt = Attempt::NotAvailable;
                            format!("store refused: {err}")
                        }
                    }
                }
            }
            GatherOutcome::Fetched(_) => "observed_at out of range".to_string(),
            other => other.label(),
        };
        if matches!(
            outcome,
            GatherOutcome::BadStatus(404) | GatherOutcome::HeadMismatch
        ) {
            attempt = witness_refresh::lagging_if_young(attempt, sth.created_at, now);
        }
        staged.push((
            key_id,
            attempt,
            label,
            applied.map(|c| (c.observed_at, c.signature)),
        ));
    }
    // A store that returns Ok can still leave an older row in place (a different author
    // timestamp), so only what reads back as written counts.
    let now_stored = chain
        .list_witness_cosignatures(chain.network_id(), own_shard_id, sth.tree_size)
        .await
        .ok();
    let mut written = 0;
    let mut gained: HashMap<String, OffsetDateTime> = HashMap::new();
    for (key_id, mut attempt, mut label, applied) in staged {
        if let Some((at, signature)) = applied {
            let in_place = confirmed_stored(now_stored.as_deref(), &key_id, &sth, &signature);
            if in_place {
                written += 1;
                gained.insert(key_id.clone(), at);
            } else {
                attempt = Attempt::NotAvailable;
                label = "not applied".to_string();
            }
        }
        state
            .refresh
            .backoff
            .record(&key_id, own_shard_id, attempt, now);
        state.refresh.asked(&key_id, own_shard_id, now);
        if attempt == Attempt::Ok {
            state
                .refresh
                .delivered(&key_id, own_shard_id, directory_cap);
        }
        if state.last_outcome.get(&key_id) != Some(&label) {
            tracing::info!(
                shard_id = own_shard_id,
                tree_size = sth.tree_size,
                witness = &key_id[..key_id.len().min(8)],
                outcome = %label,
                "author-cosign-gather: witness cosignature outcome changed",
            );
            if state.last_outcome.len() >= 1024 {
                state.last_outcome.clear();
            }
            state.last_outcome.insert(key_id, label);
        }
    }
    report_shortfall(state, known_list, &sth, &held, &gained, now);
    written
}

/// Logs (on change only) when the stored fresh cosignatures for the head fall short of a
/// majority of the known list, so a node serving a short set is visible to its operator.
fn report_shortfall(
    state: &mut AuthorGatherState,
    known_list: &[(String, VerifyingKey)],
    sth: &avalon_protocol::sth::SignedTreeHead,
    held: &HashMap<String, OffsetDateTime>,
    gained: &HashMap<String, OffsetDateTime>,
    now: OffsetDateTime,
) {
    if known_list.len() < 2 {
        return;
    }
    let cutoff = now - time::Duration::seconds(COSIGNATURE_FRESHNESS_WINDOW.as_secs() as i64);
    let have = known_list
        .iter()
        .filter(|(id, key)| {
            avalon_protocol::sth::verify_tree_head(key, sth)
                || gained
                    .get(id)
                    .or(held.get(id))
                    .is_some_and(|at| *at >= cutoff)
        })
        .count();
    let need = avalon_protocol::witness::majority_threshold(known_list.len());
    let label = format!("{}", have < need);
    if state.last_outcome.get("").map(String::as_str) == Some(label.as_str()) {
        return;
    }
    if have < need {
        tracing::warn!(
            tree_size = sth.tree_size,
            fresh = have,
            needed = need,
            known_list = known_list.len(),
            "author-cosign-gather: own head is served with fewer fresh cosignatures than a majority",
        );
    } else {
        tracing::info!(
            tree_size = sth.tree_size,
            "author-cosign-gather: own head has a fresh majority"
        );
    }
    state.last_outcome.insert(String::new(), label);
}

/// Runs [`gather_once`] on a fixed interval, re-reading the known list and peer table every
/// tick. Never returns.
pub async fn run_worker(
    chain: PostgresSettlementProvider,
    known_list: crate::known_list::KnownListHandle,
    peers: crate::nodes::PeerTable,
    own_shard_id: String,
) {
    let policy = OutboundPolicy::from_env();
    let mut state = AuthorGatherState::default();
    let mut ticker = tokio::time::interval(AUTHOR_GATHER_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let pairs = crate::cosign_verify::known_list_verifying_keys(&known_list);
        let live: std::collections::HashSet<&str> =
            pairs.iter().map(|(id, _)| id.as_str()).collect();
        state.refresh.retain_live(&live);
        gather_once(
            &chain,
            policy,
            &pairs,
            &peers.list_all(),
            &own_shard_id,
            &mut state,
            OffsetDateTime::now_utc(),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn witness(n: u8) -> (String, VerifyingKey) {
        let key = SigningKey::from_bytes(&[n; 32]).verifying_key();
        (hex::encode(key.to_bytes()), key)
    }

    fn source(id: &str) -> cosign_gather::WitnessSource {
        cosign_gather::WitnessSource {
            key_id: id.to_string(),
            base_url: format!("http://{}", &id[..4]),
        }
    }

    fn pick(
        list: &[(String, VerifyingKey)],
        held: &HashMap<String, OffsetDateTime>,
        state: &AuthorGatherState,
        now: OffsetDateTime,
    ) -> Vec<String> {
        let sources: Vec<_> = list.iter().map(|(id, _)| source(id)).collect();
        select_wanted(list, &sources, held, state, "core", now)
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    #[test]
    fn a_missing_cosignature_is_asked_for_and_a_fresh_one_is_left_alone() {
        let now = OffsetDateTime::now_utc();
        let list = vec![witness(1), witness(2)];
        let held = HashMap::from([(list[0].0.clone(), now - time::Duration::seconds(10))]);
        let wanted = pick(&list, &held, &AuthorGatherState::default(), now);
        assert_eq!(wanted, vec![list[1].0.clone()]);
    }

    #[test]
    fn a_cosignature_older_than_a_third_of_the_window_is_asked_for_again() {
        let now = OffsetDateTime::now_utc();
        let list = vec![witness(1)];
        let old = now - refresh_after() - time::Duration::seconds(1);
        let held = HashMap::from([(list[0].0.clone(), old)]);
        assert_eq!(
            pick(&list, &held, &AuthorGatherState::default(), now).len(),
            1
        );
    }

    #[test]
    fn a_witness_without_a_source_is_not_asked() {
        let now = OffsetDateTime::now_utc();
        let list = vec![witness(1)];
        let wanted = select_wanted(
            &list,
            &[],
            &HashMap::new(),
            &AuthorGatherState::default(),
            "core",
            now,
        );
        assert!(wanted.is_empty());
    }

    #[test]
    fn a_witness_in_backoff_is_skipped_until_it_expires() {
        let now = OffsetDateTime::now_utc();
        let list = vec![witness(1)];
        let mut state = AuthorGatherState::default();
        state
            .refresh
            .backoff
            .record(&list[0].0, "core", Attempt::Transport, now);
        assert!(pick(&list, &HashMap::new(), &state, now).is_empty());
        let later = now + time::Duration::minutes(5);
        assert_eq!(pick(&list, &HashMap::new(), &state, later).len(), 1);
    }

    #[test]
    fn the_per_tick_cap_keeps_the_never_held_and_stalest_first() {
        let now = OffsetDateTime::now_utc();
        let list: Vec<_> = (1..=(MAX_ASKED_PER_TICK as u8 + 4)).map(witness).collect();
        let stale = now - refresh_after() - time::Duration::seconds(5);
        let held: HashMap<_, _> = list
            .iter()
            .take(MAX_ASKED_PER_TICK)
            .map(|(id, _)| (id.clone(), stale))
            .collect();
        let wanted = pick(&list, &held, &AuthorGatherState::default(), now);
        assert_eq!(wanted.len(), MAX_ASKED_PER_TICK);
        for (id, _) in list.iter().skip(MAX_ASKED_PER_TICK) {
            assert!(
                wanted.contains(id),
                "never-held witness must be asked first"
            );
        }
    }

    #[test]
    fn stuck_witnesses_in_short_retries_cannot_starve_healthy_ones() {
        let mut all: Vec<_> = (1..=24u8).map(witness).collect();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        let (stuck, healthy) = all.split_at(20);
        let sources: Vec<_> = all.iter().map(|(id, _)| source(id)).collect();
        let mut state = AuthorGatherState::default();
        let mut held = HashMap::new();
        let mut now = OffsetDateTime::now_utc();
        let mut healthy_asked_at_tick = None;
        for tick in 0..4 {
            now += time::Duration::seconds(5);
            let wanted = select_wanted(&all, &sources, &held, &state, "core", now);
            assert!(wanted.len() <= MAX_ASKED_PER_TICK);
            for (id, _) in &wanted {
                state.refresh.asked(id, "core", now);
                if stuck.iter().any(|(s, _)| s == id) {
                    state
                        .refresh
                        .backoff
                        .record(id, "core", Attempt::Lagging, now);
                } else {
                    held.insert(id.clone(), now);
                    healthy_asked_at_tick.get_or_insert(tick);
                }
            }
        }
        assert!(
            healthy_asked_at_tick.is_some_and(|t| t <= 1),
            "healthy witnesses must be reached within two ticks"
        );
        assert!(healthy.iter().all(|(id, _)| held.contains_key(id)));
    }

    #[test]
    fn a_failed_readback_trusts_the_store_and_a_missing_row_does_not() {
        use ed25519_dalek::SigningKey;
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let created = OffsetDateTime::now_utc();
        let sth =
            avalon_protocol::sth::sign_tree_head(&key, "k", 4, &"ab".repeat(32), "n", created);
        let cosig = avalon_protocol::witness::sign_witness_cosignature(
            &key,
            "w",
            4,
            &sth.root_hash,
            "n",
            created,
            created,
        );
        assert!(confirmed_stored(None, "w", &sth, &cosig.signature));
        assert!(!confirmed_stored(Some(&[]), "w", &sth, &cosig.signature));
        assert!(confirmed_stored(
            Some(std::slice::from_ref(&cosig)),
            "w",
            &sth,
            &cosig.signature
        ));
    }
}
