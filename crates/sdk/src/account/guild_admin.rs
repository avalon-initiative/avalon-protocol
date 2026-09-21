//! Full guild administration (issues #20/#21/#22/#152/#153/#169/#242/#250/
//! #442, on top of #23's read/roster surface `crate::guilds` already
//! covers for the integrator [`crate::Session`]) on
//! [`super::AccountSession`] — creation, roles, per-resource permission
//! overrides, ownership transfer, membership, invites, join requests,
//! channels, chat, and events, as an identity acting with its own
//! authority rather than through any integrator grant. See
//! `crates/server/src/guilds.rs`/`channels.rs`/`guild_messages.rs`/
//! `guild_events.rs`.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::SdkError;

use super::{AccountSession, SignatureFields};

/// A guild, as returned by every guild-admin endpoint that hands back the
/// full record.
#[derive(Debug, Clone, Deserialize)]
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
    #[serde(with = "time::serde::rfc3339")]
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
    #[serde(default)]
    pub links: Vec<avalon_protocol::guilds::GuildLink>,
    /// Whether the guild is currently recruiting.
    #[serde(default)]
    pub recruiting: bool,
    /// Whether the guild is publicly browsable/discoverable.
    #[serde(default)]
    pub public: bool,
}

/// One guild in a discovery-board listing (`GET /guilds/discover`, issue
/// #154) — a narrower public summary than [`Guild`], not the full record.
#[derive(Debug, Clone, Deserialize)]
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
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Banner image URL, if set.
    pub banner: Option<String>,
    /// Icon image URL, if set.
    pub icon: Option<String>,
}

/// One page of a discovery-board listing.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscoverGuildsPage {
    /// This page's own results.
    pub guilds: Vec<DiscoverGuildSummary>,
    /// `Some(id)` when another page exists — pass it back as `cursor=` in
    /// the next call's own query string.
    pub next_cursor: Option<Uuid>,
}

/// One curated favorite-integrator pin (issue #207, decision #160).
#[derive(Debug, Clone, Deserialize)]
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

/// A guild's curated favorite-integrators list.
#[derive(Debug, Clone, Deserialize)]
pub struct FavoriteGames {
    /// The guild this list belongs to.
    pub guild_id: Uuid,
    /// The curated entries, in display order.
    pub favorites: Vec<FavoriteGameEntry>,
}

/// A guild role definition.
#[derive(Debug, Clone, Deserialize)]
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

/// A per-resource permission override.
#[derive(Debug, Clone, Deserialize)]
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

/// One member of a guild's roster.
#[derive(Debug, Clone, Deserialize)]
pub struct GuildMember {
    /// The guild.
    pub guild_id: Uuid,
    /// The member.
    pub identity_id: Uuid,
    /// The member's current role index.
    pub role_index: i32,
    /// When they joined.
    #[serde(with = "time::serde::rfc3339")]
    pub joined_at: OffsetDateTime,
}

/// The caller's own membership summary (`GET /me/guilds`).
#[derive(Debug, Clone, Deserialize)]
pub struct MyGuildMembership {
    /// The guild.
    pub guild_id: Uuid,
    /// The caller's own role index in it.
    pub role_index: i32,
    /// When the caller joined.
    #[serde(with = "time::serde::rfc3339")]
    pub joined_at: OffsetDateTime,
}

/// A sent (or received) guild invite.
#[derive(Debug, Clone, Deserialize)]
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
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// One of the caller's own pending invites, across every guild (`GET
/// /me/guild-invites`, issue #442).
#[derive(Debug, Clone, Deserialize)]
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
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// An applicant-initiated join request (issue #242).
#[derive(Debug, Clone, Deserialize)]
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
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When it was decided, if it has been.
    #[serde(with = "time::serde::rfc3339::option")]
    pub decided_at: Option<OffsetDateTime>,
    /// Who decided it, if it has been.
    pub decided_by: Option<Uuid>,
}

/// A guild text channel.
#[derive(Debug, Clone, Deserialize)]
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
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Whether posting requires the `channel_post` permission via override
    /// (issue #250) rather than any current member being able to post.
    pub announcement_only: bool,
    /// Short description shown in the channel header, if set.
    pub topic: Option<String>,
    /// Non-member visibility baseline for a public guild (issue #458).
    #[serde(default)]
    pub public: bool,
}

/// A channel message.
#[derive(Debug, Clone, Deserialize)]
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
    #[serde(with = "time::serde::rfc3339")]
    pub sent_at: OffsetDateTime,
}

/// RSVP tallies embedded in a [`GuildEvent`].
#[derive(Debug, Clone, Deserialize)]
pub struct RsvpCounts {
    /// Count of `"going"` responses.
    pub going: i64,
    /// Count of `"maybe"` responses.
    pub maybe: i64,
    /// Count of `"not_going"` responses.
    pub not_going: i64,
}

/// A scheduled guild event.
#[derive(Debug, Clone, Deserialize)]
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
    #[serde(with = "time::serde::rfc3339")]
    pub starts_at: OffsetDateTime,
    /// Scheduled end time, if announced.
    #[serde(with = "time::serde::rfc3339::option")]
    pub ends_at: Option<OffsetDateTime>,
    /// Who created it.
    pub created_by: Uuid,
    /// When it was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// RSVP tallies.
    pub rsvp_counts: RsvpCounts,
    /// Whether a non-member of a public guild may see this event (issue
    /// #448).
    #[serde(default)]
    pub public: bool,
}

/// The caller's own RSVP, as set by
/// [`AccountSession::rsvp_to_event`].
#[derive(Debug, Clone, Deserialize)]
pub struct Rsvp {
    /// The event.
    pub event_id: Uuid,
    /// The responder (always the caller).
    pub identity_id: Uuid,
    /// `"going"`, `"maybe"`, or `"not_going"`.
    pub status: String,
    /// When this RSVP was last set.
    #[serde(with = "time::serde::rfc3339")]
    pub responded_at: OffsetDateTime,
}

/// One entry of an event's per-member RSVP roster (`GET
/// /guilds/{id}/events/{eid}/rsvps`, issue #248) — unlike [`Rsvp`], not
/// scoped to the caller's own response and doesn't carry `event_id` (the
/// server's own response shape doesn't repeat it per row).
#[derive(Debug, Clone, Deserialize)]
pub struct RsvpRosterEntry {
    /// The responder.
    pub identity_id: Uuid,
    /// `"going"`, `"maybe"`, or `"not_going"`.
    pub status: String,
    /// When this RSVP was last set.
    #[serde(with = "time::serde::rfc3339")]
    pub responded_at: OffsetDateTime,
}

#[derive(Serialize, Default)]
struct CreateGuildRequest<'a> {
    name: &'a str,
    tag: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    description: &'a str,
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

#[derive(Serialize)]
struct UpdateGuildRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tag: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    motd: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    banner: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recruiting: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    public: Option<bool>,
}

#[derive(Serialize, Default)]
struct CreateRoleRequest<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    permissions: Vec<&'a str>,
    #[serde(skip_serializing_if = "str::is_empty")]
    description: &'a str,
    #[serde(flatten)]
    signature: SignatureFields,
}

#[derive(Serialize)]
struct UpdateRoleRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    permissions: Option<Vec<&'a str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(flatten)]
    signature: SignatureFields,
}

#[derive(Serialize, Default)]
struct SignatureOnlyRequest {
    #[serde(flatten)]
    signature: SignatureFields,
}

#[derive(Serialize)]
struct SetPermissionOverrideRequest<'a> {
    role_index: i32,
    resource_kind: &'a str,
    resource_id: Uuid,
    permission: &'a str,
    allow: bool,
    #[serde(flatten)]
    signature: SignatureFields,
}

#[derive(Serialize)]
struct TransferOwnershipRequest {
    to: Uuid,
    #[serde(flatten)]
    signature: SignatureFields,
}

#[derive(Serialize)]
struct UpdateGuildMemberRequest {
    role_index: i32,
    #[serde(flatten)]
    signature: SignatureFields,
}

#[derive(Serialize)]
struct CreateGuildInviteRequest {
    to: Uuid,
}

#[derive(Serialize, Default)]
struct CreateJoinRequestRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'a str>,
}

#[derive(Serialize)]
struct CreateChannelRequest<'a> {
    name: &'a str,
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

#[derive(Serialize)]
struct UpdateChannelRequest<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    announcement_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    topic: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    public: Option<bool>,
}

#[derive(Serialize)]
struct SendMessageRequest<'a> {
    body: &'a str,
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

#[derive(Serialize)]
struct EventRequestBody<'a> {
    channel_id: Option<Uuid>,
    title: &'a str,
    description: Option<&'a str>,
    #[serde(with = "time::serde::rfc3339")]
    starts_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    ends_at: Option<OffsetDateTime>,
    public: bool,
}

#[derive(Serialize)]
struct RsvpRequest<'a> {
    status: &'a str,
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
        self.post(
            "/guilds",
            &CreateGuildRequest {
                name,
                tag,
                description,
            },
        )
        .await
    }

    /// `GET /guilds/{id}`.
    pub async fn get_guild(&self, guild_id: Uuid) -> Result<Guild, SdkError> {
        self.get(&format!("/guilds/{guild_id}")).await
    }

    /// `GET /guilds/discover{query_string}` — `query_string` is passed
    /// through as-is (including its leading `?`), built by the caller;
    /// this crate doesn't replicate the Hub's own query-builder.
    pub async fn discover_guilds(
        &self,
        query_string: &str,
    ) -> Result<DiscoverGuildsPage, SdkError> {
        self.get(&format!("/guilds/discover{query_string}")).await
    }

    /// `PATCH /guilds/{id}` — ordinary `manage_guild`-gated metadata
    /// edits. Not signature-required.
    pub async fn update_guild(
        &self,
        guild_id: Uuid,
        update: GuildUpdate<'_>,
    ) -> Result<Guild, SdkError> {
        self.patch(
            &format!("/guilds/{guild_id}"),
            &UpdateGuildRequest {
                name: update.name,
                tag: update.tag,
                description: update.description,
                motd: update.motd,
                banner: update.banner,
                icon: update.icon,
                recruiting: update.recruiting,
                public: update.public,
            },
        )
        .await
    }

    /// `GET /guilds/{id}/roles`.
    pub async fn list_roles(&self, guild_id: Uuid) -> Result<Vec<Role>, SdkError> {
        self.get(&format!("/guilds/{guild_id}/roles")).await
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
        self.post(
            &format!("/guilds/{guild_id}/roles"),
            &CreateRoleRequest {
                name,
                permissions: permissions.to_vec(),
                description,
                signature,
            },
        )
        .await
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
        self.patch(
            &format!("/guilds/{guild_id}/roles/{name_index}"),
            &UpdateRoleRequest {
                name,
                permissions: permissions.map(|p| p.to_vec()),
                description,
                signature,
            },
        )
        .await
    }

    /// `DELETE /guilds/{id}/roles/{name_index}` — always signs
    /// (`guild.role.delete`, `[guild_id, name_index]`).
    pub async fn delete_role(&self, guild_id: Uuid, name_index: i32) -> Result<(), SdkError> {
        let signature = self.sign(
            "guild.role.delete",
            &[&guild_id.to_string(), &name_index.to_string()],
        );
        self.delete_with_body(
            &format!("/guilds/{guild_id}/roles/{name_index}"),
            &SignatureOnlyRequest { signature },
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
        self.get_query(
            &format!("/guilds/{guild_id}/permission-overrides"),
            &[
                ("resource_kind", resource_kind),
                ("resource_id", &resource_id_str),
            ],
        )
        .await
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
        self.put(
            &format!("/guilds/{guild_id}/permission-overrides"),
            &SetPermissionOverrideRequest {
                role_index,
                resource_kind,
                resource_id,
                permission,
                allow,
                signature,
            },
        )
        .await
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
            &format!("/guilds/{guild_id}/permission-overrides/{override_id}"),
            &SignatureOnlyRequest { signature },
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
        self.post(
            &format!("/guilds/{guild_id}/transfer-ownership"),
            &TransferOwnershipRequest { to, signature },
        )
        .await
    }

    /// `POST /guilds/{id}/integrations/{integrator_id}` — associates an
    /// integrator with a guild. Not signature-required.
    pub async fn associate_integrator(
        &self,
        guild_id: Uuid,
        integrator_id: Uuid,
    ) -> Result<Guild, SdkError> {
        self.post_empty(&format!("/guilds/{guild_id}/integrations/{integrator_id}"))
            .await
    }

    /// `GET /guilds/{id}/members`.
    pub async fn list_members(&self, guild_id: Uuid) -> Result<Vec<GuildMember>, SdkError> {
        self.get(&format!("/guilds/{guild_id}/members")).await
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
        self.patch(
            &format!("/guilds/{guild_id}/members/{identity_id}"),
            &UpdateGuildMemberRequest {
                role_index,
                signature,
            },
        )
        .await
    }

    /// `DELETE /guilds/{id}/members/{identity_id}` — kick, not a role
    /// change; reversible via re-invite. Not signature-required.
    pub async fn remove_member(&self, guild_id: Uuid, identity_id: Uuid) -> Result<(), SdkError> {
        self.delete(&format!("/guilds/{guild_id}/members/{identity_id}"))
            .await
    }

    /// `GET /me/guilds`.
    pub async fn my_guilds(&self) -> Result<Vec<MyGuildMembership>, SdkError> {
        self.get("/me/guilds").await
    }

    /// `GET /me/guild-invites` (issue #442).
    pub async fn my_guild_invites(&self) -> Result<Vec<MyGuildInvite>, SdkError> {
        self.get("/me/guild-invites").await
    }

    /// `POST /guilds/{id}/invites`.
    pub async fn create_guild_invite(
        &self,
        guild_id: Uuid,
        to: Uuid,
    ) -> Result<GuildInvite, SdkError> {
        self.post(
            &format!("/guilds/{guild_id}/invites"),
            &CreateGuildInviteRequest { to },
        )
        .await
    }

    /// `POST /guilds/{id}/invites/{invite_id}/accept`.
    pub async fn accept_guild_invite(
        &self,
        guild_id: Uuid,
        invite_id: Uuid,
    ) -> Result<GuildMember, SdkError> {
        self.post_empty(&format!("/guilds/{guild_id}/invites/{invite_id}/accept"))
            .await
    }

    /// `POST /guilds/{id}/invites/{invite_id}/decline`.
    pub async fn decline_guild_invite(
        &self,
        guild_id: Uuid,
        invite_id: Uuid,
    ) -> Result<(), SdkError> {
        self.post_empty_no_response(&format!("/guilds/{guild_id}/invites/{invite_id}/decline"))
            .await
    }

    /// `POST /guilds/{id}/join` — only meaningful when the guild's join
    /// policy allows it (see `avalon_protocol::guilds::JoinPolicy`).
    pub async fn join_guild(&self, guild_id: Uuid) -> Result<GuildMember, SdkError> {
        self.post_empty(&format!("/guilds/{guild_id}/join")).await
    }

    /// `POST /guilds/{id}/leave`.
    pub async fn leave_guild(&self, guild_id: Uuid) -> Result<(), SdkError> {
        self.post_empty_no_response(&format!("/guilds/{guild_id}/leave"))
            .await
    }

    /// `POST /guilds/{id}/join-requests` (issue #242).
    pub async fn create_join_request(
        &self,
        guild_id: Uuid,
        message: Option<&str>,
    ) -> Result<GuildJoinRequest, SdkError> {
        self.post(
            &format!("/guilds/{guild_id}/join-requests"),
            &CreateJoinRequestRequest { message },
        )
        .await
    }

    /// `GET /guilds/{id}/join-requests` — `manage_members`-gated.
    pub async fn list_join_requests(
        &self,
        guild_id: Uuid,
    ) -> Result<Vec<GuildJoinRequest>, SdkError> {
        self.get(&format!("/guilds/{guild_id}/join-requests")).await
    }

    /// `GET /guilds/{id}/join-requests/mine` — the caller's own pending
    /// request for this guild, or `None` (issue #256).
    pub async fn my_join_request(
        &self,
        guild_id: Uuid,
    ) -> Result<Option<GuildJoinRequest>, SdkError> {
        self.get(&format!("/guilds/{guild_id}/join-requests/mine"))
            .await
    }

    /// `POST /guilds/{id}/join-requests/{request_id}/approve`.
    pub async fn approve_join_request(
        &self,
        guild_id: Uuid,
        request_id: Uuid,
    ) -> Result<GuildMember, SdkError> {
        self.post_empty(&format!(
            "/guilds/{guild_id}/join-requests/{request_id}/approve"
        ))
        .await
    }

    /// `POST /guilds/{id}/join-requests/{request_id}/reject`.
    pub async fn reject_join_request(
        &self,
        guild_id: Uuid,
        request_id: Uuid,
    ) -> Result<(), SdkError> {
        self.post_empty_no_response(&format!(
            "/guilds/{guild_id}/join-requests/{request_id}/reject"
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
        self.delete(&format!("/guilds/{guild_id}/join-requests/{request_id}"))
            .await
    }

    /// `GET /guilds/{id}/favorite-integrators` (issue #207).
    pub async fn favorite_games(&self, guild_id: Uuid) -> Result<FavoriteGames, SdkError> {
        self.get(&format!("/guilds/{guild_id}/favorite-integrators"))
            .await
    }

    /// `PUT /guilds/{id}/favorite-integrators` — `manage_guild`-gated, full
    /// ordered replacement. Not signature-required.
    pub async fn set_favorite_games(
        &self,
        guild_id: Uuid,
        integrator_ids: &[Uuid],
    ) -> Result<FavoriteGames, SdkError> {
        #[derive(Serialize)]
        struct SetFavoriteGamesRequest<'a> {
            integrator_ids: &'a [Uuid],
        }
        self.put(
            &format!("/guilds/{guild_id}/favorite-integrators"),
            &SetFavoriteGamesRequest { integrator_ids },
        )
        .await
    }

    /// `GET /guilds/{id}/channels`.
    pub async fn list_channels(&self, guild_id: Uuid) -> Result<Vec<GuildChannel>, SdkError> {
        self.get(&format!("/guilds/{guild_id}/channels")).await
    }

    /// `POST /guilds/{id}/channels` — `manage_channels`-gated. Not
    /// signature-required (structural but reversible).
    pub async fn create_channel(
        &self,
        guild_id: Uuid,
        name: &str,
    ) -> Result<GuildChannel, SdkError> {
        self.post(
            &format!("/guilds/{guild_id}/channels"),
            &CreateChannelRequest { name },
        )
        .await
    }

    /// `PATCH /guilds/{id}/channels/{channel_id}`. Not signature-required.
    pub async fn update_channel(
        &self,
        guild_id: Uuid,
        channel_id: Uuid,
        update: ChannelUpdate<'_>,
    ) -> Result<GuildChannel, SdkError> {
        self.patch(
            &format!("/guilds/{guild_id}/channels/{channel_id}"),
            &UpdateChannelRequest {
                name: update.name,
                announcement_only: update.announcement_only,
                topic: update.topic,
                public: update.public,
            },
        )
        .await
    }

    /// `POST /guilds/{id}/channels/{channel_id}/archive`.
    pub async fn archive_channel(
        &self,
        guild_id: Uuid,
        channel_id: Uuid,
    ) -> Result<GuildChannel, SdkError> {
        self.post_empty(&format!("/guilds/{guild_id}/channels/{channel_id}/archive"))
            .await
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
        self.get_query(
            &format!("/guilds/{guild_id}/channels/{channel_id}/messages"),
            &query_refs,
        )
        .await
    }

    /// `POST /guilds/{id}/channels/{channel_id}/messages`. Not
    /// signature-required (chat, per #697's own invariants).
    pub async fn send_message(
        &self,
        guild_id: Uuid,
        channel_id: Uuid,
        body: &str,
    ) -> Result<GuildMessage, SdkError> {
        self.post(
            &format!("/guilds/{guild_id}/channels/{channel_id}/messages"),
            &SendMessageRequest { body },
        )
        .await
    }

    /// `DELETE /guilds/{id}/channels/{channel_id}/messages/{message_id}` —
    /// moderation delete, `manage_channels`-gated. Not signature-required.
    pub async fn delete_message(
        &self,
        guild_id: Uuid,
        channel_id: Uuid,
        message_id: Uuid,
    ) -> Result<(), SdkError> {
        self.delete(&format!(
            "/guilds/{guild_id}/channels/{channel_id}/messages/{message_id}"
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
        self.get_query(&format!("/guilds/{guild_id}/events"), &query)
            .await
    }

    /// `POST /guilds/{id}/events`. Not signature-required (reversible
    /// scheduling state).
    pub async fn create_event(
        &self,
        guild_id: Uuid,
        fields: EventFields<'_>,
    ) -> Result<GuildEvent, SdkError> {
        self.post(
            &format!("/guilds/{guild_id}/events"),
            &EventRequestBody {
                channel_id: fields.channel_id,
                title: fields.title,
                description: fields.description,
                starts_at: fields.starts_at,
                ends_at: fields.ends_at,
                public: fields.public,
            },
        )
        .await
    }

    /// `PATCH /guilds/{id}/events/{event_id}` — full replacement, not
    /// partial (matches the server's own `UpdateEventRequest`).
    pub async fn update_event(
        &self,
        guild_id: Uuid,
        event_id: Uuid,
        fields: EventFields<'_>,
    ) -> Result<GuildEvent, SdkError> {
        self.patch(
            &format!("/guilds/{guild_id}/events/{event_id}"),
            &EventRequestBody {
                channel_id: fields.channel_id,
                title: fields.title,
                description: fields.description,
                starts_at: fields.starts_at,
                ends_at: fields.ends_at,
                public: fields.public,
            },
        )
        .await
    }

    /// `DELETE /guilds/{id}/events/{event_id}`.
    pub async fn delete_event(&self, guild_id: Uuid, event_id: Uuid) -> Result<(), SdkError> {
        self.delete(&format!("/guilds/{guild_id}/events/{event_id}"))
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
        self.put(
            &format!("/guilds/{guild_id}/events/{event_id}/rsvp"),
            &RsvpRequest { status },
        )
        .await
    }

    /// `GET /guilds/{id}/events/{event_id}/rsvps` — the per-member roster.
    pub async fn event_rsvps(
        &self,
        guild_id: Uuid,
        event_id: Uuid,
    ) -> Result<Vec<RsvpRosterEntry>, SdkError> {
        self.get(&format!("/guilds/{guild_id}/events/{event_id}/rsvps"))
            .await
    }
}
