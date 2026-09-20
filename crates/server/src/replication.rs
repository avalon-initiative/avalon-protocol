//! Minimum replication guarantee for new identity registrations — issue
//! #629, implementing #622's decision. A shard with exactly one operator
//! has zero durability guarantee beyond that operator's continued
//! goodwill and uptime; this extends #569's already-built
//! archive-confirmation mechanism ("never prune what nothing else
//! retains") to a new trigger point: a shard that hasn't yet accumulated
//! enough independently-confirmed mirrors is not eligible to accept *new*
//! identity registrations. Identities already registered on a gated
//! shard are never affected — see [`registration_eligible`]'s own doc
//! comment and its one call site, `crate::handlers::register_start`.
//!
//! **Confirmation source of truth.** Reuses the exact signal #569 already
//! established: a peer's own `GET /ledger/mirror-progress`
//! (`crate::settlement::mirror_progress`). #569 asks "has this peer
//! mirrored *my own* history far enough to prune past some boundary
//! `seq`"; this module asks a narrower, cheaper question instead — "has
//! this peer mirrored *anything at all* for this shard" (`last_seq > 0`)
//! — since a registration-eligibility gate only needs to know a shard has
//! *some* independent copy elsewhere, not that every peer is fully caught
//! up. [`run_worker`] is the background poller that produces that answer
//! by actually asking every known peer (`crate::nodes::PeerTable`, #362)
//! about every known shard (this node's own `own_shard_id` plus whatever
//! `crate::nodes::ShardRegistry` has gossiped in, #599), storing the
//! result in [`MirrorConfirmationRegistry`] — nothing here ever trusts a
//! `POST`ed claim; every confirmation is this node's own independent HTTP
//! call to the peer being credited.
//!
//! **The bootstrap problem.** A brand-new, legitimately single-operator
//! shard hasn't had time to attract a mirror yet. [`registration_eligible`]
//! exempts a shard from the gate entirely while it's younger than
//! `AVALON_MIRROR_GRACE_PERIOD_HOURS` (default 24h) — "how old" comes
//! from `crate::nodes::ShardRegistry::first_seen_at`, the earliest
//! `last_seen_at` this node has ever recorded for that shard, across both
//! its own `record_own` claims and peer gossip. Once past the grace
//! period, the gate applies normally: at least
//! `AVALON_MIN_MIRROR_CONFIRMATIONS` (default 1) distinct, recently
//! (within [`CONFIRMATION_FRESHNESS_WINDOW`]) confirmed mirrors.
//!
//! **v1 simplification, deliberate.** One configured minimum applies
//! uniformly to every shard, regardless of whether it's a `core`-like,
//! identity-bearing shard or an individual integrator's own dedicated
//! shard — no per-shard-type config surface exists yet; the ticket this
//! implements explicitly called this out as acceptable for a first pass,
//! not an oversight. Add per-shard-type configuration if a real need for
//! it shows up.
//!
//! **Known limitation, documented rather than silently accepted**:
//! [`crate::nodes::ShardRegistry`] (and therefore `first_seen_at`) is
//! in-process/non-durable, same posture every other piece of gossip state
//! in `crate::nodes` already takes — a shard's *apparent* age resets to
//! "unknown" (treated as within grace period, see
//! [`within_grace_period`]'s own doc comment) on this node's own restart.
//! A restart therefore can't wrongly lock a shard out, only briefly
//! re-extend its grace period — the safe direction for this specific
//! failure mode to lean.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::Deserialize;
use time::OffsetDateTime;

/// Default for `AVALON_MIN_MIRROR_CONFIRMATIONS` — issue #363's
/// "hoster-configurable, defaults to a real, small value" pattern. One
/// independently-confirmed mirror is the minimum that actually means
/// "this shard's data survives its own operator disappearing," which is
/// the entire point of #622's decision — `0` would silently turn the gate
/// into a no-op.
const DEFAULT_MIN_MIRROR_CONFIRMATIONS: usize = 1;

/// Default for `AVALON_MIRROR_GRACE_PERIOD_HOURS` — long enough that a
/// legitimately new shard's operator has a real day to get a mirror
/// running (announce to a peer, have that peer opt into mirroring it,
/// let it actually poll and confirm) before the gate can ever apply,
/// short enough that "grace period" never becomes a permanent loophole.
const DEFAULT_GRACE_PERIOD_HOURS: i64 = 24;

/// Default for `AVALON_REPLICATION_POLL_INTERVAL_SECS` — not latency
/// sensitive (a confirmed-mirror count changes slowly), so this defaults
/// considerably coarser than #362's own announce interval.
const DEFAULT_POLL_INTERVAL_SECS: u64 = 300;

/// A confirmation is no longer counted once it's this old without being
/// refreshed — generous relative to [`DEFAULT_POLL_INTERVAL_SECS`] (a
/// handful of missed ticks in a row) so a transient blip never flips a
/// shard from eligible to ineligible. Deliberately a fixed constant
/// rather than derived from the configured poll interval: an operator who
/// raises `AVALON_REPLICATION_POLL_INTERVAL_SECS` well past this window
/// will see confirmations read as stale between polls — a real trade-off
/// (gating on a possibly-stale count is safer than treating a stale
/// confirmation as still live), not a bug, and worth knowing before
/// raising that interval far past the default.
pub const CONFIRMATION_FRESHNESS_WINDOW: time::Duration = time::Duration::minutes(15);

/// This node's own record of which distinct peers have confirmed
/// mirroring which shard, and how recently — issue #629. In-process only,
/// same non-durable posture `crate::nodes::PeerTable`/`ShardRegistry`
/// already take: it repopulates from scratch within a few poll ticks
/// after a restart, fed by [`run_worker`] actually polling peers, never
/// by anything a peer can simply assert about itself.
#[derive(Clone, Default)]
pub struct MirrorConfirmationRegistry {
    // shard_id -> (peer_base_url -> confirmed_at)
    confirmations: Arc<RwLock<HashMap<String, HashMap<String, OffsetDateTime>>>>,
}

impl MirrorConfirmationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that `peer_base_url` was independently observed (by this
    /// node, via its own HTTP call — see [`run_worker`]) mirroring
    /// `shard_id` as of `at`.
    pub fn record_confirmation(&self, shard_id: &str, peer_base_url: &str, at: OffsetDateTime) {
        self.confirmations
            .write()
            .expect("mirror confirmation registry lock poisoned")
            .entry(shard_id.to_string())
            .or_default()
            .insert(peer_base_url.to_string(), at);
    }

    /// How many distinct peers have confirmed mirroring `shard_id` at or
    /// after `not_before` — a caller passes `now - CONFIRMATION_FRESHNESS_WINDOW`
    /// to get "currently confirmed," same shape
    /// `crate::nodes::PeerTable::prune_older_than`'s cutoff parameter
    /// already establishes elsewhere in this module's sibling.
    pub fn confirmed_count(&self, shard_id: &str, not_before: OffsetDateTime) -> usize {
        self.confirmations
            .read()
            .expect("mirror confirmation registry lock poisoned")
            .get(shard_id)
            .map(|peers| peers.values().filter(|&&at| at >= not_before).count())
            .unwrap_or(0)
    }

    /// Drops any confirmation not refreshed since `cutoff` — same
    /// decay-not-forever posture `crate::nodes::ShardRegistry::prune_older_than`
    /// already takes.
    pub fn prune_older_than(&self, cutoff: OffsetDateTime) {
        let mut confirmations = self
            .confirmations
            .write()
            .expect("mirror confirmation registry lock poisoned");
        confirmations.retain(|_, peers| {
            peers.retain(|_, at| *at >= cutoff);
            !peers.is_empty()
        });
    }
}

/// Pure grace-period check, split out for direct unit testing — same
/// "pure function behind the env-reading wrapper" pattern
/// `crate::recovery`'s guard functions establish. `first_seen_at` of
/// `None` (this node has never recorded an age for the shard at all —
/// e.g. right after a restart, before this node's own announce loop or a
/// peer's gossip has run even once) is treated as *within* the grace
/// period: a shard whose age genuinely can't be determined yet is never
/// wrongly gated, only ever briefly, harmlessly exempted instead.
pub fn within_grace_period(
    first_seen_at: Option<OffsetDateTime>,
    now: OffsetDateTime,
    grace_period: time::Duration,
) -> bool {
    match first_seen_at {
        Some(first_seen_at) => now - first_seen_at < grace_period,
        None => true,
    }
}

/// Issue #629's actual gate: a shard is eligible for a *new* identity
/// registration when either it's still within its bootstrap grace
/// period, or it has at least `min_confirmations` distinct, currently-fresh
/// confirmed mirrors. Never disrupts an identity already registered on
/// the shard — this is only ever consulted at the start of a brand-new
/// registration (`crate::handlers::register_start`), never at login or
/// any other read/write for an identity that already exists there.
pub fn registration_eligible(
    first_seen_at: Option<OffsetDateTime>,
    now: OffsetDateTime,
    grace_period: time::Duration,
    confirmed_mirror_count: usize,
    min_confirmations: usize,
) -> bool {
    within_grace_period(first_seen_at, now, grace_period)
        || confirmed_mirror_count >= min_confirmations
}

/// This node's resolved #629 gate configuration — read once at startup
/// (`AppState::replication_gate`), same "parse env once, not per request"
/// posture every other `AppState` config field already takes.
#[derive(Clone, Debug)]
pub struct ReplicationGateConfig {
    pub min_confirmations: usize,
    pub grace_period: time::Duration,
}

impl ReplicationGateConfig {
    pub fn from_env() -> Self {
        let min_confirmations = std::env::var("AVALON_MIN_MIRROR_CONFIRMATIONS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(DEFAULT_MIN_MIRROR_CONFIRMATIONS);

        let grace_period_hours = std::env::var("AVALON_MIRROR_GRACE_PERIOD_HOURS")
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .filter(|hours| *hours >= 0)
            .unwrap_or(DEFAULT_GRACE_PERIOD_HOURS);

        Self {
            min_confirmations,
            grace_period: time::Duration::hours(grace_period_hours),
        }
    }
}

/// [`run_worker`]'s own poll cadence, resolved once at startup.
pub struct ReplicationConfig {
    pub poll_interval: Duration,
}

impl ReplicationConfig {
    pub fn from_env() -> Self {
        let poll_interval = std::env::var("AVALON_REPLICATION_POLL_INTERVAL_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|secs| *secs > 0)
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS));
        Self { poll_interval }
    }
}

#[derive(Deserialize)]
struct MirrorProgressBody {
    last_seq: i64,
}

/// Asks `peer_base_url`'s own `GET /ledger/mirror-progress` whether it has
/// mirrored anything at all (`last_seq > 0`) for `shard_id` under
/// `network_id`. Any failure (unreachable, non-2xx, unparseable body)
/// counts as "not confirmed," same posture
/// `crate::retention::peer_confirms_coverage` already takes for the exact
/// same reason: one flaky/unreachable peer must never itself become a
/// reason every other peer's confirmation is ignored, and must never turn
/// into a hard error for the caller either.
async fn peer_confirms_mirroring(
    client: &reqwest::Client,
    peer_base_url: &str,
    network_id: &str,
    shard_id: &str,
) -> bool {
    let url = format!("{peer_base_url}/ledger/mirror-progress");
    match client
        .get(&url)
        .query(&[("network_id", network_id), ("shard_id", shard_id)])
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            match response.json::<MirrorProgressBody>().await {
                Ok(body) => body.last_seq > 0,
                Err(_) => false,
            }
        }
        _ => false,
    }
}

/// Runs forever, re-polling every known peer for every known shard's
/// mirror progress on [`ReplicationConfig::poll_interval`], populating
/// `confirmations` — the background piece that actually keeps
/// [`registration_eligible`]'s `confirmed_mirror_count` input current.
/// Spawned unconditionally at startup (see `main.rs`), the same posture
/// `crate::nodes::run_worker` already takes for the peer table itself:
/// even a node with no peers known yet still needs this loop running so
/// it picks up peers (and therefore confirmations) the moment any appear,
/// with no restart required.
///
/// Polls `own_shard_id` (this node's own authored shard, if it has one)
/// plus every `shard_id` currently in `shard_registry` — a node has no
/// reason to track confirmed-mirror counts for a shard it neither authors
/// nor has heard of.
pub async fn run_worker(
    network_id: String,
    peers: crate::nodes::PeerTable,
    shard_registry: crate::nodes::ShardRegistry,
    confirmations: MirrorConfirmationRegistry,
    own_shard_id: String,
    config: ReplicationConfig,
) {
    let client = reqwest::Client::new();
    loop {
        let mut shard_ids = shard_registry.known_shard_ids();
        shard_ids.insert(own_shard_id.clone());

        let known_peers = peers.list_all();
        for shard_id in &shard_ids {
            for peer in &known_peers {
                if peer_confirms_mirroring(&client, &peer.base_url, &network_id, shard_id).await {
                    confirmations.record_confirmation(
                        shard_id,
                        &peer.base_url,
                        OffsetDateTime::now_utc(),
                    );
                    tracing::debug!(
                        event = "mirror_confirmed",
                        shard_id = %shard_id,
                        peer = %peer.base_url,
                        "replication worker: peer confirmed mirroring this shard",
                    );
                }
            }
        }

        confirmations.prune_older_than(OffsetDateTime::now_utc() - CONFIRMATION_FRESHNESS_WINDOW);
        tokio::time::sleep(config.poll_interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::Query;
    use axum::routing::get;
    use axum::{Json, Router};
    use std::collections::HashMap as StdHashMap;

    fn hours(n: i64) -> time::Duration {
        time::Duration::hours(n)
    }

    // -- within_grace_period / registration_eligible --

    #[test]
    fn a_shard_with_unknown_age_is_within_the_grace_period() {
        let now = OffsetDateTime::now_utc();
        assert!(within_grace_period(None, now, hours(24)));
        assert!(registration_eligible(None, now, hours(24), 0, 1));
    }

    #[test]
    fn a_shard_younger_than_the_grace_period_is_eligible_regardless_of_mirror_count() {
        let now = OffsetDateTime::now_utc();
        let first_seen_at = now - hours(1);
        assert!(within_grace_period(Some(first_seen_at), now, hours(24)));
        assert!(registration_eligible(
            Some(first_seen_at),
            now,
            hours(24),
            0,
            1
        ));
    }

    #[test]
    fn a_shard_past_the_grace_period_with_too_few_mirrors_is_not_eligible() {
        let now = OffsetDateTime::now_utc();
        let first_seen_at = now - hours(48);
        assert!(!within_grace_period(Some(first_seen_at), now, hours(24)));
        assert!(!registration_eligible(
            Some(first_seen_at),
            now,
            hours(24),
            0,
            1
        ));
    }

    #[test]
    fn a_shard_past_the_grace_period_with_enough_mirrors_is_eligible() {
        let now = OffsetDateTime::now_utc();
        let first_seen_at = now - hours(48);
        assert!(registration_eligible(
            Some(first_seen_at),
            now,
            hours(24),
            2,
            2
        ));
    }

    #[test]
    fn exactly_at_the_grace_period_boundary_the_gate_applies() {
        let now = OffsetDateTime::now_utc();
        let first_seen_at = now - hours(24);
        // `<` not `<=` — exactly the grace period's own duration has
        // elapsed, so it's past the grace period, not still within it.
        assert!(!within_grace_period(Some(first_seen_at), now, hours(24)));
    }

    // -- MirrorConfirmationRegistry --

    #[test]
    fn confirmed_count_only_counts_fresh_confirmations() {
        let registry = MirrorConfirmationRegistry::new();
        let now = OffsetDateTime::now_utc();
        registry.record_confirmation("core", "http://fresh-peer", now);
        registry.record_confirmation("core", "http://stale-peer", now - time::Duration::hours(1));

        assert_eq!(
            registry.confirmed_count("core", now - time::Duration::minutes(15)),
            1
        );
    }

    #[test]
    fn confirmed_count_deduplicates_the_same_peer() {
        let registry = MirrorConfirmationRegistry::new();
        let now = OffsetDateTime::now_utc();
        registry.record_confirmation("core", "http://peer-a", now);
        registry.record_confirmation("core", "http://peer-a", now + time::Duration::seconds(1));
        assert_eq!(
            registry.confirmed_count("core", now - time::Duration::minutes(1)),
            1
        );
    }

    #[test]
    fn confirmed_count_is_scoped_per_shard() {
        let registry = MirrorConfirmationRegistry::new();
        let now = OffsetDateTime::now_utc();
        registry.record_confirmation("core", "http://peer-a", now);
        registry.record_confirmation("game:other-shard", "http://peer-b", now);
        assert_eq!(
            registry.confirmed_count("core", now - time::Duration::minutes(1)),
            1
        );
        assert_eq!(
            registry.confirmed_count("game:other-shard", now - time::Duration::minutes(1)),
            1
        );
    }

    #[test]
    fn prune_older_than_drops_only_stale_confirmations() {
        let registry = MirrorConfirmationRegistry::new();
        let now = OffsetDateTime::now_utc();
        registry.record_confirmation("core", "http://fresh-peer", now);
        registry.record_confirmation("core", "http://stale-peer", now - time::Duration::hours(1));

        registry.prune_older_than(now - time::Duration::minutes(15));

        assert_eq!(
            registry.confirmed_count("core", now - time::Duration::hours(2)),
            1
        );
    }

    // -- peer_confirms_mirroring --

    async fn spawn_fake_peer(last_seq: i64) -> String {
        async fn handler(
            Query(_params): Query<StdHashMap<String, String>>,
            axum::extract::State(last_seq): axum::extract::State<i64>,
        ) -> Json<serde_json::Value> {
            Json(serde_json::json!({ "last_seq": last_seq }))
        }

        let app = Router::new()
            .route("/ledger/mirror-progress", get(handler))
            .with_state(last_seq);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_peer_reporting_any_mirrored_entries_confirms() {
        let peer = spawn_fake_peer(5).await;
        let client = reqwest::Client::new();
        assert!(peer_confirms_mirroring(&client, &peer, "avalon-test", "core").await);
    }

    #[tokio::test]
    async fn a_peer_reporting_zero_mirrored_entries_does_not_confirm() {
        let peer = spawn_fake_peer(0).await;
        let client = reqwest::Client::new();
        assert!(!peer_confirms_mirroring(&client, &peer, "avalon-test", "core").await);
    }

    #[tokio::test]
    async fn an_unreachable_peer_does_not_confirm_rather_than_erroring() {
        let client = reqwest::Client::new();
        assert!(
            !peer_confirms_mirroring(&client, "http://127.0.0.1:1", "avalon-test", "core").await
        );
    }
}
