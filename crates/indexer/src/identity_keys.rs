//! An identity's signing keys judged at a chain position from its accepted chain and inception key.
//! When the chain below the position is not accepted yet the answer is "wait", never a guess.

use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::identity_chain_wire::event_hash;
use avalon_protocol::identity_id::IdentityId;
use avalon_protocol::identity_keys::{key_at, key_records, KeyAt, KeyRecord};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::{identity_chain_store, IndexError};

/// The accepted chain of one identity with its key records.
pub struct AcceptedChain {
    /// Accepted events in chain order; contiguous from seq 1.
    pub events: Vec<ProtocolEvent>,
    records: Vec<KeyRecord>,
}

fn awaiting(what: &str) -> IndexError {
    IndexError::AwaitingKey(format!("{what} is not accepted for this identity yet"))
}

/// Loads `owner`'s accepted chain and inception keys. Waits when the identity's creation is unknown.
pub async fn load(
    tx: &mut Transaction<'_, Postgres>,
    owner: IdentityId,
) -> Result<AcceptedChain, IndexError> {
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
    let events = identity_chain_store::accepted_events(tx, owner).await?;
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
