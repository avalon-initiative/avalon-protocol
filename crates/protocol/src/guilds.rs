//! Guilds — network-level entities that exist independently of any one game.
//!
//! A guild is never assumed to belong to a single game; games optionally
//! associate their own game-specific guild representation with a network
//! guild. See `Proposal.md` §10.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::{GameId, GuildId, IdentityId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Guild {
    pub id: GuildId,
    pub name: String,
    pub tag: String,
    pub description: String,
    pub owner: IdentityId,
    pub created_at: OffsetDateTime,
    /// Whether the guild accepts open joins (`POST /guilds/{id}/join`) or
    /// requires an invite (issue #21). Defaults to `InviteOnly`.
    pub join_policy: JoinPolicy,
}

/// Whether a guild can be joined directly or only entered via invite
/// (issue #21). A guild setting, not a role permission — it governs who may
/// even attempt to join, before any role-based authority applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinPolicy {
    #[default]
    InviteOnly,
    Open,
}

impl JoinPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            JoinPolicy::InviteOnly => "invite_only",
            JoinPolicy::Open => "open",
        }
    }

    pub fn parse(s: &str) -> Option<JoinPolicy> {
        Some(match s {
            "invite_only" => JoinPolicy::InviteOnly,
            "open" => JoinPolicy::Open,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuildRole {
    pub guild_id: GuildId,
    pub name_index: u32,
}

/// Guild-level permissions a role can carry. Deliberately a small, fixed
/// set for milestone 1 (issue #20) rather than an open/extensible bitset —
/// custom role *names* are allowed, custom permissions are not, yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuildPermission {
    ManageGuild,
    ManageRoles,
    ManageMembers,
    ManageChannels,
}

impl GuildPermission {
    pub const ALL: [GuildPermission; 4] = [
        GuildPermission::ManageGuild,
        GuildPermission::ManageRoles,
        GuildPermission::ManageMembers,
        GuildPermission::ManageChannels,
    ];

    /// [`Self::ALL`], pre-rendered as strings — for seeding the starter
    /// `owner` role's `permissions` column without an allocation per call.
    pub const ALL_STRS: &'static [&'static str] = &[
        "manage_guild",
        "manage_roles",
        "manage_members",
        "manage_channels",
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            GuildPermission::ManageGuild => "manage_guild",
            GuildPermission::ManageRoles => "manage_roles",
            GuildPermission::ManageMembers => "manage_members",
            GuildPermission::ManageChannels => "manage_channels",
        }
    }

    pub fn parse(s: &str) -> Option<GuildPermission> {
        Some(match s {
            "manage_guild" => GuildPermission::ManageGuild,
            "manage_roles" => GuildPermission::ManageRoles,
            "manage_members" => GuildPermission::ManageMembers,
            "manage_channels" => GuildPermission::ManageChannels,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildMember {
    pub guild_id: GuildId,
    pub identity_id: IdentityId,
    pub role: GuildRole,
    pub joined_at: OffsetDateTime,
}

/// A game may optionally surface its own view of a network guild — this
/// association is opt-in and does not make the game the guild's owner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildGameAssociation {
    pub guild_id: GuildId,
    pub game_id: GameId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildChannel {
    pub id: uuid::Uuid,
    pub guild_id: GuildId,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildMessage {
    pub id: uuid::Uuid,
    pub channel_id: uuid::Uuid,
    pub author: IdentityId,
    pub body: String,
    pub sent_at: OffsetDateTime,
}
