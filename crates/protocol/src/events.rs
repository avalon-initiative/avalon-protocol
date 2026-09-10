//! Protocol events — the subset of what happens in a game that Avalon
//! considers interoperable or durable.
//!
//! Not every game action is a protocol event; ordinary gameplay (combat,
//! movement, XP ticks) never becomes one. See `docs/stakeholders/Proposal.md` §14 and
//! `docs/architecture/protocol-events.md` for the hot-data/durable-fact
//! distinction, and issue #75 for why durable history is canonical.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::ids::GlobalId;

/// A durable, versioned fact Avalon considers part of protocol history.
///
/// Examples: `achievement.issued`, `guild.created`, `guild.member_added`,
/// `game.registered`, `attestation.issued`. `kind` is left as a namespaced
/// string (like `Capability`) rather than a closed enum for the same reason:
/// new event kinds shouldn't require a protocol version bump.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolEvent {
    pub id: Uuid,
    pub kind: String,
    pub issuer: GlobalId,
    pub subject: GlobalId,
    pub payload: serde_json::Value,
    pub timestamp: OffsetDateTime,
    pub version: u32,
}

/// A group of protocol events committed together, so a durable commitment can
/// represent many logical events without one settlement action per event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventBatch {
    pub id: Uuid,
    pub events: Vec<ProtocolEvent>,
    pub created_at: OffsetDateTime,
}

/// A durable commitment to an `EventBatch`, produced by whatever
/// `SettlementProvider` implementation is in use (see the `chain` crate).
/// Deliberately opaque here: this crate does not know or care whether
/// `proof` is a database row's signature or a Merkle root anchored on-chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commitment {
    pub batch_id: Uuid,
    pub proof: Vec<u8>,
    pub committed_at: OffsetDateTime,
}
