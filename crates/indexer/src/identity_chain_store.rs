//! Per-identity chain storage and resolution.
//!
//! Every chained layer-1 event an identity authors, locally or mirrored,
//! is recorded in `identity_chain_events`; `identity_chain_state` holds the
//! resolved head derived by `identity_chain_wire::resolve_identity_chain`
//! over that set. The outcome depends only on the recorded events, never on
//! the order they were recorded in.

use std::collections::BTreeSet;

use avalon_protocol::events::{IdentityChainPosition, ProtocolEvent};
use avalon_protocol::identity_chain::{ChainOutcome, ChainedEvent, ClockSkewBounds, EventHash};
use avalon_protocol::identity_chain_wire::{
    chain_owner, event_hash, resolve_identity_chain, to_chained_event, truncate_to_micros,
    ChainEventError,
};
use sqlx::{PgExecutor, Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

/// Resolved head of one identity's chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainState {
    pub seq: i64,
    pub head_hash: Option<String>,
    pub forked_at_seq: Option<i64>,
}

/// What recording a chained event changed.
#[derive(Debug)]
pub enum Recorded {
    /// Not a chained event, or no chain owner: apply as an ordinary event.
    Unchained,
    /// Chained but its timestamp was outside the node's clock bounds.
    Rejected(ChainEventError),
    Chained {
        /// The event is part of the identity's resolved chain.
        accepted: bool,
        /// Events accepted before this record that no longer are.
        displaced: Vec<ProtocolEvent>,
        /// Events other than this one accepted only now (e.g. a gap filled).
        newly_accepted: Vec<ProtocolEvent>,
        /// Every accepted event, in chain order.
        accepted_events: Vec<ProtocolEvent>,
    },
}

struct Stored {
    event: ProtocolEvent,
    chained: ChainedEvent,
}

pub async fn state<'e>(
    exec: impl PgExecutor<'e>,
    identity_id: Uuid,
) -> Result<Option<ChainState>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT seq, head_hash, forked_at_seq FROM identity_chain_state WHERE identity_id = $1",
    )
    .bind(identity_id)
    .fetch_optional(exec)
    .await?;
    row.map(|r| {
        Ok(ChainState {
            seq: r.try_get("seq")?,
            head_hash: r.try_get("head_hash")?,
            forked_at_seq: r.try_get("forked_at_seq")?,
        })
    })
    .transpose()
}

/// Whether the identity's chain is forked (unresolved conflicting
/// chain-critical events).
pub async fn is_forked<'e>(
    exec: impl PgExecutor<'e>,
    identity_id: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(state(exec, identity_id)
        .await?
        .is_some_and(|s| s.forked_at_seq.is_some()))
}

async fn lock_state(
    tx: &mut Transaction<'_, Postgres>,
    identity_id: Uuid,
) -> Result<ChainState, sqlx::Error> {
    sqlx::query(
        "INSERT INTO identity_chain_state (identity_id) VALUES ($1) ON CONFLICT DO NOTHING",
    )
    .bind(identity_id)
    .execute(&mut **tx)
    .await?;
    let row = sqlx::query(
        "SELECT seq, head_hash, forked_at_seq FROM identity_chain_state \
         WHERE identity_id = $1 FOR UPDATE",
    )
    .bind(identity_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(ChainState {
        seq: row.try_get("seq")?,
        head_hash: row.try_get("head_hash")?,
        forked_at_seq: row.try_get("forked_at_seq")?,
    })
}

async fn load(
    tx: &mut Transaction<'_, Postgres>,
    identity_id: Uuid,
) -> Result<Vec<Stored>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT event, chain_timestamp FROM identity_chain_events \
         WHERE identity_id = $1 ORDER BY seq, event_hash",
    )
    .bind(identity_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let json: serde_json::Value = row.try_get("event")?;
        let ts: OffsetDateTime = row.try_get("chain_timestamp")?;
        let event: ProtocolEvent =
            serde_json::from_value(json).map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
        // The stored timestamp already passed the bounds check at record time.
        let far = ClockSkewBounds {
            max_future_skew: time::Duration::MAX,
            max_past_skew: None,
        };
        let mut chained = to_chained_event(&event, ts, &far)
            .map_err(|e| sqlx::Error::Decode(format!("{e:?}").into()))?;
        chained.timestamp = ts;
        out.push(Stored { event, chained });
    }
    Ok(out)
}

fn outcome_of(stored: &[Stored]) -> ChainOutcome {
    let recovery: BTreeSet<EventHash> = stored
        .iter()
        .filter(|s| s.event.kind == "identity.recovered")
        .map(|s| s.chained.event_hash)
        .collect();
    resolve_identity_chain(
        stored.iter().map(|s| s.chained.clone()).collect(),
        &recovery,
    )
}

fn accepted_of<'a>(stored: &'a [Stored], outcome: &ChainOutcome) -> Vec<&'a Stored> {
    outcome
        .accepted
        .iter()
        .filter_map(|a| stored.iter().find(|s| s.chained.event_hash == a.event_hash))
        .collect()
}

async fn persist_state(
    tx: &mut Transaction<'_, Postgres>,
    identity_id: Uuid,
    outcome: &ChainOutcome,
) -> Result<(), sqlx::Error> {
    let head = outcome.head();
    sqlx::query(
        "UPDATE identity_chain_state SET seq = $2, head_hash = $3, forked_at_seq = $4 \
         WHERE identity_id = $1",
    )
    .bind(identity_id)
    .bind(head.map(|h| h.seq as i64).unwrap_or(0))
    .bind(head.map(|h| hex::encode(h.event_hash)))
    .bind(outcome.forked_at.map(|s| s as i64))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Assigns `event` its position in its identity's chain (the next `seq`
/// after the resolved head, `prev_hash` = head) and records it. Leaves the
/// event unchained when its kind is not chained, or when the identity is
/// forked and the event is not the recovery completion that resolves it.
pub async fn assign_local(
    tx: &mut Transaction<'_, Postgres>,
    event: &mut ProtocolEvent,
) -> Result<(), sqlx::Error> {
    let Some(owner) = chain_owner(event) else {
        return Ok(());
    };
    let head = lock_state(tx, owner).await?;
    if head.forked_at_seq.is_some() && event.kind != "identity.recovered" {
        return Ok(());
    }
    event.timestamp = truncate_to_micros(event.timestamp);
    event.identity_chain = Some(IdentityChainPosition {
        seq: head.seq as u64 + 1,
        prev_hash: head.head_hash,
    });
    record_locked(tx, owner, event, OffsetDateTime::now_utc()).await?;
    Ok(())
}

/// Records a chained event (local or mirrored) and re-resolves its
/// identity's chain.
pub async fn record(
    tx: &mut Transaction<'_, Postgres>,
    event: &ProtocolEvent,
    node_now: OffsetDateTime,
) -> Result<Recorded, sqlx::Error> {
    if event.identity_chain.is_none() {
        return Ok(Recorded::Unchained);
    }
    let Some(owner) = chain_owner(event) else {
        return Ok(Recorded::Unchained);
    };
    lock_state(tx, owner).await?;
    record_locked(tx, owner, event, node_now).await
}

async fn record_locked(
    tx: &mut Transaction<'_, Postgres>,
    owner: Uuid,
    event: &ProtocolEvent,
    node_now: OffsetDateTime,
) -> Result<Recorded, sqlx::Error> {
    let chained = match to_chained_event(event, node_now, &ClockSkewBounds::default()) {
        Ok(c) => c,
        Err(ChainEventError::NotChained) => return Ok(Recorded::Unchained),
        Err(e) => return Ok(Recorded::Rejected(e)),
    };
    let before = load(tx, owner).await?;
    let old_outcome = outcome_of(&before);
    let old_hashes: BTreeSet<EventHash> =
        old_outcome.accepted.iter().map(|a| a.event_hash).collect();

    let hash = event_hash(event).map_err(|e| sqlx::Error::Decode(format!("{e:?}").into()))?;
    let position = event.identity_chain.as_ref().expect("checked by caller");
    sqlx::query(
        "INSERT INTO identity_chain_events \
         (identity_id, event_id, seq, prev_hash, event_hash, chain_timestamp, event) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING",
    )
    .bind(owner)
    .bind(event.id)
    .bind(position.seq as i64)
    .bind(&position.prev_hash)
    .bind(hex::encode(hash))
    .bind(chained.timestamp)
    .bind(serde_json::to_value(event).expect("ProtocolEvent should serialize"))
    .execute(&mut **tx)
    .await?;

    let after = load(tx, owner).await?;
    let outcome = outcome_of(&after);
    persist_state(tx, owner, &outcome).await?;

    let new_hashes: BTreeSet<EventHash> = outcome.accepted.iter().map(|a| a.event_hash).collect();
    let accepted_stored = accepted_of(&after, &outcome);
    let displaced = before
        .iter()
        .filter(|s| {
            old_hashes.contains(&s.chained.event_hash)
                && !new_hashes.contains(&s.chained.event_hash)
        })
        .map(|s| s.event.clone())
        .collect();
    let newly_accepted = accepted_stored
        .iter()
        .filter(|s| s.event.id != event.id && !old_hashes.contains(&s.chained.event_hash))
        .map(|s| s.event.clone())
        .collect();
    Ok(Recorded::Chained {
        accepted: new_hashes.contains(&hash),
        displaced,
        newly_accepted,
        accepted_events: accepted_stored.iter().map(|s| s.event.clone()).collect(),
    })
}
