//! Game registration — how a game becomes known to Avalon and what it asks for.
//!
//! See `Proposal.md` §18. Registering does not grant any capability by
//! itself; a player must still authorize each capability (`permissions`).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::GameId;
use crate::permissions::Capability;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Game {
    pub id: GameId,
    pub slug: String,
    pub name: String,
    pub developer: String,
    pub registered_at: OffsetDateTime,
}

/// The capabilities a game declares it wants, presented to the player before
/// they connect their identity — not a grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameRegistration {
    pub game: Game,
    pub requested_capabilities: Vec<Capability>,
}

/// A credential a game uses to authenticate itself to Avalon (server-to-server),
/// distinct from a player's own session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameCredential {
    pub game_id: GameId,
    pub key_id: String,
}
