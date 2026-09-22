//! The persistent, network-level user identity and its profile.
//!
//! An `Identity` never references an integrator's character model — it is
//! the thing that survives any single integrator shutting down. Every cap
//! and vocabulary check on the profile fields below is enforced
//! server-side; this SDK never validates them locally and never silently
//! truncates a value on the way out.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::types::ids::{GuildId, IdentityId};

/// The fixed, small controlled vocabulary `Profile::favorite_genres` draws
/// from — generated from `docs/generated/openapi.json` by `build.rs`, and
/// re-exported here so callers get a domain-shaped path rather than the
/// codegen module.
pub use crate::generated::Genre;

/// The persistent, network-level user identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    /// This identity's opaque, stable handle.
    pub id: IdentityId,
    /// When the identity was first created.
    pub created_at: OffsetDateTime,
}

/// User-controlled, human-facing profile data — deliberately small, and
/// deliberately not where game-specific data lives.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    /// Whose profile this is.
    pub identity_id: IdentityId,
    /// The human-facing name, never an identifier.
    pub display_name: String,
    /// An `http`/`https` avatar image URL, if set.
    pub avatar_url: Option<String>,
    /// Free text, capped server-side.
    pub bio: Option<String>,
    /// A fixed, small controlled vocabulary (not free text), capped
    /// server-side at a handful of entries. An invalid value is rejected,
    /// not silently dropped.
    pub favorite_genres: Vec<Genre>,
    /// Free text, capped server-side.
    pub pronouns: Option<String>,
    /// A second image slot, separate from `avatar_url`, for a profile page
    /// header — same shape and same server-side validation as `avatar_url`.
    pub banner_url: Option<String>,
    /// A short free-text tagline — shorter-capped than `bio`.
    pub status: Option<String>,
    /// A small fixed-size list of self-reported `http`/`https` URLs. Always
    /// fully replaced on write, never patched per entry.
    pub links: Vec<String>,
    /// Self-reported free text, capped server-side. Not validated against
    /// the IANA time zone database.
    pub timezone: Option<String>,
    /// A self-chosen accent color, validated server-side as a 6-digit hex
    /// color (`#rrggbb`). Purely cosmetic.
    pub theme_color: Option<String>,
    /// Self-described free text only — never IP-derived or geocoded.
    pub location: Option<String>,
    /// A self-chosen pointer to one of this identity's own current guild
    /// memberships. `None` means "not explicitly set," not "no guild" — the
    /// server clears it if the identity leaves the guild it points at, so
    /// it never dangles.
    pub main_guild: Option<GuildId>,
}
