//! User identity, separate from any integrator's character model.
//!
//! See GitHub issue #67 ("ADR: Identity Is Separate From Integrator Characters") for
//! why this boundary is mandatory rather than incidental.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::{GuildId, IdentityId};

/// The persistent, network-level user identity.
///
/// An `Identity` never references an integrator's character schema. It is the thing
/// that survives any single integrator shutting down.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub id: IdentityId,
    pub created_at: OffsetDateTime,
}

/// User-controlled, human-facing profile data.
///
/// Deliberately small and deliberately not the place where game-specific data
/// lives — see `Proposal.md` §19, "Identity vs. Integrator Data". `bio`,
/// `favorite_genres`, and `pronouns` (issue #155) are the "later" #86
/// flagged: small, user-optional, non-game-specific self-description,
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
    /// A second image slot, separate from `avatar_url`, for the Hub profile
    /// page header (issue #372). Same shape and same server-side validation
    /// as `avatar_url` (a well-formed `http`/`https` URL — see
    /// `crates/server/src/handlers.rs`'s `is_http_url`), reused directly
    /// rather than duplicated.
    pub banner_url: Option<String>,
    /// A short free-text tagline, capped server-side at [`MAX_STATUS_LEN`]
    /// characters — distinct from and shorter-capped than `bio`.
    pub status: Option<String>,
    /// A small fixed-size list of self-reported URLs, capped server-side at
    /// [`MAX_LINKS`] entries, each capped at [`MAX_LINK_LEN`] characters and
    /// required to parse as an `http`/`https` URL. Two-state like
    /// `favorite_genres`: omitted (untouched) or `Some(list)`, which always
    /// fully replaces the stored list, including `Some(vec![])` to clear it.
    pub links: Vec<String>,
    /// Self-reported free text, capped server-side at [`MAX_TIMEZONE_LEN`]
    /// characters. NOT validated against the real IANA time zone database —
    /// no such crate exists in this workspace today (issue #372); this is a
    /// documented, deliberate gap, not a silently-pretended correctness
    /// guarantee.
    pub timezone: Option<String>,
    /// A self-chosen accent color, validated server-side as a 6-digit hex
    /// color (`#rrggbb`). Purely cosmetic.
    pub theme_color: Option<String>,
    /// Free text, capped server-side at [`MAX_LOCATION_LEN`] characters.
    /// Self-described only — e.g. "Pacific Northwest" — and MUST NEVER be
    /// IP-derived or geocoded. This is load-bearing per issue #372: a future
    /// contributor must not silently add geolocation here.
    pub location: Option<String>,
    /// A self-chosen pointer to one of this identity's own current guild
    /// memberships (not scoped to any integrator/app/service category — a guild
    /// itself isn't category-scoped, see `GuildIntegratorAssociation` in
    /// `crates/protocol/src/guilds.rs`), so an integrator building a
    /// guild-chat-style UI has one guild to default to instead of having to
    /// support arbitrarily-many simultaneous memberships. Server-side
    /// (`crates/server/src/handlers.rs`) validates on every write that this
    /// names a guild the identity is *currently* a member of, and
    /// `crates/server/src/guilds.rs::leave_guild` clears it back to `None`
    /// in the same transaction if the identity leaves the guild it points
    /// at — this field must never dangle. `None` means "not explicitly
    /// set," not "no guild" — a caller wanting a default in that case
    /// computes one at read time (earliest-joined guild membership) rather
    /// than this field ever being written to reflect that default; see
    /// `crates/server/src/handlers.rs`'s `ProfileResponse::effective_main_guild`.
    pub main_guild: Option<GuildId>,
}

/// Server-side cap on `Profile::bio`'s length, in characters.
pub const MAX_BIO_LEN: usize = 500;

/// Server-side cap on how many entries `Profile::favorite_genres` may carry.
pub const MAX_FAVORITE_GENRES: usize = 5;

/// Server-side cap on `Profile::pronouns`'s length, in characters.
pub const MAX_PRONOUNS_LEN: usize = 40;

/// Server-side cap on `Profile::status`'s length, in characters.
pub const MAX_STATUS_LEN: usize = 100;

/// Server-side cap on how many entries `Profile::links` may carry.
pub const MAX_LINKS: usize = 5;

/// Server-side cap on each `Profile::links` entry's length, in characters.
pub const MAX_LINK_LEN: usize = 200;

/// Server-side cap on `Profile::timezone`'s length, in characters. See the
/// field's own doc comment: this is a length check only, not real IANA time
/// zone validation.
pub const MAX_TIMEZONE_LEN: usize = 64;

/// Server-side cap on `Profile::location`'s length, in characters.
pub const MAX_LOCATION_LEN: usize = 100;

/// The fixed, small controlled vocabulary `Profile::favorite_genres` draws
/// from (issue #155). Deliberately closed rather than free text — a bad
/// value here is more likely a real client bug than a schema drift, so it is
/// rejected server-side, not silently dropped (same reasoning
/// `GuildPermission` already established in `crates/protocol/src/guilds.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
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
