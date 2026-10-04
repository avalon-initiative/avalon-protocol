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
use crate::witness_refresh::{self, Attempt, Backoff};

/// How often the worker looks at its own newest head.
pub const AUTHOR_GATHER_INTERVAL: Duration = Duration::from_secs(5);
/// Witnesses asked per tick.
pub const MAX_ASKED_PER_TICK: usize = 16;

/// A stored cosignature this old is asked for again; witnesses re-attest every third of the
/// window, so this follows their cadence.
fn refresh_after() -> time::Duration {
    time::Duration::seconds((COSIGNATURE_FRESHNESS_WINDOW.as_secs() / 3) as i64)
}

/// In-memory backoff and last logged outcome per witness.
#[derive(Debug, Default)]
pub struct AuthorGatherState {
    backoff: Backoff,
    last_outcome: HashMap<String, String>,
}

/// Known-list witnesses to ask this tick: reachable, not in backoff, and without a stored
/// cosignature or with one at least [`refresh_after`] old. Never-held first, then stalest.
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
        .filter(|(id, _)| state.backoff.ready(id, shard_id, now))
        .filter(|(id, _)| held.get(id).is_none_or(|at| now - *at >= refresh_after()))
        .collect();
    wanted.sort_by(|a, b| held.get(&a.0).cmp(&held.get(&b.0)).then(a.0.cmp(&b.0)));
    wanted
        .into_iter()
        .take(MAX_ASKED_PER_TICK)
        .cloned()
        .collect()
}

/// A witness that has not mirrored the head yet is a short wait, not a transport failure.
fn attempt_for(outcome: &GatherOutcome, now: OffsetDateTime) -> Attempt {
    match outcome {
        GatherOutcome::BadStatus(404) => Attempt::NotAvailable,
        other => witness_refresh::attempt_for(other, now),
    }
}

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
    let sources = cosign_gather::witness_sources(&others, peers);
    let wanted = select_wanted(&others, &sources, &held, state, own_shard_id, now);
    if wanted.is_empty() {
        return 0;
    }
    let results =
        cosign_gather::gather_own_cosignatures(policy, &wanted, &sources, &sth, own_shard_id).await;
    let mut written = 0;
    for (key_id, outcome) in results {
        let mut attempt = attempt_for(&outcome, now);
        let label = match &outcome {
            GatherOutcome::Fetched(cosig) if attempt == Attempt::Ok => {
                if held.get(&key_id).is_some_and(|at| *at >= cosig.observed_at) {
                    attempt = Attempt::NotAvailable;
                    "not newer".to_string()
                } else {
                    match chain.store_witness_cosignature(own_shard_id, cosig).await {
                        Ok(()) => {
                            written += 1;
                            "stored".to_string()
                        }
                        Err(err) => {
                            attempt = Attempt::NotAvailable;
                            format!("store refused: {err}")
                        }
                    }
                }
            }
            other => other.label(),
        };
        state.backoff.record(&key_id, own_shard_id, attempt, now);
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
    written
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
        state.backoff.retain_live(&live);
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
