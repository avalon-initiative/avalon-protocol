//! Friends and presence — network-level social concepts, not integrator-scoped.
//!
//! An integrator never automatically receives a user's whole social graph; see
//! `permissions` for the capability that gates each of these reads.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::{IdentityId, IntegratorId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Friendship {
    pub a: IdentityId,
    pub b: IdentityId,
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
}

/// A friend request awaiting a response. Distinct from [`Friendship`] — a
/// request never becomes durable history on its own; only the resulting
/// `friend.accepted` (or nothing, if declined/withdrawn) does. See
/// `docs/projects/backend-server/architecture/social-graph.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendRequest {
    pub from: IdentityId,
    pub to: IdentityId,
    #[serde(with = "time::serde::rfc3339")]
    pub requested_at: OffsetDateTime,
}

/// `Online` is live/automatic: it reflects heartbeat/TTL state and can't be
/// "stuck" on. `Away`, `DoNotDisturb`, and `Offline` are sticky manual
/// overrides when set explicitly via `PUT /me/presence` — they persist
/// (ignoring TTL expiry) until the caller explicitly sets `Online` again.
/// See `crates/server/src/presence.rs::PresenceStore::get`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub enum PresenceStatus {
    Online,
    Away,
    DoNotDisturb,
    Offline,
}

/// Where a user currently is, if they've opted to share it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Presence {
    pub identity_id: IdentityId,
    pub status: PresenceStatus,
    /// The integrator the user is currently active in, if any and if shared.
    pub active_in: Option<IntegratorId>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// A direct or small-group conversation — the identity-to-identity
/// sibling of [`crate::guilds::GuildChannel`], mirroring its shape: pure
/// structure, no integrator reference anywhere. A conversation between users is
/// a fact about their relationship, not about whichever integrator either of them
/// had open when it started — see `docs/projects/backend-server/architecture/communication.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: uuid::Uuid,
    pub participants: Vec<IdentityId>,
}

/// A single message within a [`Conversation`]. Deliberately **not** protocol
/// history, for the same reason [`crate::guilds::GuildMessage`] isn't:
/// high-volume, non-interoperable, nothing a receiving integrator
/// ever needs to verify. No `conversation.message_*` event kind exists, and
/// nothing in the send/read path touches `SettlementProvider::commit` — see
/// `crates/server/src/conversations.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub id: uuid::Uuid,
    pub conversation_id: uuid::Uuid,
    pub author: IdentityId,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    pub sent_at: OffsetDateTime,
}
