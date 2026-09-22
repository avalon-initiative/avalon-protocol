//! Durable protocol-history shapes, as a managed-hosting client sees them.
//!
//! Only the batch/commitment side lives here — the part an integrator
//! submitting to a managed host (`crate::managed_hosting`) actually has to
//! construct and read back. The server's own event-kind catalogue and
//! payload types stay server-side; this SDK deliberately treats a payload
//! as opaque JSON.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::types::ids::GlobalId;

/// A durable, versioned fact Avalon considers part of protocol history —
/// e.g. `achievement.issued`, `guild.created`, `game.registered`. `kind` is
/// a plain string on the wire, and that string is what gets hashed into the
/// ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolEvent {
    /// This event's own id.
    pub id: Uuid,
    /// The event-kind string, e.g. `achievement.issued`.
    pub kind: String,
    /// Who authored the fact.
    pub issuer: GlobalId,
    /// What the fact is about.
    pub subject: GlobalId,
    /// Kind-specific body, opaque to this SDK.
    pub payload: serde_json::Value,
    /// When the fact happened.
    pub timestamp: OffsetDateTime,
    /// The payload schema version for this `kind`.
    pub version: u32,
}

/// A group of protocol events committed together, so a durable commitment
/// can represent many logical events without one settlement action per
/// event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventBatch {
    /// This batch's own id.
    pub id: Uuid,
    /// The events committed together, in order.
    pub events: Vec<ProtocolEvent>,
    /// When the batch was assembled.
    pub created_at: OffsetDateTime,
}

/// A durable commitment to an [`EventBatch`]. Deliberately opaque: nothing
/// client-side knows or cares whether `proof` is a database row's signature
/// or a Merkle root anchored elsewhere.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commitment {
    /// The batch this commitment covers.
    pub batch_id: Uuid,
    /// Settlement-provider-specific evidence; opaque here.
    pub proof: Vec<u8>,
    /// When the commitment was made.
    pub committed_at: OffsetDateTime,
}
