//! Full guild administration (issues #20/#21/#22/#152/#153/#169/#242/#250/
//! #442, on top of #23's read/roster surface `crate::guilds` already
//! covers for the integrator [`crate::Session`]) on
//! [`super::AccountSession`] — creation, roles, per-resource permission
//! overrides, ownership transfer, membership, invites, join requests,
//! channels, chat, and events, as an identity acting with its own
//! authority rather than through any integrator grant. See
//! `crates/server/src/guilds.rs`/`channels.rs`/`guild_messages.rs`/
//! `guild_events.rs`.

use time::OffsetDateTime;
use uuid::Uuid;

use crate::SdkError;

use super::AccountSession;

/// A guild, as returned by every guild-admin endpoint that hands back the
/// full record.
///
/// Doesn't yet expose `member_count`/`integrators`/`game_breakdown_public`/
/// `favorite_games`/`roster_visibility`, all of which the real
/// `GuildResponse` schema already carries — a known, pre-existing gap
/// (found migrating onto the generated type, not introduced by it), not
/// this migration's scope to close.
#[derive(Debug, Clone)]
pub struct Guild {
    /// This guild's own id.
    pub id: Uuid,
    /// Display name.
    pub name: String,
    /// Short unique tag.
    pub tag: String,
    /// Free-text description.
    pub description: String,
    /// The current owner.
    pub owner: Uuid,
    /// When the guild was created.
    pub created_at: OffsetDateTime,
    /// `"open"`, `"invite_only"`, or `"application"` — see
    /// `avalon_protocol::guilds::JoinPolicy`'s own stable vocabulary.
    pub join_policy: String,
    /// Message of the day, if set.
    pub motd: Option<String>,
    /// Banner image URL, if set.
    pub banner: Option<String>,
    /// Icon image URL, if set.
    pub icon: Option<String>,
    /// External links.
    pub links: Vec<avalon_protocol::guilds::GuildLink>,
    /// Whether the guild is currently recruiting.
    pub recruiting: bool,
    /// Whether the guild is publicly browsable/discoverable.
    pub public: bool,
}

impl TryFrom<crate::generated::GuildResponse> for Guild {
    type Error = SdkError;

    fn try_from(body: crate::generated::GuildResponse) -> Result<Self, SdkError> {
        Ok(Guild {
            id: body.id,
            name: body.name,
            tag: body.tag,
            description: body.description,
            owner: body.owner,
            created_at: super::parse_rfc3339(&body.created_at)?,
            join_policy: body.join_policy,
            motd: body.motd,
            banner: body.banner,
            icon: body.icon,
            links: body.links,
            recruiting: body.recruiting,
            public: body.public,
        })
    }
}

/// One guild in a discovery-board listing (`GET /guilds/discover`, issue
/// #154) — a narrower public summary than [`Guild`], not the full record.
#[derive(Debug, Clone)]
pub struct DiscoverGuildSummary {
    /// This guild's own id.
    pub id: Uuid,
    /// Display name.
    pub name: String,
    /// Short unique tag.
    pub tag: String,
    /// Free-text description.
    pub description: String,
    /// Whether currently recruiting.
    pub recruiting: bool,
    /// Current member count.
    pub member_count: i64,
    /// When the guild was created.
    pub created_at: OffsetDateTime,
    /// Banner image URL, if set.
    pub banner: Option<String>,
    /// Icon image URL, if set.
    pub icon: Option<String>,
}

impl TryFrom<crate::generated::DiscoverGuildSummary> for DiscoverGuildSummary {
    type Error = SdkError;

    fn try_from(body: crate::generated::DiscoverGuildSummary) -> Result<Self, SdkError> {
        Ok(DiscoverGuildSummary {
            id: body.id,
            name: body.name,
            tag: body.tag,
            description: body.description,
            recruiting: body.recruiting,
            member_count: body.member_count,
            created_at: super::parse_rfc3339(&body.created_at)?,
            banner: body.banner,
            icon: body.icon,
        })
    }
}

/// One page of a discovery-board listing.
#[derive(Debug, Clone)]
pub struct DiscoverGuildsPage {
    /// This page's own results.
    pub guilds: Vec<DiscoverGuildSummary>,
    /// `Some(id)` when another page exists — pass it back as `cursor=` in
    /// the next call's own query string.
    pub next_cursor: Option<Uuid>,
}

impl TryFrom<crate::generated::DiscoverGuildsResponse> for DiscoverGuildsPage {
    type Error = SdkError;

    fn try_from(body: crate::generated::DiscoverGuildsResponse) -> Result<Self, SdkError> {
        Ok(DiscoverGuildsPage {
            guilds: body
                .guilds
                .into_iter()
                .map(DiscoverGuildSummary::try_from)
                .collect::<Result<_, _>>()?,
            next_cursor: body.next_cursor,
        })
    }
}

/// One curated favorite-integrator pin (issue #207, decision #160).
#[derive(Debug, Clone)]
pub struct FavoriteGameEntry {
    /// The pinned integrator's id.
    pub integrator_id: Uuid,
    /// The pinned integrator's slug.
    pub integrator_slug: String,
    /// The pinned integrator's display name.
    pub integrator_name: String,
    /// 0-indexed curated display order.
    pub position: i16,
    /// `true` when this integrator no longer has any actively-bound guild
    /// member — a stale pin `manage_guild` may choose to unpin.
    pub stale: bool,
}

impl From<crate::generated::FavoriteGameEntry> for FavoriteGameEntry {
    fn from(body: crate::generated::FavoriteGameEntry) -> Self {
        FavoriteGameEntry {
            integrator_id: body.integrator_id,
            integrator_slug: body.integrator_slug,
            integrator_name: body.integrator_name,
            // Wire carries `position` as int32; this SDK's own type has
            // always narrowed it to i16 — unchanged by this migration.
            position: body.position as i16,
            stale: body.stale,
        }
    }
}

/// A guild's curated favorite-integrators list.
#[derive(Debug, Clone)]
pub struct FavoriteGames {
    /// The guild this list belongs to.
    pub guild_id: Uuid,
    /// The curated entries, in display order.
    pub favorites: Vec<FavoriteGameEntry>,
}

impl From<crate::generated::FavoriteGamesResponse> for FavoriteGames {
    fn from(body: crate::generated::FavoriteGamesResponse) -> Self {
        FavoriteGames {
            guild_id: body.guild_id,
            favorites: body.favorites.into_iter().map(Into::into).collect(),
        }
    }
}

/// A guild role definition.
#[derive(Debug, Clone)]
pub struct Role {
    /// The role's index within its guild (stable identifier, not a `Uuid`).
    pub name_index: i32,
    /// Display name.
    pub name: String,
    /// Guild-wide permission strings this role grants.
    pub permissions: Vec<String>,
    /// Free-text description.
    pub description: String,
    /// Badge (icon/color).
    pub badge: avalon_protocol::guilds::RoleBadge,
}

impl From<crate::generated::RoleResponse> for Role {
    fn from(body: crate::generated::RoleResponse) -> Self {
        Role {
            name_index: body.name_index,
            name: body.name,
            permissions: body.permissions,
            description: body.description,
            badge: body.badge,
        }
    }
}

/// A per-resource permission override.
#[derive(Debug, Clone)]
pub struct PermissionOverride {
    /// This override's own id.
    pub id: Uuid,
    /// The role it applies to.
    pub role_index: i32,
    /// The kind of resource overridden (e.g. `"channel"`).
    pub resource_kind: String,
    /// The specific resource overridden.
    pub resource_id: Uuid,
    /// The permission overridden.
    pub permission: String,
    /// Whether this override grants (`true`) or denies (`false`) it.
    pub allow: bool,
}

impl From<crate::generated::PermissionOverrideResponse> for PermissionOverride {
    fn from(body: crate::generated::PermissionOverrideResponse) -> Self {
        PermissionOverride {
            id: body.id,
            role_index: body.role_index,
            resource_kind: body.resource_kind,
            resource_id: body.resource_id,
            permission: body.permission,
            allow: body.allow,
        }
    }
}

/// One member of a guild's roster.
#[derive(Debug, Clone)]
pub struct GuildMember {
    /// The guild.
    pub guild_id: Uuid,
    /// The member.
    pub identity_id: Uuid,
    /// The member's current role index.
    pub role_index: i32,
    /// When they joined.
    pub joined_at: OffsetDateTime,
}

impl TryFrom<crate::generated::GuildMemberResponse> for GuildMember {
    type Error = SdkError;

    fn try_from(body: crate::generated::GuildMemberResponse) -> Result<Self, SdkError> {
        Ok(GuildMember {
            guild_id: body.guild_id,
            identity_id: body.identity_id,
            role_index: body.role_index,
            joined_at: super::parse_rfc3339(&body.joined_at)?,
        })
    }
}

/// The caller's own membership summary (`GET /me/guilds`).
#[derive(Debug, Clone)]
pub struct MyGuildMembership {
    /// The guild.
    pub guild_id: Uuid,
    /// The caller's own role index in it.
    pub role_index: i32,
    /// When the caller joined.
    pub joined_at: OffsetDateTime,
}

impl TryFrom<crate::generated::MyGuildMembershipResponse> for MyGuildMembership {
    type Error = SdkError;

    fn try_from(body: crate::generated::MyGuildMembershipResponse) -> Result<Self, SdkError> {
        Ok(MyGuildMembership {
            guild_id: body.guild_id,
            role_index: body.role_index,
            joined_at: super::parse_rfc3339(&body.joined_at)?,
        })
    }
}

/// A sent (or received) guild invite.
#[derive(Debug, Clone)]
pub struct GuildInvite {
    /// This invite's own id.
    pub id: Uuid,
    /// The guild it's for.
    pub guild_id: Uuid,
    /// The invitee.
    pub to: Uuid,
    /// The inviter.
    pub from: Uuid,
    /// When it was sent.
    pub created_at: OffsetDateTime,
}

impl TryFrom<crate::generated::GuildInviteResponse> for GuildInvite {
    type Error = SdkError;

    fn try_from(body: crate::generated::GuildInviteResponse) -> Result<Self, SdkError> {
        Ok(GuildInvite {
            id: body.id,
            guild_id: body.guild_id,
            to: body.to,
            from: body.from,
            created_at: super::parse_rfc3339(&body.created_at)?,
        })
    }
}

/// One of the caller's own pending invites, across every guild (`GET
/// /me/guild-invites`, issue #442).
#[derive(Debug, Clone)]
pub struct MyGuildInvite {
    /// This invite's own id.
    pub id: Uuid,
    /// The guild it's for.
    pub guild_id: Uuid,
    /// That guild's name.
    pub guild_name: String,
    /// The inviter.
    pub from: Uuid,
    /// When it was sent.
    pub created_at: OffsetDateTime,
}

impl TryFrom<crate::generated::MyGuildInviteResponse> for MyGuildInvite {
    type Error = SdkError;

    fn try_from(body: crate::generated::MyGuildInviteResponse) -> Result<Self, SdkError> {
        Ok(MyGuildInvite {
            id: body.id,
            guild_id: body.guild_id,
            guild_name: body.guild_name,
            from: body.from,
            created_at: super::parse_rfc3339(&body.created_at)?,
        })
    }
}

/// An applicant-initiated join request (issue #242).
#[derive(Debug, Clone)]
pub struct GuildJoinRequest {
    /// This request's own id.
    pub id: Uuid,
    /// The guild applied to.
    pub guild_id: Uuid,
    /// The applicant.
    pub applicant: Uuid,
    /// An optional message from the applicant.
    pub message: Option<String>,
    /// `"pending"`, `"approved"`, or `"rejected"`.
    pub status: String,
    /// When it was submitted.
    pub created_at: OffsetDateTime,
    /// When it was decided, if it has been.
    pub decided_at: Option<OffsetDateTime>,
    /// Who decided it, if it has been.
    pub decided_by: Option<Uuid>,
}

impl TryFrom<crate::generated::GuildJoinRequestResponse> for GuildJoinRequest {
    type Error = SdkError;

    fn try_from(body: crate::generated::GuildJoinRequestResponse) -> Result<Self, SdkError> {
        Ok(GuildJoinRequest {
            id: body.id,
            guild_id: body.guild_id,
            applicant: body.applicant,
            message: body.message,
            status: body.status,
            created_at: super::parse_rfc3339(&body.created_at)?,
            decided_at: body
                .decided_at
                .as_deref()
                .map(super::parse_rfc3339)
                .transpose()?,
            decided_by: body.decided_by,
        })
    }
}

/// A guild text channel.
#[derive(Debug, Clone)]
pub struct GuildChannel {
    /// This channel's own id.
    pub id: Uuid,
    /// The guild it belongs to.
    pub guild_id: Uuid,
    /// Display name.
    pub name: String,
    /// Whether it's archived.
    pub archived: bool,
    /// When it was created.
    pub created_at: OffsetDateTime,
    /// Whether posting requires the `channel_post` permission via override
    /// (issue #250) rather than any current member being able to post.
    pub announcement_only: bool,
    /// Short description shown in the channel header, if set.
    pub topic: Option<String>,
    /// Non-member visibility baseline for a public guild (issue #458).
    pub public: bool,
}

impl TryFrom<crate::generated::ChannelResponse> for GuildChannel {
    type Error = SdkError;

    fn try_from(body: crate::generated::ChannelResponse) -> Result<Self, SdkError> {
        Ok(GuildChannel {
            id: body.id,
            guild_id: body.guild_id,
            name: body.name,
            archived: body.archived,
            created_at: super::parse_rfc3339(&body.created_at)?,
            announcement_only: body.announcement_only,
            topic: body.topic,
            public: body.public,
        })
    }
}

/// A channel message.
#[derive(Debug, Clone)]
pub struct GuildMessage {
    /// This message's own id.
    pub id: Uuid,
    /// The channel it was posted in.
    pub channel_id: Uuid,
    /// The author.
    pub author: Uuid,
    /// The message body.
    pub body: String,
    /// When it was sent.
    pub sent_at: OffsetDateTime,
}

impl TryFrom<crate::generated::MessageResponse> for GuildMessage {
    type Error = SdkError;

    fn try_from(body: crate::generated::MessageResponse) -> Result<Self, SdkError> {
        Ok(GuildMessage {
            id: body.id,
            channel_id: body.channel_id,
            author: body.author,
            body: body.body,
            sent_at: super::parse_rfc3339(&body.sent_at)?,
        })
    }
}

/// RSVP tallies embedded in a [`GuildEvent`].
#[derive(Debug, Clone)]
pub struct RsvpCounts {
    /// Count of `"going"` responses.
    pub going: i64,
    /// Count of `"maybe"` responses.
    pub maybe: i64,
    /// Count of `"not_going"` responses.
    pub not_going: i64,
}

impl From<crate::generated::RsvpCounts> for RsvpCounts {
    fn from(body: crate::generated::RsvpCounts) -> Self {
        RsvpCounts {
            going: body.going,
            maybe: body.maybe,
            not_going: body.not_going,
        }
    }
}

/// A scheduled guild event.
///
/// Doesn't yet expose `details_visible`/`my_rsvp`, both of which the real
/// `EventResponse` schema already carries — a known, pre-existing gap
/// (found migrating onto the generated type), not this migration's scope
/// to close.
#[derive(Debug, Clone)]
pub struct GuildEvent {
    /// This event's own id.
    pub id: Uuid,
    /// The guild it belongs to.
    pub guild_id: Uuid,
    /// An optional associated channel.
    pub channel_id: Option<Uuid>,
    /// Title.
    pub title: String,
    /// Description, if any.
    pub description: Option<String>,
    /// Scheduled start time.
    pub starts_at: OffsetDateTime,
    /// Scheduled end time, if announced.
    pub ends_at: Option<OffsetDateTime>,
    /// Who created it.
    pub created_by: Uuid,
    /// When it was created.
    pub created_at: OffsetDateTime,
    /// RSVP tallies.
    pub rsvp_counts: RsvpCounts,
    /// Whether a non-member of a public guild may see this event (issue
    /// #448).
    pub public: bool,
}

impl TryFrom<crate::generated::EventResponse> for GuildEvent {
    type Error = SdkError;

    fn try_from(body: crate::generated::EventResponse) -> Result<Self, SdkError> {
        Ok(GuildEvent {
            id: body.id,
            guild_id: body.guild_id,
            channel_id: body.channel_id,
            title: body.title,
            description: body.description,
            starts_at: super::parse_rfc3339(&body.starts_at)?,
            ends_at: body
                .ends_at
                .as_deref()
                .map(super::parse_rfc3339)
                .transpose()?,
            created_by: body.created_by,
            created_at: super::parse_rfc3339(&body.created_at)?,
            rsvp_counts: body.rsvp_counts.into(),
            public: body.public,
        })
    }
}

/// The caller's own RSVP, as set by
/// [`AccountSession::rsvp_to_event`].
#[derive(Debug, Clone)]
pub struct Rsvp {
    /// The event.
    pub event_id: Uuid,
    /// The responder (always the caller).
    pub identity_id: Uuid,
    /// `"going"`, `"maybe"`, or `"not_going"`.
    pub status: String,
    /// When this RSVP was last set.
    pub responded_at: OffsetDateTime,
}

impl TryFrom<crate::generated::RsvpResponse> for Rsvp {
    type Error = SdkError;

    fn try_from(body: crate::generated::RsvpResponse) -> Result<Self, SdkError> {
        Ok(Rsvp {
            event_id: body.event_id,
            identity_id: body.identity_id,
            status: body.status,
            responded_at: super::parse_rfc3339(&body.responded_at)?,
        })
    }
}

/// One entry of an event's per-member RSVP roster (`GET
/// /guilds/{id}/events/{eid}/rsvps`, issue #248) — unlike [`Rsvp`], not
/// scoped to the caller's own response and doesn't carry `event_id` (the
/// server's own response shape doesn't repeat it per row).
#[derive(Debug, Clone)]
pub struct RsvpRosterEntry {
    /// The responder.
    pub identity_id: Uuid,
    /// `"going"`, `"maybe"`, or `"not_going"`.
    pub status: String,
    /// When this RSVP was last set.
    pub responded_at: OffsetDateTime,
}

impl TryFrom<crate::generated::RsvpRosterEntry> for RsvpRosterEntry {
    type Error = SdkError;

    fn try_from(body: crate::generated::RsvpRosterEntry) -> Result<Self, SdkError> {
        Ok(RsvpRosterEntry {
            identity_id: body.identity_id,
            status: body.status,
            responded_at: super::parse_rfc3339(&body.responded_at)?,
        })
    }
}

/// A partial update to a guild's own metadata — every field `None` means
/// "leave untouched," matching `PATCH /guilds/{id}`'s own convention.
#[derive(Debug, Clone, Default)]
pub struct GuildUpdate<'a> {
    /// New display name.
    pub name: Option<&'a str>,
    /// New tag.
    pub tag: Option<&'a str>,
    /// New description.
    pub description: Option<&'a str>,
    /// `None` leaves it untouched; `Some("")` clears it.
    pub motd: Option<&'a str>,
    /// `None` leaves it untouched; `Some("")` clears it.
    pub banner: Option<&'a str>,
    /// `None` leaves it untouched; `Some("")` clears it.
    pub icon: Option<&'a str>,
    /// Whether the guild is currently recruiting.
    pub recruiting: Option<bool>,
    /// Whether the guild is publicly browsable/discoverable.
    pub public: Option<bool>,
}

/// A partial update to a channel — `None` leaves that field untouched,
/// matching `PATCH /guilds/{id}/channels/{cid}`'s own convention.
#[derive(Debug, Clone, Default)]
pub struct ChannelUpdate<'a> {
    /// New channel name (always resent, not three-state).
    pub name: &'a str,
    /// `None` leaves it untouched.
    pub announcement_only: Option<bool>,
    /// `None` leaves it untouched; `Some("")` clears it.
    pub topic: Option<&'a str>,
    /// `None` leaves it untouched.
    pub public: Option<bool>,
}

/// Fields for creating or fully replacing a guild event — `PUT`-style full
/// replacement on update, matching `PATCH /guilds/{id}/events/{eid}`'s own
/// convention (unlike most other partial updates in this module).
#[derive(Debug, Clone)]
pub struct EventFields<'a> {
    /// Optional associated channel.
    pub channel_id: Option<Uuid>,
    /// Title.
    pub title: &'a str,
    /// Description, if any.
    pub description: Option<&'a str>,
    /// Scheduled start time.
    pub starts_at: OffsetDateTime,
    /// Scheduled end time, if announced.
    pub ends_at: Option<OffsetDateTime>,
    /// Whether a non-member of a public guild may see this event.
    pub public: bool,
}

impl AccountSession {
    /// `POST /guilds` — creates a new guild, the caller as owner. Not
    /// signature-required.
    pub async fn create_guild(
        &self,
        name: &str,
        tag: &str,
        description: &str,
    ) -> Result<Guild, SdkError> {
        let raw: crate::generated::GuildResponse = self
            .post(
                crate::generated::paths::guilds::CREATE_GUILD,
                &crate::generated::CreateGuildRequest {
                    name: name.to_string(),
                    tag: tag.to_string(),
                    description: if description.is_empty() {
                        None
                    } else {
                        Some(description.to_string())
                    },
                },
            )
            .await?;
        raw.try_into()
    }

    /// `GET /guilds/{id}`.
    pub async fn get_guild(&self, guild_id: Uuid) -> Result<Guild, SdkError> {
        let raw: crate::generated::GuildResponse = self
            .get(&super::path(
                crate::generated::paths::guilds::GET_GUILD,
                &[("id", &guild_id.to_string())],
            ))
            .await?;
        raw.try_into()
    }

    /// `GET /guilds/discover{query_string}` — `query_string` is passed
    /// through as-is (including its leading `?`), built by the caller;
    /// this crate doesn't replicate the Hub's own query-builder.
    pub async fn discover_guilds(
        &self,
        query_string: &str,
    ) -> Result<DiscoverGuildsPage, SdkError> {
        let raw: crate::generated::DiscoverGuildsResponse = self
            .get(&format!(
                "{}{query_string}",
                crate::generated::paths::guilds::DISCOVER_GUILDS
            ))
            .await?;
        raw.try_into()
    }

    /// `PATCH /guilds/{id}` — ordinary `manage_guild`-gated metadata
    /// edits. Not signature-required.
    ///
    /// Doesn't yet expose `game_breakdown_public`/`join_policy`/`links`/
    /// `roster_visibility`, all of which the real `UpdateGuildRequest`
    /// schema already accepts — a known, pre-existing gap (found migrating
    /// onto the generated type), not this migration's scope to close.
    pub async fn update_guild(
        &self,
        guild_id: Uuid,
        update: GuildUpdate<'_>,
    ) -> Result<Guild, SdkError> {
        let raw: crate::generated::GuildResponse = self
            .patch(
                &super::path(
                    crate::generated::paths::guilds::UPDATE_GUILD,
                    &[("id", &guild_id.to_string())],
                ),
                &crate::generated::UpdateGuildRequest {
                    name: update.name.map(str::to_string),
                    tag: update.tag.map(str::to_string),
                    description: update.description.map(str::to_string),
                    motd: update.motd.map(str::to_string),
                    banner: update.banner.map(str::to_string),
                    icon: update.icon.map(str::to_string),
                    recruiting: update.recruiting,
                    public: update.public,
                    game_breakdown_public: None,
                    join_policy: None,
                    links: None,
                    roster_visibility: None,
                },
            )
            .await?;
        raw.try_into()
    }

    /// `GET /guilds/{id}/roles`.
    pub async fn list_roles(&self, guild_id: Uuid) -> Result<Vec<Role>, SdkError> {
        let raw: Vec<crate::generated::RoleResponse> = self
            .get(&super::path(
                crate::generated::paths::guilds::LIST_ROLES,
                &[("id", &guild_id.to_string())],
            ))
            .await?;
        Ok(raw.into_iter().map(Into::into).collect())
    }

    /// `POST /guilds/{id}/roles` — always signs (`guild.role.create`,
    /// `[guild_id, name, permissions comma-joined]`).
    pub async fn create_role(
        &self,
        guild_id: Uuid,
        name: &str,
        permissions: &[&str],
        description: &str,
    ) -> Result<Role, SdkError> {
        let joined = permissions.join(",");
        let signature = self.sign("guild.role.create", &[&guild_id.to_string(), name, &joined]);
        let raw: crate::generated::RoleResponse = self
            .post(
                &super::path(
                    crate::generated::paths::guilds::CREATE_ROLE,
                    &[("id", &guild_id.to_string())],
                ),
                &crate::generated::CreateRoleRequest {
                    name: name.to_string(),
                    permissions: permissions.iter().map(|s| s.to_string()).collect(),
                    description: Some(description.to_string()),
                    badge: None,
                    signature: signature.signature,
                    signing_key_id: signature.signing_key_id,
                },
            )
            .await?;
        Ok(raw.into())
    }

    /// `PATCH /guilds/{id}/roles/{name_index}` — always signs
    /// (`guild.role.update`, `[guild_id, name_index]`).
    pub async fn update_role(
        &self,
        guild_id: Uuid,
        name_index: i32,
        name: Option<&str>,
        permissions: Option<&[&str]>,
        description: Option<&str>,
    ) -> Result<Role, SdkError> {
        let signature = self.sign(
            "guild.role.update",
            &[&guild_id.to_string(), &name_index.to_string()],
        );
        let raw: crate::generated::RoleResponse = self
            .patch(
                &super::path(
                    crate::generated::paths::guilds::UPDATE_ROLE,
                    &[
                        ("id", &guild_id.to_string()),
                        ("idx", &name_index.to_string()),
                    ],
                ),
                &crate::generated::UpdateRoleRequest {
                    name: name.map(str::to_string),
                    permissions: permissions.map(|p| p.iter().map(|s| s.to_string()).collect()),
                    description: description.map(str::to_string),
                    badge: None,
                    signature: signature.signature,
                    signing_key_id: signature.signing_key_id,
                },
            )
            .await?;
        Ok(raw.into())
    }

    /// `DELETE /guilds/{id}/roles/{name_index}` — always signs
    /// (`guild.role.delete`, `[guild_id, name_index]`).
    pub async fn delete_role(&self, guild_id: Uuid, name_index: i32) -> Result<(), SdkError> {
        let signature = self.sign(
            "guild.role.delete",
            &[&guild_id.to_string(), &name_index.to_string()],
        );
        self.delete_with_body(
            &super::path(
                crate::generated::paths::guilds::DELETE_ROLE,
                &[
                    ("id", &guild_id.to_string()),
                    ("idx", &name_index.to_string()),
                ],
            ),
            &crate::generated::DeleteRoleRequest {
                signature: signature.signature,
                signing_key_id: signature.signing_key_id,
            },
        )
        .await
    }

    /// `GET /guilds/{id}/permission-overrides?resource_kind=&resource_id=`.
    pub async fn list_permission_overrides(
        &self,
        guild_id: Uuid,
        resource_kind: &str,
        resource_id: Uuid,
    ) -> Result<Vec<PermissionOverride>, SdkError> {
        let resource_id_str = resource_id.to_string();
        let raw: Vec<crate::generated::PermissionOverrideResponse> = self
            .get_query(
                &super::path(
                    crate::generated::paths::guilds::LIST_PERMISSION_OVERRIDES,
                    &[("id", &guild_id.to_string())],
                ),
                &[
                    ("resource_kind", resource_kind),
                    ("resource_id", &resource_id_str),
                ],
            )
            .await?;
        Ok(raw.into_iter().map(Into::into).collect())
    }

    /// `PUT /guilds/{id}/permission-overrides` — always signs
    /// (`guild.permission_override.set`, `[guild_id, role_index,
    /// resource_kind, resource_id, permission, allow]`).
    #[allow(clippy::too_many_arguments)]
    pub async fn set_permission_override(
        &self,
        guild_id: Uuid,
        role_index: i32,
        resource_kind: &str,
        resource_id: Uuid,
        permission: &str,
        allow: bool,
    ) -> Result<PermissionOverride, SdkError> {
        let signature = self.sign(
            "guild.permission_override.set",
            &[
                &guild_id.to_string(),
                &role_index.to_string(),
                resource_kind,
                &resource_id.to_string(),
                permission,
                &allow.to_string(),
            ],
        );
        let raw: crate::generated::PermissionOverrideResponse = self
            .put(
                &super::path(
                    crate::generated::paths::guilds::SET_PERMISSION_OVERRIDE,
                    &[("id", &guild_id.to_string())],
                ),
                &crate::generated::SetPermissionOverrideRequest {
                    role_index,
                    resource_kind: resource_kind.to_string(),
                    resource_id,
                    permission: permission.to_string(),
                    allow,
                    signature: signature.signature,
                    signing_key_id: signature.signing_key_id,
                },
            )
            .await?;
        Ok(raw.into())
    }

    /// `DELETE /guilds/{id}/permission-overrides/{override_id}` — always
    /// signs (`guild.permission_override.delete`, `[guild_id,
    /// override_id]`).
    pub async fn delete_permission_override(
        &self,
        guild_id: Uuid,
        override_id: Uuid,
    ) -> Result<(), SdkError> {
        let signature = self.sign(
            "guild.permission_override.delete",
            &[&guild_id.to_string(), &override_id.to_string()],
        );
        self.delete_with_body(
            &super::path(
                crate::generated::paths::guilds::DELETE_PERMISSION_OVERRIDE,
                &[
                    ("id", &guild_id.to_string()),
                    ("override_id", &override_id.to_string()),
                ],
            ),
            &crate::generated::DeletePermissionOverrideRequest {
                signature: signature.signature,
                signing_key_id: signature.signing_key_id,
            },
        )
        .await
    }

    /// `POST /guilds/{id}/transfer-ownership` — owner-only, always signs
    /// (`guild.transfer_ownership`, `[guild_id, current owner, to]`).
    pub async fn transfer_ownership(&self, guild_id: Uuid, to: Uuid) -> Result<Guild, SdkError> {
        let signature = self.sign(
            "guild.transfer_ownership",
            &[
                &guild_id.to_string(),
                &self.identity().id.0.to_string(),
                &to.to_string(),
            ],
        );
        let raw: crate::generated::GuildResponse = self
            .post(
                &super::path(
                    crate::generated::paths::guilds::TRANSFER_OWNERSHIP,
                    &[("id", &guild_id.to_string())],
                ),
                &crate::generated::TransferOwnershipRequest {
                    to,
                    signature: signature.signature,
                    signing_key_id: signature.signing_key_id,
                },
            )
            .await?;
        raw.try_into()
    }

    /// `POST /guilds/{id}/integrations/{integrator_id}` — associates an
    /// integrator with a guild. Not signature-required.
    pub async fn associate_integrator(
        &self,
        guild_id: Uuid,
        integrator_id: Uuid,
    ) -> Result<Guild, SdkError> {
        let raw: crate::generated::GuildResponse = self
            .post_empty(&super::path(
                crate::generated::paths::guilds::ASSOCIATE_INTEGRATOR,
                &[
                    ("id", &guild_id.to_string()),
                    ("integrator_id", &integrator_id.to_string()),
                ],
            ))
            .await?;
        raw.try_into()
    }

    /// `GET /guilds/{id}/members`.
    pub async fn list_members(&self, guild_id: Uuid) -> Result<Vec<GuildMember>, SdkError> {
        let raw: Vec<crate::generated::GuildMemberResponse> = self
            .get(&super::path(
                crate::generated::paths::guilds::LIST_MEMBERS,
                &[("id", &guild_id.to_string())],
            ))
            .await?;
        raw.into_iter().map(GuildMember::try_from).collect()
    }

    /// `PATCH /guilds/{id}/members/{identity_id}` — role change; always
    /// signs (`guild.member_role.update`, `[guild_id, identity_id,
    /// role_index]`), whether or not this particular change actually
    /// escalates (the only case #697 requires it for) — same
    /// unused-but-valid-signature-is-harmless simplification used
    /// throughout this crate.
    pub async fn update_member_role(
        &self,
        guild_id: Uuid,
        identity_id: Uuid,
        role_index: i32,
    ) -> Result<GuildMember, SdkError> {
        let signature = self.sign(
            "guild.member_role.update",
            &[
                &guild_id.to_string(),
                &identity_id.to_string(),
                &role_index.to_string(),
            ],
        );
        let raw: crate::generated::GuildMemberResponse = self
            .patch(
                &super::path(
                    crate::generated::paths::guilds::UPDATE_MEMBER_ROLE,
                    &[
                        ("id", &guild_id.to_string()),
                        ("identity_id", &identity_id.to_string()),
                    ],
                ),
                &crate::generated::UpdateGuildMemberRequest {
                    role_index,
                    signature: signature.signature,
                    signing_key_id: signature.signing_key_id,
                },
            )
            .await?;
        raw.try_into()
    }

    /// `DELETE /guilds/{id}/members/{identity_id}` — kick, not a role
    /// change; reversible via re-invite. Not signature-required.
    pub async fn remove_member(&self, guild_id: Uuid, identity_id: Uuid) -> Result<(), SdkError> {
        self.delete(&super::path(
            crate::generated::paths::guilds::REMOVE_MEMBER,
            &[
                ("id", &guild_id.to_string()),
                ("identity_id", &identity_id.to_string()),
            ],
        ))
        .await
    }

    /// `GET /me/guilds`.
    pub async fn my_guilds(&self) -> Result<Vec<MyGuildMembership>, SdkError> {
        let raw: Vec<crate::generated::MyGuildMembershipResponse> = self
            .get(crate::generated::paths::guilds::LIST_MY_GUILDS)
            .await?;
        raw.into_iter().map(MyGuildMembership::try_from).collect()
    }

    /// `GET /me/guild-invites` (issue #442).
    pub async fn my_guild_invites(&self) -> Result<Vec<MyGuildInvite>, SdkError> {
        let raw: Vec<crate::generated::MyGuildInviteResponse> = self
            .get(crate::generated::paths::guilds::MY_GUILD_INVITES)
            .await?;
        raw.into_iter().map(MyGuildInvite::try_from).collect()
    }

    /// `POST /guilds/{id}/invites`.
    pub async fn create_guild_invite(
        &self,
        guild_id: Uuid,
        to: Uuid,
    ) -> Result<GuildInvite, SdkError> {
        let raw: crate::generated::GuildInviteResponse = self
            .post(
                &super::path(
                    crate::generated::paths::guilds::CREATE_INVITE,
                    &[("id", &guild_id.to_string())],
                ),
                &crate::generated::CreateGuildInviteRequest { to },
            )
            .await?;
        raw.try_into()
    }

    /// `POST /guilds/{id}/invites/{invite_id}/accept`.
    pub async fn accept_guild_invite(
        &self,
        guild_id: Uuid,
        invite_id: Uuid,
    ) -> Result<GuildMember, SdkError> {
        let raw: crate::generated::GuildMemberResponse = self
            .post_empty(&super::path(
                crate::generated::paths::guilds::ACCEPT_INVITE,
                &[
                    ("id", &guild_id.to_string()),
                    ("invite_id", &invite_id.to_string()),
                ],
            ))
            .await?;
        raw.try_into()
    }

    /// `POST /guilds/{id}/invites/{invite_id}/decline`.
    pub async fn decline_guild_invite(
        &self,
        guild_id: Uuid,
        invite_id: Uuid,
    ) -> Result<(), SdkError> {
        self.post_empty_no_response(&super::path(
            crate::generated::paths::guilds::DECLINE_INVITE,
            &[
                ("id", &guild_id.to_string()),
                ("invite_id", &invite_id.to_string()),
            ],
        ))
        .await
    }

    /// `POST /guilds/{id}/join` — only meaningful when the guild's join
    /// policy allows it (see `avalon_protocol::guilds::JoinPolicy`).
    pub async fn join_guild(&self, guild_id: Uuid) -> Result<GuildMember, SdkError> {
        let raw: crate::generated::GuildMemberResponse = self
            .post_empty(&super::path(
                crate::generated::paths::guilds::JOIN_GUILD,
                &[("id", &guild_id.to_string())],
            ))
            .await?;
        raw.try_into()
    }

    /// `POST /guilds/{id}/leave`.
    pub async fn leave_guild(&self, guild_id: Uuid) -> Result<(), SdkError> {
        self.post_empty_no_response(&super::path(
            crate::generated::paths::guilds::LEAVE_GUILD,
            &[("id", &guild_id.to_string())],
        ))
        .await
    }

    /// `POST /guilds/{id}/join-requests` (issue #242).
    pub async fn create_join_request(
        &self,
        guild_id: Uuid,
        message: Option<&str>,
    ) -> Result<GuildJoinRequest, SdkError> {
        let raw: crate::generated::GuildJoinRequestResponse = self
            .post(
                &super::path(
                    crate::generated::paths::guilds::CREATE_JOIN_REQUEST,
                    &[("id", &guild_id.to_string())],
                ),
                &crate::generated::CreateJoinRequestRequest {
                    message: message.map(str::to_string),
                },
            )
            .await?;
        raw.try_into()
    }

    /// `GET /guilds/{id}/join-requests` — `manage_members`-gated.
    pub async fn list_join_requests(
        &self,
        guild_id: Uuid,
    ) -> Result<Vec<GuildJoinRequest>, SdkError> {
        let raw: Vec<crate::generated::GuildJoinRequestResponse> = self
            .get(&super::path(
                crate::generated::paths::guilds::LIST_JOIN_REQUESTS,
                &[("id", &guild_id.to_string())],
            ))
            .await?;
        raw.into_iter().map(GuildJoinRequest::try_from).collect()
    }

    /// `GET /guilds/{id}/join-requests/mine` — the caller's own pending
    /// request for this guild, or `None` (issue #256).
    pub async fn my_join_request(
        &self,
        guild_id: Uuid,
    ) -> Result<Option<GuildJoinRequest>, SdkError> {
        let raw: Option<crate::generated::GuildJoinRequestResponse> = self
            .get(&super::path(
                crate::generated::paths::guilds::MY_JOIN_REQUEST,
                &[("id", &guild_id.to_string())],
            ))
            .await?;
        raw.map(GuildJoinRequest::try_from).transpose()
    }

    /// `POST /guilds/{id}/join-requests/{request_id}/approve`.
    pub async fn approve_join_request(
        &self,
        guild_id: Uuid,
        request_id: Uuid,
    ) -> Result<GuildMember, SdkError> {
        let raw: crate::generated::GuildMemberResponse = self
            .post_empty(&super::path(
                crate::generated::paths::guilds::APPROVE_JOIN_REQUEST,
                &[
                    ("id", &guild_id.to_string()),
                    ("request_id", &request_id.to_string()),
                ],
            ))
            .await?;
        raw.try_into()
    }

    /// `POST /guilds/{id}/join-requests/{request_id}/reject`.
    pub async fn reject_join_request(
        &self,
        guild_id: Uuid,
        request_id: Uuid,
    ) -> Result<(), SdkError> {
        self.post_empty_no_response(&super::path(
            crate::generated::paths::guilds::REJECT_JOIN_REQUEST,
            &[
                ("id", &guild_id.to_string()),
                ("request_id", &request_id.to_string()),
            ],
        ))
        .await
    }

    /// `DELETE /guilds/{id}/join-requests/{request_id}` — the applicant
    /// withdrawing their own request.
    pub async fn withdraw_join_request(
        &self,
        guild_id: Uuid,
        request_id: Uuid,
    ) -> Result<(), SdkError> {
        self.delete(&super::path(
            crate::generated::paths::guilds::WITHDRAW_JOIN_REQUEST,
            &[
                ("id", &guild_id.to_string()),
                ("request_id", &request_id.to_string()),
            ],
        ))
        .await
    }

    /// `GET /guilds/{id}/favorite-integrators` (issue #207).
    pub async fn favorite_games(&self, guild_id: Uuid) -> Result<FavoriteGames, SdkError> {
        let raw: crate::generated::FavoriteGamesResponse = self
            .get(&super::path(
                crate::generated::paths::guilds::LIST_FAVORITE_GAMES,
                &[("id", &guild_id.to_string())],
            ))
            .await?;
        Ok(raw.into())
    }

    /// `PUT /guilds/{id}/favorite-integrators` — `manage_guild`-gated, full
    /// ordered replacement. Not signature-required.
    pub async fn set_favorite_games(
        &self,
        guild_id: Uuid,
        integrator_ids: &[Uuid],
    ) -> Result<FavoriteGames, SdkError> {
        let raw: crate::generated::FavoriteGamesResponse = self
            .put(
                &super::path(
                    crate::generated::paths::guilds::SET_FAVORITE_GAMES,
                    &[("id", &guild_id.to_string())],
                ),
                &crate::generated::SetFavoriteGamesRequest {
                    integrator_ids: integrator_ids.to_vec(),
                },
            )
            .await?;
        Ok(raw.into())
    }

    /// `GET /guilds/{id}/channels`.
    pub async fn list_channels(&self, guild_id: Uuid) -> Result<Vec<GuildChannel>, SdkError> {
        let raw: Vec<crate::generated::ChannelResponse> = self
            .get(&super::path(
                crate::generated::paths::guilds::LIST_CHANNELS,
                &[("id", &guild_id.to_string())],
            ))
            .await?;
        raw.into_iter().map(GuildChannel::try_from).collect()
    }

    /// `POST /guilds/{id}/channels` — `manage_channels`-gated. Not
    /// signature-required (structural but reversible).
    pub async fn create_channel(
        &self,
        guild_id: Uuid,
        name: &str,
    ) -> Result<GuildChannel, SdkError> {
        let raw: crate::generated::ChannelResponse = self
            .post(
                &super::path(
                    crate::generated::paths::guilds::CREATE_CHANNEL,
                    &[("id", &guild_id.to_string())],
                ),
                &crate::generated::CreateChannelRequest {
                    name: name.to_string(),
                },
            )
            .await?;
        raw.try_into()
    }

    /// `PATCH /guilds/{id}/channels/{channel_id}`. Not signature-required.
    pub async fn update_channel(
        &self,
        guild_id: Uuid,
        channel_id: Uuid,
        update: ChannelUpdate<'_>,
    ) -> Result<GuildChannel, SdkError> {
        let raw: crate::generated::ChannelResponse = self
            .patch(
                &super::path(
                    crate::generated::paths::guilds::UPDATE_CHANNEL,
                    &[
                        ("id", &guild_id.to_string()),
                        ("cid", &channel_id.to_string()),
                    ],
                ),
                &crate::generated::UpdateChannelRequest {
                    name: update.name.to_string(),
                    announcement_only: update.announcement_only,
                    topic: update.topic.map(str::to_string),
                    public: update.public,
                },
            )
            .await?;
        raw.try_into()
    }

    /// `POST /guilds/{id}/channels/{channel_id}/archive`.
    pub async fn archive_channel(
        &self,
        guild_id: Uuid,
        channel_id: Uuid,
    ) -> Result<GuildChannel, SdkError> {
        let raw: crate::generated::ChannelResponse = self
            .post_empty(&super::path(
                crate::generated::paths::guilds::ARCHIVE_CHANNEL,
                &[
                    ("id", &guild_id.to_string()),
                    ("cid", &channel_id.to_string()),
                ],
            ))
            .await?;
        raw.try_into()
    }

    /// `GET /guilds/{id}/channels/{channel_id}/messages`, cursor-paginated.
    pub async fn channel_messages(
        &self,
        guild_id: Uuid,
        channel_id: Uuid,
        before: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<GuildMessage>, SdkError> {
        let mut query = Vec::new();
        if let Some(before) = before {
            query.push(("before", before.to_string()));
        }
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        let query_refs: Vec<(&str, &str)> = query.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let raw: Vec<crate::generated::MessageResponse> = self
            .get_query(
                &super::path(
                    crate::generated::paths::guilds::LIST_MESSAGES,
                    &[
                        ("id", &guild_id.to_string()),
                        ("cid", &channel_id.to_string()),
                    ],
                ),
                &query_refs,
            )
            .await?;
        raw.into_iter().map(GuildMessage::try_from).collect()
    }

    /// `POST /guilds/{id}/channels/{channel_id}/messages`. Not
    /// signature-required (chat, per #697's own invariants).
    pub async fn send_message(
        &self,
        guild_id: Uuid,
        channel_id: Uuid,
        body: &str,
    ) -> Result<GuildMessage, SdkError> {
        let raw: crate::generated::MessageResponse = self
            .post(
                &super::path(
                    crate::generated::paths::guilds::SEND_MESSAGE,
                    &[
                        ("id", &guild_id.to_string()),
                        ("cid", &channel_id.to_string()),
                    ],
                ),
                &crate::generated::SendMessageRequest {
                    body: body.to_string(),
                },
            )
            .await?;
        raw.try_into()
    }

    /// `DELETE /guilds/{id}/channels/{channel_id}/messages/{message_id}` —
    /// moderation delete, `manage_channels`-gated. Not signature-required.
    pub async fn delete_message(
        &self,
        guild_id: Uuid,
        channel_id: Uuid,
        message_id: Uuid,
    ) -> Result<(), SdkError> {
        self.delete(&super::path(
            crate::generated::paths::guilds::DELETE_MESSAGE,
            &[
                ("id", &guild_id.to_string()),
                ("cid", &channel_id.to_string()),
                ("mid", &message_id.to_string()),
            ],
        ))
        .await
    }

    /// `GET /guilds/{id}/events`, optionally windowed by `from`/`to`
    /// (RFC 3339).
    pub async fn list_events(
        &self,
        guild_id: Uuid,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<GuildEvent>, SdkError> {
        let mut query = Vec::new();
        if let Some(from) = from {
            query.push(("from", from));
        }
        if let Some(to) = to {
            query.push(("to", to));
        }
        let raw: Vec<crate::generated::EventResponse> = self
            .get_query(
                &super::path(
                    crate::generated::paths::guilds::LIST_EVENTS,
                    &[("id", &guild_id.to_string())],
                ),
                &query,
            )
            .await?;
        raw.into_iter().map(GuildEvent::try_from).collect()
    }

    /// `POST /guilds/{id}/events`. Not signature-required (reversible
    /// scheduling state).
    pub async fn create_event(
        &self,
        guild_id: Uuid,
        fields: EventFields<'_>,
    ) -> Result<GuildEvent, SdkError> {
        let raw: crate::generated::EventResponse = self
            .post(
                &super::path(
                    crate::generated::paths::guilds::CREATE_EVENT,
                    &[("id", &guild_id.to_string())],
                ),
                &crate::generated::CreateEventRequest {
                    channel_id: fields.channel_id,
                    title: fields.title.to_string(),
                    description: fields.description.map(str::to_string),
                    starts_at: super::format_rfc3339(fields.starts_at)?,
                    ends_at: fields.ends_at.map(super::format_rfc3339).transpose()?,
                    public: Some(fields.public),
                },
            )
            .await?;
        raw.try_into()
    }

    /// `PATCH /guilds/{id}/events/{event_id}` — full replacement, not
    /// partial (matches the server's own `UpdateEventRequest`).
    pub async fn update_event(
        &self,
        guild_id: Uuid,
        event_id: Uuid,
        fields: EventFields<'_>,
    ) -> Result<GuildEvent, SdkError> {
        let raw: crate::generated::EventResponse = self
            .patch(
                &super::path(
                    crate::generated::paths::guilds::UPDATE_EVENT,
                    &[
                        ("id", &guild_id.to_string()),
                        ("eid", &event_id.to_string()),
                    ],
                ),
                &crate::generated::UpdateEventRequest {
                    channel_id: fields.channel_id,
                    title: fields.title.to_string(),
                    description: fields.description.map(str::to_string),
                    starts_at: super::format_rfc3339(fields.starts_at)?,
                    ends_at: fields.ends_at.map(super::format_rfc3339).transpose()?,
                    public: Some(fields.public),
                },
            )
            .await?;
        raw.try_into()
    }

    /// `DELETE /guilds/{id}/events/{event_id}`.
    pub async fn delete_event(&self, guild_id: Uuid, event_id: Uuid) -> Result<(), SdkError> {
        self.delete(&super::path(
            crate::generated::paths::guilds::DELETE_EVENT,
            &[
                ("id", &guild_id.to_string()),
                ("eid", &event_id.to_string()),
            ],
        ))
        .await
    }

    /// `PUT /guilds/{id}/events/{event_id}/rsvp` — always sets the
    /// caller's own RSVP; `status` is `"going"`, `"maybe"`, or
    /// `"not_going"`.
    pub async fn rsvp_to_event(
        &self,
        guild_id: Uuid,
        event_id: Uuid,
        status: &str,
    ) -> Result<Rsvp, SdkError> {
        let raw: crate::generated::RsvpResponse = self
            .put(
                &super::path(
                    crate::generated::paths::guilds::UPSERT_RSVP,
                    &[
                        ("id", &guild_id.to_string()),
                        ("eid", &event_id.to_string()),
                    ],
                ),
                &crate::generated::RsvpRequest {
                    status: status.to_string(),
                },
            )
            .await?;
        raw.try_into()
    }

    /// `GET /guilds/{id}/events/{event_id}/rsvps` — the per-member roster.
    pub async fn event_rsvps(
        &self,
        guild_id: Uuid,
        event_id: Uuid,
    ) -> Result<Vec<RsvpRosterEntry>, SdkError> {
        let raw: Vec<crate::generated::RsvpRosterEntry> = self
            .get(&super::path(
                crate::generated::paths::guilds::LIST_RSVPS,
                &[
                    ("id", &guild_id.to_string()),
                    ("eid", &event_id.to_string()),
                ],
            ))
            .await?;
        raw.into_iter().map(RsvpRosterEntry::try_from).collect()
    }
}
