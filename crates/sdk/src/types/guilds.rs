//! Guilds — network-level entities that exist independently of any one
//! integrator.
//!
//! A guild is never assumed to belong to a single integrator; integrators
//! optionally associate their own game-specific guild representation with a
//! network guild.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::types::ids::{GuildId, IdentityId};

/// A guild's external links, and a role's icon+color badge — generated from
/// `docs/generated/openapi.json` by `build.rs` and re-exported here under a
/// domain-shaped path.
pub use crate::generated::{GuildLink, RoleBadge, RoleBadgeColor, RoleBadgeIcon};

/// A guild as the server reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Guild {
    /// This guild's own id.
    pub id: GuildId,
    /// The guild's display name.
    pub name: String,
    /// A short tag/abbreviation.
    pub tag: String,
    /// Longer prose describing the guild.
    pub description: String,
    /// Who currently owns it.
    pub owner: IdentityId,
    /// When it was created.
    pub created_at: OffsetDateTime,
    /// Whether the guild accepts open joins or requires an invite.
    pub join_policy: JoinPolicy,
    /// Message-of-the-day — short, capped prose. `None` means unset; an
    /// empty string is never stored, it's normalized to `None` on the way in.
    pub motd: Option<String>,
    /// Wide cover-image URL. `None` means unset.
    pub banner: Option<String>,
    /// Small badge/icon image URL — a compact identity mark, distinct from
    /// [`Guild::banner`]'s wide cover-image role.
    pub icon: Option<String>,
    /// Small, capped, ordered list of external links. Always fully
    /// replaced on write, never patched per entry.
    pub links: Vec<GuildLink>,
    /// Whether this guild is advertising for new members — feeds the
    /// discovery board.
    pub recruiting: bool,
    /// Whether this guild's roster and public events are visible to any
    /// authenticated identity. Independent of [`Guild::recruiting`].
    pub public: bool,
}

/// Whether a guild can be joined directly or only entered via invite. A
/// guild setting, not a role permission — it governs who may even attempt
/// to join, before any role-based authority applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinPolicy {
    /// Only an invited identity may join.
    #[default]
    InviteOnly,
    /// Any authenticated identity may join directly.
    Open,
}

impl JoinPolicy {
    /// This policy's permanent wire string.
    pub fn as_str(&self) -> &'static str {
        match self {
            JoinPolicy::InviteOnly => "invite_only",
            JoinPolicy::Open => "open",
        }
    }

    /// Parses a wire string back into a policy; `None` if unrecognized.
    pub fn parse(s: &str) -> Option<JoinPolicy> {
        Some(match s {
            "invite_only" => JoinPolicy::InviteOnly,
            "open" => JoinPolicy::Open,
            _ => return None,
        })
    }
}

/// A role within one guild, identified by its position in that guild's own
/// role list rather than by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuildRole {
    /// The guild this role belongs to.
    pub guild_id: GuildId,
    /// The role's position in that guild's own role list.
    pub name_index: u32,
}

/// One identity's membership in one guild.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildMember {
    /// The guild this membership is in.
    pub guild_id: GuildId,
    /// Who the member is.
    pub identity_id: IdentityId,
    /// The role they hold.
    pub role: GuildRole,
    /// When they joined.
    pub joined_at: OffsetDateTime,
}

/// A text channel inside a guild.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildChannel {
    /// This channel's own id.
    pub id: uuid::Uuid,
    /// The guild it belongs to.
    pub guild_id: GuildId,
    /// The channel's display name.
    pub name: String,
    /// When `true`, posting requires an explicit per-channel grant rather
    /// than plain membership.
    pub announcement_only: bool,
    /// Short line describing what this channel is for. `None` means unset.
    pub topic: Option<String>,
}

/// One message in a guild channel. Hot state, not protocol history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildMessage {
    /// This message's own id.
    pub id: uuid::Uuid,
    /// The channel it was posted in.
    pub channel_id: uuid::Uuid,
    /// Who posted it.
    pub author: IdentityId,
    /// The message text.
    pub body: String,
    /// When it was posted.
    pub sent_at: OffsetDateTime,
}

/// A scheduled guild event — raid night, tournament prep, meetup. A plan
/// for something upcoming, not a durable claim about something that already
/// happened (that's an attestation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildEvent {
    /// This event's own id.
    pub id: uuid::Uuid,
    /// The guild running it.
    pub guild_id: GuildId,
    /// Optionally scopes the event to one of the guild's channels, for
    /// discussion. `None` means the event isn't tied to a specific channel.
    pub channel_id: Option<uuid::Uuid>,
    /// The event's title.
    pub title: String,
    /// Longer prose, if any.
    pub description: Option<String>,
    /// When it starts.
    pub starts_at: OffsetDateTime,
    /// `None` means open-ended / no announced end time.
    pub ends_at: Option<OffsetDateTime>,
    /// Who scheduled it.
    pub created_by: IdentityId,
    /// When it was scheduled.
    pub created_at: OffsetDateTime,
    /// Whether a non-member of a [`Guild::public`] guild may see this event.
    /// Never widens or narrows what a *member* sees.
    pub public: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_policy_round_trips_through_its_wire_string() {
        for policy in [JoinPolicy::InviteOnly, JoinPolicy::Open] {
            assert_eq!(JoinPolicy::parse(policy.as_str()), Some(policy));
        }
        assert_eq!(JoinPolicy::parse("request_to_join"), None);
    }
}
