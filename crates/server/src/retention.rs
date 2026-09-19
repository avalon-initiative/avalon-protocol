//! Background driver for node-tiered durable history retention — issue
//! #208. `avalon_chain::retention` owns the config shape, the cutoff math,
//! and the actual pruning `UPDATE`; this module is only the periodic
//! "call it on a timer, log what happened" wiring, the same shape
//! `crate::outbox::run_worker` already uses for the settlement worker.
//!
//! **Only spawned when there's something to do.** `main.rs` checks
//! [`avalon_chain::retention::RetentionConfig::should_prune`] before ever
//! calling [`run_worker`] — a full-tier node, or a hot-tier node with
//! pruning left disabled, never runs this loop at all, rather than running
//! it and having it no-op every tick forever.
//!
//! **Archive-confirmation gating (issue #569).** `avalon_chain::retention::RetentionConfig`'s
//! `archive_peers`/`min_archive_confirmations` are pure config — the
//! *chain* crate deliberately has no network I/O of its own (see its own
//! module doc comment). This module is where that config actually becomes
//! an enforced check: before a pruning pass runs, [`confirm_archive_coverage`]
//! asks each configured peer's own `GET /ledger/mirror-progress` (see
//! `crate::settlement::mirror_progress`) how far it has independently
//! verified and mirrored this node's history, and only proceeds once
//! enough of them report having covered the pass's boundary `seq`. This is
//! what turns "never prune what nothing else retains" from operator
//! discipline into something the code itself checks — see
//! `docs/architecture/nodes.md`'s "Settlement retention tiers" section for
//! the full design this closes.
//!
//! `avalon prune-ledger`'s manual entry point (`crates/cli/src/main.rs`)
//! deliberately does **not** perform this check itself — it has no
//! `reqwest` dependency in a `--no-default-features` build (see that
//! crate's own feature-split doc comment) — and instead refuses to run a
//! real (non-`--dry-run`) prune at all when the loaded config requires
//! archive confirmation, pointing the operator at this background worker
//! instead. That worker is the one place this check actually runs.

use avalon_chain::retention::RetentionConfig;
use avalon_chain::PostgresSettlementProvider;
use serde::Deserialize;

/// How often a hot-tier node with pruning enabled re-checks for newly
/// prunable entries. Coarse on purpose — this is a maintenance task
/// pruning a slowly-growing window boundary, not a latency-sensitive
/// worker like `outbox::run_worker`'s few-second poll.
const PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

#[derive(Deserialize)]
struct MirrorProgressBody {
    last_seq: i64,
}

/// Issue #569: queries `peer`'s own `GET /ledger/mirror-progress` and
/// reports whether it has verified/mirrored at least through `boundary_seq`
/// for `network_id`. Any failure (unreachable, non-2xx, unparseable body)
/// counts as "not confirmed" rather than erroring the whole pruning pass —
/// one flaky/misconfigured archive peer should never itself become a
/// reason every *other* configured peer's confirmation is ignored.
///
/// Issue #573: `own_base_url`, when this node knows its own
/// (`AVALON_NODE_URL`), is sent as `source_url` — the archive peer might
/// mirror more than one node under this same `network_id` (#527's sharded
/// model), so without it, "has this peer mirrored far enough" could
/// answer using a completely different mirrored source's progress.
/// `None` keeps the exact pre-#573 unscoped query.
///
/// Issue #604 note: this call doesn't send `shard_id` at all yet, so
/// `GET /ledger/mirror-progress` answers for its default (`"core"`) shard
/// regardless of which shard this node actually authors — correct for
/// every deployment today (a node's own authored shard is always `"core"`
/// unless `AVALON_OWN_SHARD_ID` is set), but a real, separate gap once a
/// non-`"core"`-authoring node needs archive confirmation for *its own*
/// shard specifically. Not fixed here — this function has no access to
/// this node's own `own_shard_id` today, and threading it through is a
/// wider change than this ticket's actual scope (mirror storage/
/// verification collision between shards).
async fn peer_confirms_coverage(
    client: &reqwest::Client,
    peer: &str,
    network_id: &str,
    boundary_seq: i64,
    own_base_url: Option<&str>,
) -> bool {
    let url = format!("{peer}/ledger/mirror-progress");
    let mut params = vec![("network_id", network_id)];
    if let Some(own_base_url) = own_base_url {
        params.push(("source_url", own_base_url));
    }
    match client.get(&url).query(&params).send().await {
        Ok(response) if response.status().is_success() => {
            match response.json::<MirrorProgressBody>().await {
                Ok(body) => body.last_seq >= boundary_seq,
                Err(_) => false,
            }
        }
        _ => false,
    }
}

/// Issue #569: `true` once at least `config.min_archive_confirmations`
/// *distinct* peers in `config.archive_peers` have confirmed coverage
/// through `boundary_seq` — short-circuits as soon as that count is
/// reached rather than always querying every configured peer.
pub async fn confirm_archive_coverage(
    client: &reqwest::Client,
    config: &RetentionConfig,
    network_id: &str,
    boundary_seq: i64,
    own_base_url: Option<&str>,
) -> bool {
    if !config.requires_archive_confirmation() {
        return true;
    }
    let mut confirmations = 0usize;
    for peer in &config.archive_peers {
        if peer_confirms_coverage(client, peer, network_id, boundary_seq, own_base_url).await {
            confirmations += 1;
            if confirmations >= config.min_archive_confirmations {
                return true;
            }
        }
    }
    false
}

/// Runs forever, pruning on [`PRUNE_INTERVAL`]. Intended to be handed to
/// `tokio::spawn`, mirroring `outbox::run_worker`. Callers must have
/// already checked `config.should_prune()` — see the module doc comment;
/// this function trusts its caller rather than re-checking every tick,
/// keeping the actual prune call a straight pass-through to
/// `avalon-chain`.
pub async fn run_worker(chain: PostgresSettlementProvider, config: RetentionConfig) {
    let client = reqwest::Client::new();
    // Issue #573: read once, not per tick — `AVALON_NODE_URL` doesn't
    // change while this process runs, and this is the same env var
    // `crate::nodes::AnnounceConfig` already reads for the same purpose
    // (telling a peer "this is who I am").
    let own_base_url = std::env::var("AVALON_NODE_URL").ok();
    loop {
        match prune_once(&chain, &config, &client, own_base_url.as_deref()).await {
            Ok(Some(report)) if report.pruned_count > 0 => {
                tracing::info!(
                    pruned_count = report.pruned_count,
                    cutoff = %report.cutoff,
                    "retention worker: pruned entries' payloads"
                );
            }
            Ok(Some(_)) => {}
            Ok(None) => {}
            Err(err) => tracing::error!("retention worker: {err}"),
        }
        tokio::time::sleep(PRUNE_INTERVAL).await;
    }
}

async fn prune_once(
    chain: &PostgresSettlementProvider,
    config: &RetentionConfig,
    client: &reqwest::Client,
    own_base_url: Option<&str>,
) -> Result<Option<avalon_chain::retention::PruneReport>, avalon_chain::SettlementError> {
    let Some(cutoff) = config.prune_cutoff(time::OffsetDateTime::now_utc()) else {
        return Ok(None);
    };

    if config.requires_archive_confirmation() {
        let Some(boundary_seq) = chain.max_seq_before(cutoff).await? else {
            // Nothing prunable at this cutoff at all — nothing to confirm
            // coverage for, and nothing for prune_payloads_older_than to
            // do either.
            return Ok(None);
        };
        if !confirm_archive_coverage(
            client,
            config,
            chain.network_id(),
            boundary_seq,
            own_base_url,
        )
        .await
        {
            tracing::warn!(
                boundary_seq,
                min_confirmations = config.min_archive_confirmations,
                peers = config.archive_peers.len(),
                "retention worker: skipping this prune pass — fewer than the required number \
                 of archive peers have confirmed coverage up to the boundary seq yet"
            );
            return Ok(None);
        }
    }

    chain.prune_payloads_older_than(cutoff).await.map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::Query;
    use axum::routing::get;
    use axum::{Json, Router};
    use std::collections::HashMap;

    /// A minimal fake peer — just enough of `GET /ledger/mirror-progress`
    /// to drive [`confirm_archive_coverage`]/[`peer_confirms_coverage`]
    /// without a real Postgres-backed `avalon-server` behind it. `last_seq`
    /// is fixed per fake peer; a real server's own handler
    /// (`crate::settlement::mirror_progress`) is exercised separately by
    /// this crate's live `--ignored` tests.
    async fn spawn_fake_peer(last_seq: i64) -> String {
        async fn handler(
            Query(params): Query<HashMap<String, String>>,
            axum::extract::State(last_seq): axum::extract::State<i64>,
        ) -> Json<serde_json::Value> {
            Json(serde_json::json!({
                "network_id": params.get("network_id"),
                "last_seq": last_seq,
            }))
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

    fn config_with_peers(peers: Vec<String>, min_confirmations: usize) -> RetentionConfig {
        RetentionConfig {
            tier: avalon_chain::retention::RetentionTier::Hot { window_days: 30 },
            pruning_enabled: true,
            archive_peers: peers,
            min_archive_confirmations: min_confirmations,
        }
    }

    #[tokio::test]
    async fn a_peer_reporting_coverage_at_or_past_the_boundary_confirms() {
        let peer = spawn_fake_peer(100).await;
        let client = reqwest::Client::new();
        assert!(peer_confirms_coverage(&client, &peer, "avalon-test", 100, None).await);
        assert!(peer_confirms_coverage(&client, &peer, "avalon-test", 50, None).await);
    }

    #[tokio::test]
    async fn a_peer_reporting_coverage_short_of_the_boundary_does_not_confirm() {
        let peer = spawn_fake_peer(50).await;
        let client = reqwest::Client::new();
        assert!(!peer_confirms_coverage(&client, &peer, "avalon-test", 100, None).await);
    }

    #[tokio::test]
    async fn an_unreachable_peer_does_not_confirm_rather_than_erroring() {
        let client = reqwest::Client::new();
        assert!(
            !peer_confirms_coverage(&client, "http://127.0.0.1:1", "avalon-test", 1, None).await
        );
    }

    #[tokio::test]
    async fn no_archive_peers_configured_means_coverage_is_always_confirmed() {
        let config = config_with_peers(vec![], 0);
        let client = reqwest::Client::new();
        assert!(confirm_archive_coverage(&client, &config, "avalon-test", 999, None).await);
    }

    #[tokio::test]
    async fn requires_the_configured_number_of_distinct_confirmations() {
        let covering = spawn_fake_peer(100).await;
        let short = spawn_fake_peer(10).await;
        let client = reqwest::Client::new();

        let config = config_with_peers(vec![covering.clone(), short.clone()], 1);
        assert!(
            confirm_archive_coverage(&client, &config, "avalon-test", 100, None).await,
            "one covering peer should be enough when min_confirmations is 1"
        );

        let config = config_with_peers(vec![short, covering], 2);
        assert!(
            !confirm_archive_coverage(&client, &config, "avalon-test", 100, None).await,
            "only one of two peers actually covers the boundary — 2 confirmations isn't met"
        );
    }
}
