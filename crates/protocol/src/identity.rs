//! Player identity, separate from any game's character model.
//!
//! See GitHub issue #67 ("ADR: Identity Is Separate From Game Characters") for
//! why this boundary is mandatory rather than incidental.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::IdentityId;

/// The persistent, network-level player identity.
///
/// An `Identity` never references a game's character schema. It is the thing
/// that survives any single game shutting down.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub id: IdentityId,
    pub created_at: OffsetDateTime,
}

/// Player-controlled, human-facing profile data.
///
/// Deliberately small and deliberately not the place where game-specific data
/// lives — see `Proposal.md` §19, "Identity vs. Game Data".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub identity_id: IdentityId,
    pub display_name: String,
    pub avatar_url: Option<String>,
}
