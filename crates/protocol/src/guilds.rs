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
    /// Message-of-the-day (issue #153) — short, capped prose set by the
    /// owner/`manage_guild`. `None` means unset, same "no value" convention
    /// `identity::Profile.bio` already uses; an empty string is never
    /// stored, it's normalized to `None` on the way in.
    pub motd: Option<String>,
    /// Banner image URL (issue #153) — same `http`/`https`-only, length-capped
    /// validation as a profile's `avatar_url`. `None` means unset.
    pub banner: Option<String>,
    /// Small badge/icon image URL (issue #246) — a compact identity mark,
    /// distinct from [`Guild::banner`]'s wide cover-image role. Same
    /// `http`/`https`-only, length-capped validation as `banner`. `None`
    /// means unset.
    pub icon: Option<String>,
    /// Small, capped list of external links (Discord, website, ...) the
    /// guild wants to point at (issue #153). Ordered; a caller that wants a
    /// different order resends the whole list, same "full replace, not a
    /// per-entry patch" convention `favorite_genres` already established.
    pub links: Vec<GuildLink>,
    /// Whether this guild is advertising for new members (issue #153). Feeds
    /// the discovery board (issue #154) — a real, queryable column, not
    /// derived from anything else.
    pub recruiting: bool,
}

/// One entry in [`Guild::links`] (issue #153): a human label paired with the
/// URL it points at. Both fields are validated/capped server-side
/// (`crates/server/src/guilds.rs`) — this type carries no invariant of its
/// own beyond "these are the two fields a link has."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuildLink {
    pub label: String,
    pub url: String,
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

/// Fixed milestone-1 vocabulary of role badge icons (issue #152). Not
/// user-uploadable — a role's icon is chosen from this closed set, same
/// "custom names allowed, custom permissions/values not yet" precedent
/// [`GuildPermission`] already established for milestone 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleBadgeIcon {
    Shield,
    Crown,
    Star,
    Sword,
    Wrench,
    Heart,
    Flag,
    Bolt,
}

impl RoleBadgeIcon {
    pub const ALL: [RoleBadgeIcon; 8] = [
        RoleBadgeIcon::Shield,
        RoleBadgeIcon::Crown,
        RoleBadgeIcon::Star,
        RoleBadgeIcon::Sword,
        RoleBadgeIcon::Wrench,
        RoleBadgeIcon::Heart,
        RoleBadgeIcon::Flag,
        RoleBadgeIcon::Bolt,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            RoleBadgeIcon::Shield => "shield",
            RoleBadgeIcon::Crown => "crown",
            RoleBadgeIcon::Star => "star",
            RoleBadgeIcon::Sword => "sword",
            RoleBadgeIcon::Wrench => "wrench",
            RoleBadgeIcon::Heart => "heart",
            RoleBadgeIcon::Flag => "flag",
            RoleBadgeIcon::Bolt => "bolt",
        }
    }

    pub fn parse(s: &str) -> Option<RoleBadgeIcon> {
        Some(match s {
            "shield" => RoleBadgeIcon::Shield,
            "crown" => RoleBadgeIcon::Crown,
            "star" => RoleBadgeIcon::Star,
            "sword" => RoleBadgeIcon::Sword,
            "wrench" => RoleBadgeIcon::Wrench,
            "heart" => RoleBadgeIcon::Heart,
            "flag" => RoleBadgeIcon::Flag,
            "bolt" => RoleBadgeIcon::Bolt,
            _ => return None,
        })
    }
}

/// Fixed milestone-1 vocabulary of role badge colors — same closed-set
/// reasoning as [`RoleBadgeIcon`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleBadgeColor {
    Gray,
    Red,
    Orange,
    Gold,
    Green,
    Blue,
    Purple,
}

impl RoleBadgeColor {
    pub const ALL: [RoleBadgeColor; 7] = [
        RoleBadgeColor::Gray,
        RoleBadgeColor::Red,
        RoleBadgeColor::Orange,
        RoleBadgeColor::Gold,
        RoleBadgeColor::Green,
        RoleBadgeColor::Blue,
        RoleBadgeColor::Purple,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            RoleBadgeColor::Gray => "gray",
            RoleBadgeColor::Red => "red",
            RoleBadgeColor::Orange => "orange",
            RoleBadgeColor::Gold => "gold",
            RoleBadgeColor::Green => "green",
            RoleBadgeColor::Blue => "blue",
            RoleBadgeColor::Purple => "purple",
        }
    }

    pub fn parse(s: &str) -> Option<RoleBadgeColor> {
        Some(match s {
            "gray" => RoleBadgeColor::Gray,
            "red" => RoleBadgeColor::Red,
            "orange" => RoleBadgeColor::Orange,
            "gold" => RoleBadgeColor::Gold,
            "green" => RoleBadgeColor::Green,
            "blue" => RoleBadgeColor::Blue,
            "purple" => RoleBadgeColor::Purple,
            _ => return None,
        })
    }
}

/// A role's small, fixed visual identity (issue #152): an icon id from a
/// closed enum paired with a color id from a closed enum — deliberately
/// not a free-form asset/upload, no user-supplied image hosting in scope
/// for milestone 1. Shaped as icon+color today (rather than e.g. a single
/// opaque badge id) so it can grow into a richer badge system later —
/// more icons/colors, tiers, an uploaded custom asset as an additional
/// variant — without a breaking change to callers that just want "an icon
/// and a color" out of a role (`packages/ui`'s planned `AvalonRoleBadge`,
/// #24, is the first such caller).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleBadge {
    pub icon: RoleBadgeIcon,
    pub color: RoleBadgeColor,
}

impl RoleBadge {
    /// Assigned to a role that doesn't specify a badge (e.g. the `member`
    /// starter role, or a `create_role` call that omits one).
    pub const DEFAULT: RoleBadge = RoleBadge {
        icon: RoleBadgeIcon::Star,
        color: RoleBadgeColor::Gray,
    };
}

impl Default for RoleBadge {
    fn default() -> Self {
        RoleBadge::DEFAULT
    }
}

/// Guild-level permissions a role can carry. A small, fixed vocabulary for
/// milestone 1 (issue #20) rather than an open/extensible bitset — custom
/// role *names* are allowed, custom permissions are not, yet — but no
/// longer a *closed* set: issue #250 (decided by #243) adds a per-resource
/// override layer on top (see [`GuildPermissionOverride`]) without
/// changing this enum's own closed-vocabulary posture.
///
/// `EventManage` was added by #250 and no longer piggybacks on
/// `ManageChannels` the way #169's guild-events work originally had it —
/// see `crates/server/src/guild_events.rs`. `ChannelPost` was added by the
/// same ticket to prove the override layer out end-to-end via
/// announcement-only channels (`GuildChannel`/`guild_channels.announcement_only`
/// below): a channel in that mode requires `ChannelPost` to post, resolved
/// per-channel through the override layer rather than a guild-wide grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuildPermission {
    ManageGuild,
    ManageRoles,
    ManageMembers,
    ManageChannels,
    EventManage,
    ChannelPost,
}

impl GuildPermission {
    pub const ALL: [GuildPermission; 6] = [
        GuildPermission::ManageGuild,
        GuildPermission::ManageRoles,
        GuildPermission::ManageMembers,
        GuildPermission::ManageChannels,
        GuildPermission::EventManage,
        GuildPermission::ChannelPost,
    ];

    /// [`Self::ALL`], pre-rendered as strings — for seeding the starter
    /// `owner` role's `permissions` column without an allocation per call.
    pub const ALL_STRS: &'static [&'static str] = &[
        "manage_guild",
        "manage_roles",
        "manage_members",
        "manage_channels",
        "event_manage",
        "channel_post",
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            GuildPermission::ManageGuild => "manage_guild",
            GuildPermission::ManageRoles => "manage_roles",
            GuildPermission::ManageMembers => "manage_members",
            GuildPermission::ManageChannels => "manage_channels",
            GuildPermission::EventManage => "event_manage",
            GuildPermission::ChannelPost => "channel_post",
        }
    }

    pub fn parse(s: &str) -> Option<GuildPermission> {
        Some(match s {
            "manage_guild" => GuildPermission::ManageGuild,
            "manage_roles" => GuildPermission::ManageRoles,
            "manage_members" => GuildPermission::ManageMembers,
            "manage_channels" => GuildPermission::ManageChannels,
            "event_manage" => GuildPermission::EventManage,
            "channel_post" => GuildPermission::ChannelPost,
            _ => return None,
        })
    }
}

/// A guild resource a [`GuildPermissionOverride`] can be scoped to (issue
/// #250) — currently a channel or an event, the two resource-shaped
/// things channel/event endpoints in `crates/server/src/guilds.rs` gate on.
/// Fixed, closed set — same posture as [`JoinPolicy`]/[`RoleBadgeIcon`]
/// elsewhere in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuildResourceKind {
    Channel,
    Event,
}

impl GuildResourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            GuildResourceKind::Channel => "channel",
            GuildResourceKind::Event => "event",
        }
    }

    pub fn parse(s: &str) -> Option<GuildResourceKind> {
        Some(match s {
            "channel" => GuildResourceKind::Channel,
            "event" => GuildResourceKind::Event,
            _ => return None,
        })
    }
}

/// A per-resource override on top of a role's flat [`GuildPermission`]
/// base list (issue #250, shape decided by #243): grants or denies one
/// permission to one role, scoped to a single channel or event. Base role
/// permissions (`GuildRole`'s stored `permissions` list) stay the source
/// of truth when no override row exists for a resource; when one does, an
/// explicit deny always wins over a base grant and an explicit grant
/// always wins over a base absence. Owner bypass
/// (`crates/server/src/guilds.rs::has_guild_permission`'s structural
/// owner check) is untouched by overrides — see
/// `has_resource_permission`/`resolve_resource_permission` in that module
/// for the resolution logic this type feeds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildPermissionOverride {
    pub id: uuid::Uuid,
    pub guild_id: GuildId,
    pub role_index: i32,
    pub resource_kind: GuildResourceKind,
    pub resource_id: uuid::Uuid,
    pub permission: GuildPermission,
    /// `true` grants, `false` explicitly denies — there is no third
    /// "inert" row state; the absence of any row for a given
    /// (role, resource, permission) triple is what "no override" means.
    pub allow: bool,
    pub created_at: OffsetDateTime,
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
    /// Issue #250's channel-organization proof point for the per-resource
    /// override layer: when `true`, posting requires the `ChannelPost`
    /// [`GuildPermission`], resolved per-channel through the override
    /// layer rather than a role's guild-wide base list (a role earns post
    /// access in an announcement-only channel only via an explicit
    /// `ChannelPost` override on that channel, or by owning the guild).
    /// `false` (the default) keeps today's behavior: any current guild
    /// member may post.
    pub announcement_only: bool,
    /// Short line describing what this channel is for (issue #276),
    /// rendered in the channel header — same "no value" convention as
    /// `Guild::motd`/`Guild::banner`: `None` means unset, an empty string
    /// is never stored (normalized to `None` server-side on the way in).
    /// Editable by whoever can manage the channel, same gate as a rename.
    pub topic: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildMessage {
    pub id: uuid::Uuid,
    pub channel_id: uuid::Uuid,
    pub author: IdentityId,
    pub body: String,
    pub sent_at: OffsetDateTime,
}

/// A scheduled guild event (issue #169) — raid night, tournament prep,
/// meetup, anything a guild plans in advance. Distinct from #88's game
/// event *result* attestations: this is a plan for something upcoming, not
/// a durable claim about something that already happened.
///
/// `guild_events` (and `GuildEventRsvp` below) are a projection-only table
/// with no `guild.event_*` protocol event kind — see
/// `crates/server/src/guild_events.rs`'s module doc comment for the full
/// durability reasoning (same "hot state, not history" treatment #22 gave
/// chat messages).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildEvent {
    pub id: uuid::Uuid,
    pub guild_id: GuildId,
    /// Optionally scopes the event to one of the guild's channels, for
    /// discussion. `None` means the event isn't tied to a specific channel.
    pub channel_id: Option<uuid::Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub starts_at: OffsetDateTime,
    /// `None` means open-ended / no announced end time.
    pub ends_at: Option<OffsetDateTime>,
    pub created_by: IdentityId,
    pub created_at: OffsetDateTime,
}

/// A member's RSVP to a `GuildEvent`. One row per (event, identity) — a
/// member can only ever mutate their own row, never another's.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildEventRsvp {
    pub event_id: uuid::Uuid,
    pub identity_id: IdentityId,
    pub status: RsvpStatus,
    pub responded_at: OffsetDateTime,
}

/// Fixed, closed set — same "small, not extensible" posture as
/// `JoinPolicy`/`RoleBadgeIcon` elsewhere in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RsvpStatus {
    Going,
    Maybe,
    NotGoing,
}

impl RsvpStatus {
    pub const ALL: [RsvpStatus; 3] = [RsvpStatus::Going, RsvpStatus::Maybe, RsvpStatus::NotGoing];

    pub fn as_str(&self) -> &'static str {
        match self {
            RsvpStatus::Going => "going",
            RsvpStatus::Maybe => "maybe",
            RsvpStatus::NotGoing => "not_going",
        }
    }

    pub fn parse(s: &str) -> Option<RsvpStatus> {
        match s {
            "going" => Some(RsvpStatus::Going),
            "maybe" => Some(RsvpStatus::Maybe),
            "not_going" => Some(RsvpStatus::NotGoing),
            _ => None,
        }
    }
}
