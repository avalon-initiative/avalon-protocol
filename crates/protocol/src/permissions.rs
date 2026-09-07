//! Explicit, capability-based permissions.
//!
//! Least privilege by default: a game receives only the capabilities a player
//! has actually authorized, never everything associated with an identity.
//! See `Proposal.md` §13.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::{GameId, IdentityId};

/// A single scoped permission string, e.g. `"achievements.issue"`.
///
/// Kept as a newtype over `String` rather than a closed enum so the
/// capability list can grow without a protocol version bump for every
/// addition — see `Proposal.md` §13 for the starting list.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Capability(pub String);

impl Capability {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

/// A capability a player has actually granted to a specific game.
///
/// Revocable and scoped: this record is the entire answer to "can Game X do
/// Y for Player Z," not an implicit `game_has_access_to_player = true`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionGrant {
    pub identity_id: IdentityId,
    pub game_id: GameId,
    pub capability: Capability,
    pub granted_at: OffsetDateTime,
    pub revoked_at: Option<OffsetDateTime>,
}

impl PermissionGrant {
    pub fn is_active(&self, now: OffsetDateTime) -> bool {
        match self.revoked_at {
            Some(revoked_at) => revoked_at > now,
            None => true,
        }
    }
}
