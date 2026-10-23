//! An identity's signing keys judged at a chain position from its accepted chain and inception key.
//! When the chain below the position is not accepted yet the answer is "wait", never a guess.

use std::collections::BTreeMap;

use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::identity_chain::EventHash;
use avalon_protocol::identity_chain_wire::{event_hash, parse_hash};
use avalon_protocol::identity_id::IdentityId;
use avalon_protocol::identity_keys::{key_at, key_records, KeyAt, KeyRecord};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::head_attestor::HeadAttestor;
use crate::{identity_chain_store, IndexError};

/// A chain of one identity (its accepted chain, or the branch below a signed position) with its key
/// records.
pub struct AcceptedChain {
    /// Events in chain order; contiguous from seq 1.
    pub events: Vec<ProtocolEvent>,
    records: Vec<KeyRecord>,
}

fn awaiting(what: &str) -> IndexError {
    IndexError::AwaitingKey(format!("{what} is not accepted for this identity yet"))
}

async fn inception_keys(
    tx: &mut Transaction<'_, Postgres>,
    owner: IdentityId,
) -> Result<Vec<(Uuid, [u8; 32])>, IndexError> {
    let inception: Vec<(Uuid, Vec<u8>)> = sqlx::query_as(
        "SELECT signing_key_id, public_key FROM indexer_identity_signing_keys \
         WHERE identity_id = $1 AND inception",
    )
    .bind(owner)
    .fetch_all(&mut **tx)
    .await?;
    let inception: Vec<(Uuid, [u8; 32])> = inception
        .into_iter()
        .filter_map(|(id, key)| Some((id, <[u8; 32]>::try_from(key).ok()?)))
        .collect();
    if inception.is_empty() {
        return Err(awaiting("the inception key"));
    }
    Ok(inception)
}

/// Loads `owner`'s accepted chain and inception keys. Waits when the identity's creation is unknown.
pub async fn load(
    tx: &mut Transaction<'_, Postgres>,
    heads: Option<&dyn HeadAttestor>,
    owner: IdentityId,
) -> Result<AcceptedChain, IndexError> {
    load_beside(tx, heads, owner, None).await
}

/// [`load`] with `candidate` judged beside the chain: its own revocations apply, it is not in it.
pub async fn load_beside(
    tx: &mut Transaction<'_, Postgres>,
    heads: Option<&dyn HeadAttestor>,
    owner: IdentityId,
    candidate: Option<&ProtocolEvent>,
) -> Result<AcceptedChain, IndexError> {
    let inception = inception_keys(tx, owner).await?;
    let events = identity_chain_store::accepted_events_beside(tx, heads, owner, candidate).await?;
    let records = key_records(&inception, &events);
    Ok(AcceptedChain { events, records })
}

/// The chain a signed position builds on: the ancestors reached from `prev_hash` through recorded
/// events, on whichever branch they sit, so an event is judged by the history its signature covers
/// and not by which branch currently wins. Waits while an ancestor is not recorded yet.
pub async fn load_branch(
    tx: &mut Transaction<'_, Postgres>,
    owner: IdentityId,
    seq: u64,
    prev_hash: Option<&[u8; 32]>,
) -> Result<AcceptedChain, IndexError> {
    let refused = |what: &str| IndexError::Rejected(format!("the signed position {what}"));
    if (seq == 1) != prev_hash.is_none() || seq == 0 {
        return Err(refused("does not extend any chain"));
    }
    let inception = inception_keys(tx, owner).await?;
    let stored = identity_chain_store::stored_events(tx, owner).await?;
    let by_hash: BTreeMap<EventHash, &ProtocolEvent> =
        stored.iter().map(|(e, h)| (*h, e)).collect();
    let mut events = Vec::new();
    let mut wanted = prev_hash.copied();
    let mut expect = seq - 1;
    while let Some(hash) = wanted {
        let event = by_hash
            .get(&hash)
            .ok_or_else(|| awaiting("the chain below the signed position"))?;
        let position = event.identity_chain.as_ref();
        if position.map(|p| p.seq) != Some(expect) {
            return Err(refused("names an event at another position"));
        }
        wanted = position
            .and_then(|p| p.prev_hash.as_deref())
            .and_then(parse_hash);
        events.push((*event).clone());
        expect -= 1;
    }
    if expect != 0 {
        return Err(refused("does not reach the start of the chain"));
    }
    events.reverse();
    let records = key_records(&inception, &events);
    Ok(AcceptedChain { events, records })
}

impl AcceptedChain {
    /// `seq` of the last accepted event; 0 for an empty chain.
    pub fn head_seq(&self) -> u64 {
        self.events
            .last()
            .and_then(|e| e.identity_chain.as_ref())
            .map_or(0, |p| p.seq)
    }

    /// Waits unless the chain is accepted up to `position - 1`.
    pub fn require_prefix(&self, position: u64) -> Result<(), IndexError> {
        if self.head_seq() + 1 < position {
            return Err(awaiting("the chain below the signed position"));
        }
        Ok(())
    }

    /// Checks that a signed position extends the accepted chain: the event hash at `position - 1`
    /// is `prev_hash` (none for the first position). Waits for an incomplete prefix.
    pub fn require_extends(
        &self,
        position: u64,
        prev_hash: Option<&[u8; 32]>,
    ) -> Result<(), IndexError> {
        self.require_prefix(position)?;
        let before = position.checked_sub(1).filter(|s| *s > 0);
        let accepted = match before {
            None => None,
            Some(seq) => self
                .events
                .iter()
                .find(|e| e.identity_chain.as_ref().is_some_and(|p| p.seq == seq))
                .and_then(|e| event_hash(e).ok()),
        };
        if accepted.as_ref() == prev_hash && (before.is_none() == prev_hash.is_none()) {
            Ok(())
        } else {
            Err(IndexError::Rejected(
                "the signed position does not extend the accepted chain".to_string(),
            ))
        }
    }

    /// Whether the accepted chain revokes the key at any position.
    pub fn is_revoked(&self, signing_key_id: Uuid) -> bool {
        self.records
            .iter()
            .any(|r| r.signing_key_id == signing_key_id && r.revoked_seq.is_some())
    }

    /// The key as it stood at `position`. Waits when the chain below it is incomplete; a key the
    /// complete prefix never added is refused.
    pub fn key_at(&self, signing_key_id: Uuid, position: u64) -> Result<KeyAt, IndexError> {
        self.require_prefix(position)?;
        Ok(key_at(&self.records, signing_key_id, position))
    }
}

/// The active key a signer used, or why the event is refused or must wait.
pub fn require_valid(judged: KeyAt, role: &str) -> Result<[u8; 32], IndexError> {
    match judged {
        KeyAt::Valid(key) => Ok(key),
        KeyAt::Revoked => Err(IndexError::Rejected(format!(
            "{role} key was revoked below the signed position"
        ))),
        KeyAt::Unknown => Err(IndexError::Rejected(format!(
            "{role} key was not added below the signed position"
        ))),
    }
}
