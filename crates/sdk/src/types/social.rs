//! Friends, presence, and direct conversations — network-level social
//! concepts, not integrator-scoped.
//!
//! An integrator never automatically receives a user's whole social graph;
//! every read here is gated by a [`crate::types::permissions::Capability`]
//! the user actually granted.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::types::ids::{IdentityId, IntegratorId};

/// `Online` is live/automatic: it reflects heartbeat/TTL state and can't be
/// "stuck" on. `Away`, `DoNotDisturb`, and `Offline` are sticky manual
/// overrides when set explicitly — they persist (ignoring TTL expiry) until
/// the caller explicitly sets `Online` again. Generated from
/// `docs/generated/openapi.json` by `build.rs`.
pub use crate::generated::PresenceStatus;

/// An accepted, mutual friendship between two identities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Friendship {
    /// One side of the friendship — the pair is unordered.
    pub a: IdentityId,
    /// The other side.
    pub b: IdentityId,
    /// When the friendship was accepted.
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
}

/// Where a user currently is, if they've opted to share it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Presence {
    /// Whose presence this is.
    pub identity_id: IdentityId,
    /// Their current status.
    pub status: PresenceStatus,
    /// The integrator the user is currently active in, if any and if shared.
    pub active_in: Option<IntegratorId>,
    /// When the status last changed.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// A direct or small-group conversation — the identity-to-identity sibling
/// of a guild channel: pure structure, no integrator reference anywhere. A
/// conversation between users is a fact about their relationship, not about
/// whichever integrator either of them had open when it started.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    /// This conversation's own id.
    pub id: uuid::Uuid,
    /// Everyone in it.
    pub participants: Vec<IdentityId>,
}

/// A single message within a [`Conversation`]. Deliberately not protocol
/// history: high-volume, non-interoperable, nothing a receiving integrator
/// ever needs to verify.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    /// This message's own id.
    pub id: uuid::Uuid,
    /// The conversation it belongs to.
    pub conversation_id: uuid::Uuid,
    /// Who sent it.
    pub author: IdentityId,
    /// The message text.
    pub body: String,
    /// When it was sent.
    #[serde(with = "time::serde::rfc3339")]
    pub sent_at: OffsetDateTime,
}
