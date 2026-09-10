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

/// What kind of integrator a registrant is (issue #282, decision #275).
/// Additive only — does not rename `GameId`/`Issuer::Game`/`games`/`game.*`
/// events. Defaults to `Game`, so a caller that omits `category` is unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegratorCategory {
    #[default]
    Game,
    App,
    Service,
}

impl IntegratorCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            IntegratorCategory::Game => "game",
            IntegratorCategory::App => "app",
            IntegratorCategory::Service => "service",
        }
    }

    pub fn parse(s: &str) -> Option<IntegratorCategory> {
        Some(match s {
            "game" => IntegratorCategory::Game,
            "app" => IntegratorCategory::App,
            "service" => IntegratorCategory::Service,
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
    #[serde(default)]
    pub category: IntegratorCategory,
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

/// "This identity participates in this game" — nothing more (issue #83). No
/// characters, race, class, level, appearance, or progression: those stay in
/// the game's own database, and this type deliberately has no field for any
/// of them. See `docs/architecture/game-bindings.md`.
///
/// A binding is established by the **player**, through the consent flow
/// (issue #27, `POST /games/{slug}/connect`) — never created by a game
/// unilaterally. Capability grants (`PermissionGrant`, `permissions.rs`) are
/// scoped to a binding: no active binding, no grants, and ending a binding
/// ends every grant under it. Ending a binding does not delete history —
/// `ended_at` records when, it does not remove the row or the durable
/// `game.binding_established`/`game.binding_ended` events behind it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameBinding {
    pub identity_id: crate::ids::IdentityId,
    pub game_id: GameId,
    pub established_at: OffsetDateTime,
    pub ended_at: Option<OffsetDateTime>,
}

impl GameBinding {
    pub fn is_active(&self) -> bool {
        self.ended_at.is_none()
    }
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

    #[test]
    fn integrator_category_round_trips_through_its_wire_string() {
        assert_eq!(IntegratorCategory::Game.as_str(), "game");
        assert_eq!(IntegratorCategory::App.as_str(), "app");
        assert_eq!(IntegratorCategory::Service.as_str(), "service");
        assert_eq!(
            IntegratorCategory::parse("game"),
            Some(IntegratorCategory::Game)
        );
        assert_eq!(
            IntegratorCategory::parse("app"),
            Some(IntegratorCategory::App)
        );
        assert_eq!(
            IntegratorCategory::parse("service"),
            Some(IntegratorCategory::Service)
        );
        assert_eq!(IntegratorCategory::parse("bogus"), None);
    }

    #[test]
    fn integrator_category_defaults_to_game() {
        assert_eq!(IntegratorCategory::default(), IntegratorCategory::Game);
    }
}
