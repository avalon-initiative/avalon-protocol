//! Per-identity chain storage and resolution.
//!
//! Every chained layer-1 event an identity authors, locally or mirrored,
//! is recorded in `identity_chain_events`; `identity_chain_state` holds the
//! resolved head derived by `identity_chain_wire::resolve_identity_chain`
//! over that set. The outcome depends only on the recorded events and the cosigned heads
//! that date them, never on the order the events were recorded in.

use std::collections::{BTreeMap, BTreeSet};

use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::identity_chain::{ChainOutcome, ChainedEvent, ClockSkewBounds, EventHash};
use avalon_protocol::identity_chain_wire::{
    chain_owner, event_hash, resolve_identity_chain, to_chained_event, ChainEventError,
};
use avalon_protocol::identity_id::IdentityId;
use avalon_protocol::identity_revocation::{revocation_conflicts, settle, Settled};
use sqlx::{PgExecutor, Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::head_attestor::{chain_event_attestation, HeadAttestor};
use crate::IndexError;

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
    /// The chain already holds this hash under another event id: the same signed event,
    /// republished with different unsigned fields.
    Duplicate,
    Chained {
        /// The event is part of the identity's resolved chain.
        accepted: bool,
    },
}

/// What differs between an identity's resolved chain and what is projected.
#[derive(Debug, Default)]
pub struct Reconciliation {
    /// Every event of the resolved chain, in chain order.
    pub accepted: Vec<ProtocolEvent>,
    /// Projected events the chain no longer holds, in chain order.
    pub stale: Vec<ProtocolEvent>,
    /// Events of the chain not projected yet, in chain order.
    pub fresh: Vec<ProtocolEvent>,
}

#[derive(Clone)]
struct Stored {
    event: ProtocolEvent,
    chained: ChainedEvent,
    projected: bool,
}

pub async fn state<'e>(
    exec: impl PgExecutor<'e>,
    identity_id: IdentityId,
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
    identity_id: IdentityId,
) -> Result<bool, sqlx::Error> {
    Ok(state(exec, identity_id)
        .await?
        .is_some_and(|s| s.forked_at_seq.is_some()))
}

/// Locks the identity's chain head for the transaction, so the position a signer covered cannot
/// move before the event is assigned. Creates the (empty) head row for a new identity.
pub async fn lock_head(
    tx: &mut Transaction<'_, Postgres>,
    identity_id: IdentityId,
) -> Result<ChainState, sqlx::Error> {
    lock_state(tx, identity_id).await
}

async fn lock_state(
    tx: &mut Transaction<'_, Postgres>,
    identity_id: IdentityId,
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
    identity_id: IdentityId,
) -> Result<Vec<Stored>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT event, chain_timestamp, projected FROM identity_chain_events \
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
        out.push(Stored {
            event,
            chained,
            projected: row.try_get("projected")?,
        });
    }
    Ok(out)
}

/// What keeps a stored event out of the chain, and whether that rests on cosigned head dates.
struct Verdict {
    settled: Settled,
    /// Some revocation conflict exists, so a head date arriving or lowering can change the chain.
    anchor_dependent: bool,
}

impl Verdict {
    fn keeps(&self, hash: &EventHash) -> bool {
        self.settled.keeps(hash)
    }

    fn excluded_count(&self) -> usize {
        self.settled.excluded().len()
    }
}

fn hashed<'a>(events: impl Iterator<Item = &'a Stored>) -> Vec<(ProtocolEvent, EventHash)> {
    events
        .map(|s| (s.event.clone(), s.chained.event_hash))
        .collect()
}

/// Decides which stored events stand: a recovery the owner cancelled at or below its position is
/// invalid, and an event signed by a revoked key stands only if it is older than the revocation by
/// cosigned head time. Anchors of every event in a possible conflict are read once from `heads`; with
/// none, such events are undated.
async fn verdict_of(
    tx: &mut Transaction<'_, Postgres>,
    heads: Option<&dyn HeadAttestor>,
    stored: &[Stored],
) -> Result<Verdict, IndexError> {
    let events = hashed(stored.iter());
    let conflicts = revocation_conflicts(&events);
    let mut anchors: BTreeMap<EventHash, OffsetDateTime> = BTreeMap::new();
    if let Some(heads) = heads {
        let involved: BTreeSet<EventHash> = conflicts.iter().flat_map(|(r, v)| [*r, *v]).collect();
        for held in stored
            .iter()
            .filter(|s| involved.contains(&s.chained.event_hash))
        {
            let attested = chain_event_attestation(tx, heads, &held.event).await?;
            if let Some(at) = attested.and_then(|a| a.anchor_at) {
                anchors.insert(held.chained.event_hash, at);
            }
        }
    }
    Ok(Verdict {
        settled: settle(&events, &anchors),
        anchor_dependent: !conflicts.is_empty(),
    })
}

fn resolve_live(live: &[&Stored]) -> ChainOutcome {
    let recovery: BTreeSet<EventHash> = live
        .iter()
        .filter(|s| s.event.kind == "identity.recovered")
        .map(|s| s.chained.event_hash)
        .collect();
    resolve_identity_chain(live.iter().map(|s| s.chained.clone()).collect(), &recovery)
}

async fn outcome_of(
    tx: &mut Transaction<'_, Postgres>,
    heads: Option<&dyn HeadAttestor>,
    stored: &[Stored],
) -> Result<(ChainOutcome, Verdict), IndexError> {
    let verdict = verdict_of(tx, heads, stored).await?;
    let live: Vec<&Stored> = stored
        .iter()
        .filter(|s| verdict.keeps(&s.chained.event_hash))
        .collect();
    Ok((resolve_live(&live), verdict))
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
    identity_id: IdentityId,
    outcome: &ChainOutcome,
    anchor_dependent: bool,
) -> Result<(), sqlx::Error> {
    let head = outcome.head();
    sqlx::query(
        "UPDATE identity_chain_state SET seq = $2, head_hash = $3, forked_at_seq = $4, \
         anchor_dependent = $5 WHERE identity_id = $1",
    )
    .bind(identity_id)
    .bind(head.map(|h| h.seq as i64).unwrap_or(0))
    .bind(head.map(|h| hex::encode(h.event_hash)))
    .bind(outcome.forked_at.map(|s| s as i64))
    .bind(anchor_dependent)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The events of the identity's resolved chain, in chain order.
pub async fn accepted_events(
    tx: &mut Transaction<'_, Postgres>,
    heads: Option<&dyn HeadAttestor>,
    identity_id: IdentityId,
) -> Result<Vec<ProtocolEvent>, IndexError> {
    accepted_events_beside(tx, heads, identity_id, None).await
}

/// The resolved chain as it stands when `candidate` is judged beside it: the candidate's own
/// revocations apply (events it supersedes are out) but it is not part of the chain it must extend.
pub async fn accepted_events_beside(
    tx: &mut Transaction<'_, Postgres>,
    heads: Option<&dyn HeadAttestor>,
    identity_id: IdentityId,
    candidate: Option<&ProtocolEvent>,
) -> Result<Vec<ProtocolEvent>, IndexError> {
    let stored = load(tx, identity_id).await?;
    let with_candidate = match candidate {
        Some(event) => Some(with_event(stored.clone(), event)?),
        None => None,
    };
    let verdict = verdict_of(tx, heads, with_candidate.as_deref().unwrap_or(&stored)).await?;
    let candidate_hash = candidate.and_then(|e| event_hash(e).ok());
    let live: Vec<Stored> = stored
        .into_iter()
        .filter(|s| {
            verdict.keeps(&s.chained.event_hash) && Some(s.chained.event_hash) != candidate_hash
        })
        .collect();
    let outcome = resolve_live(&live.iter().collect::<Vec<_>>());
    Ok(accepted_of(&live, &outcome)
        .into_iter()
        .map(|s| s.event.clone())
        .collect())
}

/// `stored` plus `event`, unless it is recorded already.
fn with_event(mut stored: Vec<Stored>, event: &ProtocolEvent) -> Result<Vec<Stored>, IndexError> {
    let hash = event_hash(event).map_err(|e| IndexError::Rejected(format!("{e:?}")))?;
    if !stored.iter().any(|s| s.chained.event_hash == hash) {
        let far = ClockSkewBounds {
            max_future_skew: time::Duration::MAX,
            max_past_skew: None,
        };
        let chained = to_chained_event(event, OffsetDateTime::now_utc(), &far)
            .map_err(|e| IndexError::Rejected(format!("{e:?}")))?;
        stored.push(Stored {
            event: event.clone(),
            chained,
            projected: false,
        });
    }
    Ok(stored)
}

/// Every event recorded for the identity with its chain hash, on any branch.
pub async fn stored_events(
    tx: &mut Transaction<'_, Postgres>,
    identity_id: IdentityId,
) -> Result<Vec<(ProtocolEvent, EventHash)>, sqlx::Error> {
    Ok(hashed(load(tx, identity_id).await?.iter()))
}

/// The most events of one identity that the chain keeps out (vetoed, superseded or undated) before more
/// are refused, so a flood of events signed by a revoked key stays bounded.
pub const MAX_EXCLUDED_PER_IDENTITY: usize = 64;

/// Refuses `event` when the identity already holds [`MAX_EXCLUDED_PER_IDENTITY`] events the chain
/// keeps out and this one would be kept out too. Anything else is recorded: whether an event
/// stands is decided by the chain, from the events and cosigned head dates held, not by arrival.
pub async fn admit(
    tx: &mut Transaction<'_, Postgres>,
    heads: Option<&dyn HeadAttestor>,
    event: &ProtocolEvent,
) -> Result<(), IndexError> {
    let Some(owner) = chain_owner(event) else {
        return Ok(());
    };
    let stored = load(tx, owner).await?;
    let hash = event_hash(event).map_err(|e| IndexError::Rejected(format!("{e:?}")))?;
    if stored.iter().any(|s| s.chained.event_hash == hash) {
        return Ok(());
    }
    let with = with_event(stored, event)?;
    let verdict = verdict_of(tx, heads, &with).await?;
    if !verdict.keeps(&hash) && verdict.excluded_count() > MAX_EXCLUDED_PER_IDENTITY {
        return Err(IndexError::Rejected(
            "too many events of this identity are kept out of its chain".to_string(),
        ));
    }
    Ok(())
}

/// Records a locally authored event at the position its author signed. The caller has locked the
/// identity's head ([`lock_head`]) and checked that the position extends it.
pub async fn record_local(
    tx: &mut Transaction<'_, Postgres>,
    heads: Option<&dyn HeadAttestor>,
    event: &ProtocolEvent,
) -> Result<Recorded, IndexError> {
    let Some(owner) = chain_owner(event) else {
        return Ok(Recorded::Unchained);
    };
    record_locked(tx, heads, owner, event, OffsetDateTime::now_utc()).await
}

/// Records a chained event (local or mirrored) and re-resolves its
/// identity's chain.
pub async fn record(
    tx: &mut Transaction<'_, Postgres>,
    heads: Option<&dyn HeadAttestor>,
    event: &ProtocolEvent,
    node_now: OffsetDateTime,
) -> Result<Recorded, IndexError> {
    if event.identity_chain.is_none() {
        return Ok(Recorded::Unchained);
    }
    let Some(owner) = chain_owner(event) else {
        return Ok(Recorded::Unchained);
    };
    lock_state(tx, owner).await?;
    record_locked(tx, heads, owner, event, node_now).await
}

async fn record_locked(
    tx: &mut Transaction<'_, Postgres>,
    heads: Option<&dyn HeadAttestor>,
    owner: IdentityId,
    event: &ProtocolEvent,
    node_now: OffsetDateTime,
) -> Result<Recorded, IndexError> {
    let chained = match to_chained_event(event, node_now, &ClockSkewBounds::default()) {
        Ok(c) => c,
        Err(ChainEventError::NotChained) => return Ok(Recorded::Unchained),
        Err(e) => return Ok(Recorded::Rejected(e)),
    };
    let hash = event_hash(event).map_err(|e| sqlx::Error::Decode(format!("{e:?}").into()))?;
    let held: Option<Uuid> = sqlx::query_scalar(
        "SELECT event_id FROM identity_chain_events WHERE identity_id = $1 AND event_hash = $2",
    )
    .bind(owner)
    .bind(hex::encode(hash))
    .fetch_optional(&mut **tx)
    .await?;
    if held.is_some_and(|id| id != event.id) {
        return Ok(Recorded::Duplicate);
    }
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
    let (outcome, verdict) = outcome_of(tx, heads, &after).await?;
    persist_state(tx, owner, &outcome, verdict.anchor_dependent).await?;
    Ok(Recorded::Chained {
        accepted: outcome.accepted.iter().any(|a| a.event_hash == hash),
    })
}

/// Re-resolves the identity's chain with the dates now known, stores its head, and reports which
/// projected events the chain no longer holds and which of its events are not projected yet.
pub async fn reconcile_view(
    tx: &mut Transaction<'_, Postgres>,
    heads: Option<&dyn HeadAttestor>,
    owner: IdentityId,
) -> Result<Reconciliation, IndexError> {
    lock_state(tx, owner).await?;
    let stored = load(tx, owner).await?;
    let (outcome, verdict) = outcome_of(tx, heads, &stored).await?;
    persist_state(tx, owner, &outcome, verdict.anchor_dependent).await?;
    let accepted = accepted_of(&stored, &outcome);
    let held: BTreeSet<EventHash> = accepted.iter().map(|s| s.chained.event_hash).collect();
    Ok(Reconciliation {
        accepted: accepted.iter().map(|s| s.event.clone()).collect(),
        stale: stored
            .iter()
            .filter(|s| s.projected && !held.contains(&s.chained.event_hash))
            .map(|s| s.event.clone())
            .collect(),
        fresh: accepted
            .iter()
            .filter(|s| !s.projected)
            .map(|s| s.event.clone())
            .collect(),
    })
}

/// Records whether the chain event with this id is projected.
pub async fn mark_projected(
    tx: &mut Transaction<'_, Postgres>,
    owner: IdentityId,
    events: &[ProtocolEvent],
    projected: bool,
) -> Result<(), sqlx::Error> {
    for event in events {
        let hash = event_hash(event).map_err(|e| sqlx::Error::Decode(format!("{e:?}").into()))?;
        sqlx::query(
            "UPDATE identity_chain_events SET projected = $3 \
             WHERE identity_id = $1 AND event_hash = $2",
        )
        .bind(owner)
        .bind(hex::encode(hash))
        .bind(projected)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Identities whose chain depends on cosigned head dates, up to `limit`.
pub async fn anchor_dependent_identities(
    tx: &mut Transaction<'_, Postgres>,
    limit: i64,
) -> Result<Vec<IdentityId>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT identity_id FROM identity_chain_state WHERE anchor_dependent \
         ORDER BY identity_id LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&mut **tx)
    .await
}
