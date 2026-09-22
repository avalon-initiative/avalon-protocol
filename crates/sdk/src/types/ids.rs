//! Globally unique identifiers, as they appear on the wire.
//!
//! Every one of these is a transparent newtype over the exact JSON the
//! server sends (a UUID string, or the `<namespace>:<owner>:<kind>:<key>`
//! text of a [`GlobalId`]) — they exist so a call site can't pass a guild
//! id where an identity id belongs, not to add any encoding of their own.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// An opaque, stable identity handle. Never derived from a display name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IdentityId(
    /// The identity's UUID, exactly as it appears on the wire.
    pub Uuid,
);

impl fmt::Display for IdentityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A registered integrator (game, app, or service).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IntegratorId(
    /// The integrator's UUID, exactly as it appears on the wire.
    pub Uuid,
);

impl fmt::Display for IntegratorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A guild, which exists independently of any one integrator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GuildId(
    /// The guild's UUID, exactly as it appears on the wire.
    pub Uuid,
);

impl fmt::Display for GuildId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A namespaced, human-readable identifier: `<namespace>:<owner>:<kind>:<key>`.
///
/// Example: `game:ashen-realms:achievement:dragon_slayer`. Two different
/// integrators can both define `dragon_slayer` without colliding, because
/// the integrator's own slug is part of the identifier, not just the
/// achievement key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GlobalId(String);

impl GlobalId {
    /// Builds `<namespace>:<owner>:<kind>:<key>`.
    pub fn new(namespace: &str, owner: &str, kind: &str, key: &str) -> Self {
        Self(format!("{namespace}:{owner}:{kind}:{key}"))
    }

    /// The full identifier text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GlobalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
