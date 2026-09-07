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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuildRole {
    pub guild_id: GuildId,
    pub name_index: u32,
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
