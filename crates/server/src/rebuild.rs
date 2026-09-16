//! The rebuild-from-events flow, closing issue #43.
//!
//! ADR #75's claim ("Postgres is a projection") is worthless until
//! something proves it by actually dropping the read model and
//! regenerating it from `ledger_entries` alone. [`rebuild_index_from_ledger`]
//! is that proof: it reads every entry off the settlement ledger in `seq`
//! order, decodes each back into a [`ProtocolEvent`]
//! (`LedgerEntryView::to_protocol_event`), and replays them through
//! [`avalon_indexer::postgres::PostgresIndexer::rebuild_from_scratch`],
//! which truncates every projection table first.
//!
//! Used by `avalon rebuild-index` (the operator-facing entry point) and by
//! `crates/server/tests/rebuild_from_events.rs` (the disaster-recovery
//! test that drives this directly and diffs a snapshot before/after).
//!
//! Scope: only the promised-durable projections `PROJECTION_TABLES` lists
//! are touched. `sessions`, `credentials` (until #73), presence (#78), and
//! any other cache are explicitly out of scope — see
//! `docs/architecture/disaster-recovery.md`.

use avalon_chain::PostgresSettlementProvider;
use avalon_indexer::postgres::PostgresIndexer;
use sqlx::PgPool;

/// `entries_skipped_undecodable` counts a ledger entry whose payload was
/// pruned (issue #208) or whose issuer/subject wasn't a well-formed
/// `GlobalId` — should never happen for a genuine ledger entry, but
/// skipped rather than panicking, same posture
/// `mirror_watcher::protocol_event_from_mirrored` already takes for
/// peer-sourced entries.
#[derive(Debug, Clone, Copy)]
pub struct RebuildReport {
    pub entries_read: usize,
    pub events_applied: usize,
    pub entries_skipped_undecodable: usize,
}

/// Truncates every projection table and replays `ledger_entries` back
/// through the indexer, in order. `chain` and `indexer` may point at
/// different pools (the read model is never required to live in the same
/// database as settlement) — both are passed explicitly rather than one
/// being derived from the other.
pub async fn rebuild_index_from_ledger(
    chain: &PostgresSettlementProvider,
    index_pool: &PgPool,
) -> Result<RebuildReport, avalon_chain::SettlementError> {
    let entries = chain.list_entries().await?;
    let entries_read = entries.len();

    let mut events = Vec::with_capacity(entries_read);
    let mut skipped = 0usize;
    for entry in &entries {
        match entry.to_protocol_event() {
            Some(event) => events.push(event),
            None => skipped += 1,
        }
    }

    let indexer = PostgresIndexer::new(index_pool.clone());
    let events_applied = indexer
        .rebuild_from_scratch(&events)
        .await
        .map_err(|e| avalon_chain::SettlementError::Storage(e.to_string()))?;

    Ok(RebuildReport {
        entries_read,
        events_applied,
        entries_skipped_undecodable: skipped,
    })
}
