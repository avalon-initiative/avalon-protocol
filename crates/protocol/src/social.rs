//! Friends and presence — network-level social concepts, not game-scoped.
//!
//! A game never automatically receives a player's whole social graph; see
//! `permissions` for the capability that gates each of these reads.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::{GameId, IdentityId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Friendship {
    pub a: IdentityId,
    pub b: IdentityId,
    pub since: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PresenceStatus {
    Online,
    Away,
    Offline,
}

/// Where a player currently is, if they've opted to share it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Presence {
    pub identity_id: IdentityId,
    pub status: PresenceStatus,
    /// The game the player is currently in, if any and if shared.
    pub playing: Option<GameId>,
    pub updated_at: OffsetDateTime,
}
