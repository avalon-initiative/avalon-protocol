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

use avalon_chain::retention::RetentionConfig;
use avalon_chain::PostgresSettlementProvider;

/// How often a hot-tier node with pruning enabled re-checks for newly
/// prunable entries. Coarse on purpose — this is a maintenance task
/// pruning a slowly-growing window boundary, not a latency-sensitive
/// worker like `outbox::run_worker`'s few-second poll.
const PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Runs forever, pruning on [`PRUNE_INTERVAL`]. Intended to be handed to
/// `tokio::spawn`, mirroring `outbox::run_worker`. Callers must have
/// already checked `config.should_prune()` — see the module doc comment;
/// this function trusts its caller rather than re-checking every tick,
/// keeping the actual prune call a straight pass-through to
/// `avalon-chain`.
pub async fn run_worker(chain: PostgresSettlementProvider, config: RetentionConfig) {
    loop {
        match prune_once(&chain, &config).await {
            Ok(Some(report)) if report.pruned_count > 0 => {
                println!(
                    "retention worker: pruned {} entries' payloads (cutoff: {})",
                    report.pruned_count, report.cutoff
                );
            }
            Ok(_) => {}
            Err(err) => eprintln!("retention worker: {err}"),
        }
        tokio::time::sleep(PRUNE_INTERVAL).await;
    }
}

async fn prune_once(
    chain: &PostgresSettlementProvider,
    config: &RetentionConfig,
) -> Result<Option<avalon_chain::retention::PruneReport>, avalon_chain::SettlementError> {
    let Some(cutoff) = config.prune_cutoff(time::OffsetDateTime::now_utc()) else {
        return Ok(None);
    };
    chain.prune_payloads_older_than(cutoff).await.map(Some)
}
