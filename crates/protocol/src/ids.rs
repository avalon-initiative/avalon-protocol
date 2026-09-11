//! Globally unique identifiers.
//!
//! Human-readable names (a game's slug, an achievement's key) are never
//! assumed to be globally unique on their own — see
//! `docs/architecture/achievements-and-attestations.md` (claim namespacing).
//! A `GlobalId` namespaces a human-readable key under the entity that issued
//! it, e.g. `game:ashen-realms:achievement:dragon_slayer`.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// An opaque, stable identity handle. Never derived from a display name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IdentityId(pub Uuid);

impl fmt::Display for IdentityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GameId(pub Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GuildId(pub Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AttestationId(pub Uuid);

impl fmt::Display for AttestationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A namespaced, human-readable identifier: `<namespace>:<owner>:<kind>:<key>`.
///
/// Example: `game:ashen-realms:achievement:dragon_slayer`. Two different games
/// can both define `dragon_slayer` without colliding, because the game's own
/// slug is part of the identifier, not just the achievement key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GlobalId(String);

impl GlobalId {
    pub fn new(namespace: &str, owner: &str, kind: &str, key: &str) -> Self {
        Self(format!("{namespace}:{owner}:{kind}:{key}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GlobalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
