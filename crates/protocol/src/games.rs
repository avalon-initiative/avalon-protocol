//! Game registration — how a game becomes known to Avalon and what it asks for.
//!
//! See `Proposal.md` §18. Registering does not grant any capability by
//! itself; a player must still authorize each capability (`permissions`).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::GameId;
use crate::permissions::Capability;

/// A game's registration status (issue #26). `Active` is the only variant
/// milestone 1 ever sets — suspension/revocation/deprecation (see
/// `docs/architecture/games-and-issuers.md`'s status column) are a later,
/// separate operator action, not built here. A growable enum rather than a
/// bare `bool` so those states have somewhere to land later without a wire
/// format change, same shape `JoinPolicy` (`crates/protocol/src/guilds.rs`)
/// already established in this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameStatus {
    Active,
}

impl GameStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            GameStatus::Active => "active",
        }
    }

    pub fn parse(s: &str) -> Option<GameStatus> {
        Some(match s {
            "active" => GameStatus::Active,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Game {
    pub id: GameId,
    pub slug: String,
    pub name: String,
    pub developer: String,
    pub registered_at: OffsetDateTime,
    pub status: GameStatus,
}

/// The first signing key a game registers with (issue #26). Shaped so issue
/// #84 (issuer key lifecycle — rotation, multiple keys, revocation) can
/// extend rather than replace it: this only ever describes the one key
/// recorded at registration time, never a full key history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssuerKeyInfo {
    pub key_id: String,
    pub algorithm: String,
    pub public_key: Vec<u8>,
}

/// The capabilities a game declares it wants, presented to the player before
/// they connect their identity — not a grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameRegistration {
    pub game: Game,
    pub requested_capabilities: Vec<Capability>,
    pub initial_key: IssuerKeyInfo,
}

/// A credential a game uses to authenticate itself to Avalon (server-to-server),
/// distinct from a player's own session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameCredential {
    pub game_id: GameId,
    pub key_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_status_round_trips_through_its_wire_string() {
        assert_eq!(GameStatus::Active.as_str(), "active");
        assert_eq!(GameStatus::parse("active"), Some(GameStatus::Active));
        assert_eq!(GameStatus::parse("bogus"), None);
    }
}
