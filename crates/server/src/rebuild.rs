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
use avalon_indexer::identity_proof::verify_created;
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

/// How many `identity.created` events in `events` do not verify as created on `network_id`.
pub fn unverifiable_creations(events: &[ProtocolEvent], network_id: &str) -> usize {
    events
        .iter()
        .filter(|e| e.kind == "identity.created")
        .filter(|e| verify_created(e, network_id).is_err())
        .count()
}

/// A ledger read once: its decodable events in `seq` order and how many entries were skipped.
pub struct LedgerEvents {
    pub entries_read: usize,
    pub entries_skipped_undecodable: usize,
    pub events: Vec<ProtocolEvent>,
}

pub async fn load_ledger_events(
    chain: &PostgresSettlementProvider,
) -> Result<LedgerEvents, avalon_chain::SettlementError> {
    let entries = chain.list_entries().await?;
    let mut events = Vec::with_capacity(entries.len());
    let mut skipped = 0usize;
    for entry in &entries {
        match entry.to_protocol_event() {
            Some(event) => events.push(event),
            None => skipped += 1,
        }
    }
    Ok(LedgerEvents {
        entries_read: entries.len(),
        entries_skipped_undecodable: skipped,
        events,
    })
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
    let ledger = load_ledger_events(chain).await?;
    rebuild_index_from_events(chain.network_id(), index_pool, own_shard_id, ledger).await
}

/// [`rebuild_index_from_ledger`] over a ledger already read.
pub async fn rebuild_index_from_events(
    network_id: &str,
    index_pool: &PgPool,
    own_shard_id: &str,
    ledger: LedgerEvents,
) -> Result<RebuildReport, avalon_chain::SettlementError> {
    let indexer =
        PostgresIndexer::new(index_pool.clone()).with_local_origin(network_id, own_shard_id);
    let outcome = indexer
        .rebuild_from_scratch(&ledger.events)
        .await
        .map_err(|e| avalon_chain::SettlementError::Storage(e.to_string()))?;

    Ok(RebuildReport {
        entries_read: ledger.entries_read,
        events_applied: outcome.applied,
        events_refused: outcome.refused,
        entries_skipped_undecodable: ledger.entries_skipped_undecodable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use avalon_protocol::identity_id::TestIdentity;
    use avalon_protocol::ids::GlobalId;
    use time::OffsetDateTime;
    use uuid::Uuid;

    fn created_on(who: &TestIdentity, network: &str) -> ProtocolEvent {
        let gid = GlobalId::new("identity", &who.id.to_string(), "self", "created");
        ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "identity.created".to_string(),
            issuer: gid.clone(),
            subject: gid,
            payload: serde_json::to_value(who.created_payload_for(network, Uuid::new_v4(), "Ada"))
                .unwrap(),
            timestamp: OffsetDateTime::now_utc(),
            version: 2,
            identity_chain: None,
        }
    }

    #[test]
    fn creations_for_another_network_are_counted_before_a_rebuild() {
        let (a, b) = (TestIdentity::new(), TestIdentity::new());
        let events = vec![created_on(&a, "net"), created_on(&b, "other")];
        assert_eq!(unverifiable_creations(&events, "net"), 1);
        assert_eq!(unverifiable_creations(&events, "other"), 1);
        assert_eq!(unverifiable_creations(&events, "third"), 2);
    }
}
