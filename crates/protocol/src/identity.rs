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
/// lives — see `Proposal.md` §19, "Identity vs. Game Data". `bio`,
/// `favorite_genres`, and `pronouns` (issue #155) are the "later" #86
/// flagged: small, player-optional, non-game-specific self-description,
/// same promised-durable tier as `display_name`/`avatar_url` — see
/// `docs/architecture/identity.md`'s durable-field table. Server-side
/// validation of these (length caps, vocabulary membership) lives in
/// `crates/server/src/handlers.rs`, not here — this crate is domain types
/// only, no I/O, no validation logic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub identity_id: IdentityId,
    pub display_name: String,
    pub avatar_url: Option<String>,
    /// Free text, capped server-side at [`MAX_BIO_LEN`] characters.
    pub bio: Option<String>,
    /// A fixed, small controlled vocabulary (not free text), so it stays
    /// useful for matching/filtering later — capped server-side at
    /// [`MAX_FAVORITE_GENRES`] entries. Invalid values are rejected, not
    /// silently dropped.
    pub favorite_genres: Vec<Genre>,
    /// Free text, capped server-side at [`MAX_PRONOUNS_LEN`] characters.
    pub pronouns: Option<String>,
}

/// Server-side cap on `Profile::bio`'s length, in characters.
pub const MAX_BIO_LEN: usize = 500;

/// Server-side cap on how many entries `Profile::favorite_genres` may carry.
pub const MAX_FAVORITE_GENRES: usize = 5;

/// Server-side cap on `Profile::pronouns`'s length, in characters.
pub const MAX_PRONOUNS_LEN: usize = 40;

/// The fixed, small controlled vocabulary `Profile::favorite_genres` draws
/// from (issue #155). Deliberately closed rather than free text — a bad
/// value here is more likely a real client bug than a schema drift, so it is
/// rejected server-side, not silently dropped (same reasoning
/// `GuildPermission` already established in `crates/protocol/src/guilds.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Genre {
    Action,
    Adventure,
    Rpg,
    Strategy,
    Simulation,
    Puzzle,
    Racing,
    Sports,
    Horror,
    Sandbox,
    Mmo,
    Shooter,
    Platformer,
    Party,
}

impl Genre {
    pub const ALL: [Genre; 14] = [
        Genre::Action,
        Genre::Adventure,
        Genre::Rpg,
        Genre::Strategy,
        Genre::Simulation,
        Genre::Puzzle,
        Genre::Racing,
        Genre::Sports,
        Genre::Horror,
        Genre::Sandbox,
        Genre::Mmo,
        Genre::Shooter,
        Genre::Platformer,
        Genre::Party,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Genre::Action => "action",
            Genre::Adventure => "adventure",
            Genre::Rpg => "rpg",
            Genre::Strategy => "strategy",
            Genre::Simulation => "simulation",
            Genre::Puzzle => "puzzle",
            Genre::Racing => "racing",
            Genre::Sports => "sports",
            Genre::Horror => "horror",
            Genre::Sandbox => "sandbox",
            Genre::Mmo => "mmo",
            Genre::Shooter => "shooter",
            Genre::Platformer => "platformer",
            Genre::Party => "party",
        }
    }

    pub fn parse(s: &str) -> Option<Genre> {
        Some(match s {
            "action" => Genre::Action,
            "adventure" => Genre::Adventure,
            "rpg" => Genre::Rpg,
            "strategy" => Genre::Strategy,
            "simulation" => Genre::Simulation,
            "puzzle" => Genre::Puzzle,
            "racing" => Genre::Racing,
            "sports" => Genre::Sports,
            "horror" => Genre::Horror,
            "sandbox" => Genre::Sandbox,
            "mmo" => Genre::Mmo,
            "shooter" => Genre::Shooter,
            "platformer" => Genre::Platformer,
            "party" => Genre::Party,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_genre_round_trips_through_as_str_and_parse() {
        for genre in Genre::ALL {
            assert_eq!(Genre::parse(genre.as_str()), Some(genre));
        }
    }

    #[test]
    fn parse_rejects_an_unknown_genre() {
        assert_eq!(Genre::parse("visual_novel"), None);
    }
}
