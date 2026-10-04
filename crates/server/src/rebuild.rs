//! The rebuild-from-events flow.
//!
//! The claim that "Postgres is a projection" is worthless until
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
//! are touched. `sessions`, `credentials` (for now), presence, and
//! any other cache are explicitly out of scope — see
//! `avalon-docs/architecture/disaster-recovery.md`.

use avalon_chain::PostgresSettlementProvider;
use avalon_indexer::identity_proof::{verify_created, EventOrigin};
use avalon_indexer::postgres::PostgresIndexer;
use avalon_protocol::events::ProtocolEvent;
use sqlx::PgPool;

/// `entries_skipped_undecodable` counts a ledger entry whose payload was
/// pruned or whose issuer/subject wasn't a well-formed
/// `GlobalId` — should never happen for a genuine ledger entry, but
/// skipped rather than panicking, same posture
/// `mirror_watcher::protocol_event_from_mirrored` already takes for
/// peer-sourced entries.
#[derive(Debug, Clone, Copy)]
pub struct RebuildReport {
    pub entries_read: usize,
    pub events_applied: usize,
    /// Events the indexer refused, or whose dependencies never arrived.
    pub events_refused: usize,
    pub entries_skipped_undecodable: usize,
}

/// How many `identity.created` events in `events` do not verify as created on
/// (`network_id`, `own_shard_id`). A rebuild that assumes the wrong own shard would refuse them.
pub fn unverifiable_creations(
    events: &[ProtocolEvent],
    network_id: &str,
    own_shard_id: &str,
) -> usize {
    let origin = EventOrigin::local(network_id, own_shard_id);
    events
        .iter()
        .filter(|e| e.kind == "identity.created")
        .filter(|e| verify_created(e, &origin).is_err())
        .count()
}

/// Reads the ledger and counts creations that do not verify for `own_shard_id`; run before
/// [`rebuild_index_from_ledger`] touches anything.
pub async fn count_unverifiable_creations(
    chain: &PostgresSettlementProvider,
    own_shard_id: &str,
) -> Result<usize, avalon_chain::SettlementError> {
    let events: Vec<ProtocolEvent> = chain
        .list_entries()
        .await?
        .iter()
        .filter_map(|entry| entry.to_protocol_event())
        .collect();
    Ok(unverifiable_creations(
        &events,
        chain.network_id(),
        own_shard_id,
    ))
}

/// Truncates every projection table and replays `ledger_entries` back
/// through the indexer, in order. `chain` and `indexer` may point at
/// different pools (the read model is never required to live in the same
/// database as settlement) — both are passed explicitly rather than one
/// being derived from the other.
pub async fn rebuild_index_from_ledger(
    chain: &PostgresSettlementProvider,
    index_pool: &PgPool,
    own_shard_id: &str,
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

    let indexer = PostgresIndexer::new(index_pool.clone())
        .with_local_origin(chain.network_id(), own_shard_id);
    let outcome = indexer
        .rebuild_from_scratch(&events)
        .await
        .map_err(|e| avalon_chain::SettlementError::Storage(e.to_string()))?;

    Ok(RebuildReport {
        entries_read,
        events_applied: outcome.applied,
        events_refused: outcome.refused,
        entries_skipped_undecodable: skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use avalon_protocol::identity_id::TestIdentity;
    use avalon_protocol::ids::GlobalId;
    use time::OffsetDateTime;
    use uuid::Uuid;

    fn created_on(who: &TestIdentity, network: &str, shard: &str) -> ProtocolEvent {
        let gid = GlobalId::new("identity", &who.id.to_string(), "self", "created");
        ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "identity.created".to_string(),
            issuer: gid.clone(),
            subject: gid,
            payload: serde_json::to_value(who.created_payload_for(
                network,
                shard,
                Uuid::new_v4(),
                "Ada",
            ))
            .unwrap(),
            timestamp: OffsetDateTime::now_utc(),
            version: 2,
            identity_chain: None,
        }
    }

    #[test]
    fn creations_for_another_shard_are_counted_before_a_rebuild() {
        let (a, b) = (TestIdentity::new(), TestIdentity::new());
        let events = vec![
            created_on(&a, "net", "core"),
            created_on(&b, "net", "game:slug/1"),
        ];
        assert_eq!(unverifiable_creations(&events, "net", "core"), 1);
        assert_eq!(unverifiable_creations(&events, "net", "game:slug/1"), 1);
        assert_eq!(unverifiable_creations(&events, "other", "core"), 2);
    }
}
