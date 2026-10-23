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
    chain_owner, event_hash, resolve_identity_chain, to_chained_event, vetoed_recoveries,
    ChainEventError,
};
use avalon_protocol::identity_id::IdentityId;
use avalon_protocol::identity_revocation::{judge_conflicts, revocation_conflicts, Judged};
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
        /// Events accepted before this record that no longer are.
        displaced: Vec<ProtocolEvent>,
        /// Events other than this one accepted only now (e.g. a gap filled).
        newly_accepted: Vec<ProtocolEvent>,
        /// Every accepted event, in chain order.
        accepted_events: Vec<ProtocolEvent>,
    },
}

#[derive(Clone)]
struct Stored {
    event: ProtocolEvent,
    chained: ChainedEvent,
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

/// Rounds of re-deriving which recoveries the owner vetoed once the revocation rule has dropped
/// events (a dropped cancel no longer vetoes). The result is a function of the stored set alone.
const VERDICT_ROUNDS: usize = 4;

/// What keeps a stored event out of the chain: an owner veto of a recovery, or the revocation rule.
struct Verdict {
    vetoed: BTreeSet<EventHash>,
    judged: Judged,
}

impl Verdict {
    fn keeps(&self, hash: &EventHash) -> bool {
        !self.vetoed.contains(hash) && !self.judged.is_excluded(hash)
    }
}

fn hashed<'a>(events: impl Iterator<Item = &'a Stored>) -> Vec<(ProtocolEvent, EventHash)> {
    events
        .map(|s| (s.event.clone(), s.chained.event_hash))
        .collect()
}

/// Decides which stored events stand: a recovery the owner cancelled at or below its position is
/// invalid, and an event signed by a revoked key stands only if it is older than the revocation by
/// cosigned head time. Anchors come from `heads`; with none, every such event waits.
async fn verdict_of(
    tx: &mut Transaction<'_, Postgres>,
    heads: Option<&dyn HeadAttestor>,
    stored: &[Stored],
) -> Result<Verdict, IndexError> {
    let mut vetoed = BTreeSet::new();
    let mut anchors: BTreeMap<EventHash, OffsetDateTime> = BTreeMap::new();
    let mut asked: BTreeSet<EventHash> = BTreeSet::new();
    let mut judged = Judged::default();
    for _ in 0..VERDICT_ROUNDS {
        let base = hashed(
            stored
                .iter()
                .filter(|s| !vetoed.contains(&s.chained.event_hash)),
        );
        let conflicts = revocation_conflicts(&base);
        if let Some(heads) = heads {
            for hash in conflicts.iter().flat_map(|(r, v)| [*r, *v]) {
                if !asked.insert(hash) {
                    continue;
                }
                let Some(held) = stored.iter().find(|s| s.chained.event_hash == hash) else {
                    continue;
                };
                let attested = chain_event_attestation(tx, heads, &held.event).await?;
                if let Some(at) = attested.and_then(|a| a.anchor_at) {
                    anchors.insert(hash, at);
                }
            }
        }
        judged = judge_conflicts(&conflicts, &anchors);
        let standing = hashed(
            stored
                .iter()
                .filter(|s| !judged.is_excluded(&s.chained.event_hash)),
        );
        let next = vetoed_recoveries(&standing);
        if next == vetoed {
            break;
        }
        vetoed = next;
    }
    Ok(Verdict { vetoed, judged })
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
) -> Result<ChainOutcome, IndexError> {
    let verdict = verdict_of(tx, heads, stored).await?;
    let live: Vec<&Stored> = stored
        .iter()
        .filter(|s| verdict.keeps(&s.chained.event_hash))
        .collect();
    Ok(resolve_live(&live))
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

/// How the revocation rule treats an event about to be recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supersession {
    Stands,
    /// Signed by a key revoked by an event on the ledger no later than it.
    Superseded,
    /// A cosigned head date is still missing for it or for the revocation.
    Pending,
}

/// Judges `event` together with the events already recorded for its owner, by the same rule
/// that resolves the chain.
pub async fn supersession(
    tx: &mut Transaction<'_, Postgres>,
    heads: Option<&dyn HeadAttestor>,
    event: &ProtocolEvent,
) -> Result<Supersession, IndexError> {
    let Some(owner) = chain_owner(event) else {
        return Ok(Supersession::Stands);
    };
    let stored = with_event(load(tx, owner).await?, event)?;
    let hash = event_hash(event).map_err(|e| IndexError::Rejected(format!("{e:?}")))?;
    let verdict = verdict_of(tx, heads, &stored).await?;
    Ok(if verdict.judged.superseded.contains(&hash) {
        Supersession::Superseded
    } else if verdict.judged.pending.contains(&hash)
        || verdict.judged.awaiting_revocation.contains(&hash)
    {
        Supersession::Pending
    } else {
        Supersession::Stands
    })
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
    let before = load(tx, owner).await?;
    let old_outcome = outcome_of(tx, heads, &before).await?;
    let old_hashes: BTreeSet<EventHash> =
        old_outcome.accepted.iter().map(|a| a.event_hash).collect();

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
    let outcome = outcome_of(tx, heads, &after).await?;
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
