//! Guild chat channels (issue #22).
//!
//! A `GuildChannel` (`crates/protocol/src/guilds.rs`) is guild structure,
//! not chat content — it's a rebuildable projection of durable history,
//! same pattern as `guilds`/`guild_roles` from issue #20
//! (`crates/server/src/guilds.rs`). Creating, renaming, and archiving a
//! channel all write a `guild.channel_*` event into the outbox in the same
//! transaction as the `guild_channels` row change; the actual chat
//! messages that flow through a channel do not (see
//! `crate::guild_messages`, the deliberately non-durable sibling of this
//! module).
//!
//! **Membership.** Reading or managing channels requires the caller to
//! currently be a member of the guild — [`is_guild_member`] queries the
//! real `guild_members` table #21 built.
//!
//! `manage_channels` gates create/rename/archive. Creation has no channel
//! yet to scope a check to, so it uses the flat guild-wide check
//! (`crate::guilds::has_guild_permission`, made `pub(crate)` for exactly
//! this, with `crate::guilds::actor_role_permissions` imported here as
//! `actor_permissions`). Rename/archive already have a concrete channel,
//! so they go through the resource-aware sibling
//! (`crate::guilds::has_resource_permission`, issue #250) instead — a role
//! can be granted or denied `manage_channels` on one specific channel via
//! a per-resource override, on top of (or instead of) holding it
//! guild-wide.
//!
//! **Announcement-only channels (issue #250).** `GuildChannel.announcement_only`
//! (`guild_channels.announcement_only`) is this ticket's end-to-end proof
//! point for the override layer: when set, `crate::guild_messages::send_message`
//! requires the `ChannelPost` permission — resolved per-channel through
//! the same override layer, not a guild-wide grant — instead of today's
//! "any current member may post." Toggled via `PATCH .../channels/{cid}`,
//! gated the same as a rename (`manage_channels`, resource-aware).

use avalon_protocol::event_payloads::{
    GuildChannelArchivedPayload, GuildChannelCreatedPayload, GuildChannelRenamedPayload,
};
use avalon_protocol::events::{ProtocolEvent, ProtocolEventKindVariant};
use avalon_protocol::guilds::{GuildPermission, GuildResourceKind};
use avalon_protocol::ids::GlobalId;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::AppError;
use crate::guilds::{
    actor_role_permissions as actor_permissions, has_guild_permission, has_resource_permission,
};
use crate::handlers::authenticate;
use crate::outbox;
use crate::state::AppState;

const CHANNEL_NAME_MAX_CHARS: usize = 100;

/// Cap on `GuildChannel.topic` (issue #276) — short prose, same order of
/// magnitude as `MAX_ROLE_DESCRIPTION_LEN` in `crate::guilds` rather than
/// `Guild::motd`'s longer cap, since a channel topic is meant to be a
/// single line, not a paragraph.
const CHANNEL_TOPIC_MAX_CHARS: usize = 200;

fn identity_ref(identity_id: Uuid, verb: &str) -> GlobalId {
    GlobalId::new("identity", &identity_id.to_string(), "self", verb)
}

fn channel_ref(channel_id: Uuid, verb: &str) -> GlobalId {
    GlobalId::new("guild_channel", &channel_id.to_string(), "self", verb)
}

pub(crate) async fn guild_owner(state: &AppState, guild_id: Uuid) -> Result<Uuid, AppError> {
    let row = sqlx::query("SELECT owner FROM guilds WHERE id = $1")
        .bind(guild_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::GuildNotFound)?;
    Ok(row.try_get("owner")?)
}

/// True if `identity_id` currently holds membership in `guild_id`.
pub(crate) async fn is_guild_member(
    state: &AppState,
    guild_id: Uuid,
    identity_id: Uuid,
) -> Result<bool, AppError> {
    Ok(
        avalon_indexer::projections::guild_rosters::is_member(&state.pool, guild_id, identity_id)
            .await?,
    )
}

pub(crate) async fn require_member(
    state: &AppState,
    guild_id: Uuid,
    actor: Uuid,
) -> Result<(), AppError> {
    if is_guild_member(state, guild_id, actor).await? {
        Ok(())
    } else {
        Err(AppError::NotGuildMember)
    }
}

/// True if `identity_id` currently holds membership in whichever guild owns
/// `channel_id` — issue #610's `crate::interest::lookup_claimed` needs
/// exactly this from just a channel id (an [`crate::interest_claim`] claim
/// carries no `guild_id` of its own), without the 404-on-mismatch behavior
/// [`fetch_channel`] exists for. `false`, not an error, for a since-deleted
/// channel — a claim naming one simply never re-authorizes, same as a
/// revoked membership.
pub(crate) async fn is_member_of_channel(
    state: &AppState,
    channel_id: Uuid,
    identity_id: Uuid,
) -> Result<bool, AppError> {
    let guild_id: Option<Uuid> =
        sqlx::query_scalar("SELECT guild_id FROM guild_channels WHERE id = $1")
            .bind(channel_id)
            .fetch_optional(&state.pool)
            .await?;
    let Some(guild_id) = guild_id else {
        return Ok(false);
    };
    is_guild_member(state, guild_id, identity_id).await
}

async fn require_manage_channels(
    state: &AppState,
    guild_id: Uuid,
    actor: Uuid,
) -> Result<(), AppError> {
    let owner = guild_owner(state, guild_id).await?;
    let permissions = actor_permissions(state, guild_id, actor).await?;
    if has_guild_permission(owner, actor, &permissions, GuildPermission::ManageChannels) {
        Ok(())
    } else {
        Err(AppError::MissingGuildPermission)
    }
}

/// Resource-aware `manage_channels` check against one specific channel
/// (issue #250) — used once a channel already exists to scope a
/// per-resource override to (rename, archive, announcement-only toggle).
pub(crate) async fn require_manage_channel_resource(
    state: &AppState,
    guild_id: Uuid,
    channel_id: Uuid,
    actor: Uuid,
) -> Result<(), AppError> {
    let owner = guild_owner(state, guild_id).await?;
    let allowed = has_resource_permission(
        state,
        guild_id,
        owner,
        actor,
        GuildResourceKind::Channel,
        channel_id,
        GuildPermission::ManageChannels,
    )
    .await?;
    if allowed {
        Ok(())
    } else {
        Err(AppError::MissingGuildPermission)
    }
}

fn validate_channel_name(name: &str) -> Result<(), AppError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > CHANNEL_NAME_MAX_CHARS {
        return Err(AppError::InvalidChannelName);
    }
    Ok(())
}

/// Same "empty means clear, over-cap is an error" convention
/// `crate::guilds::validate_guild_motd` uses for `Guild::motd`.
fn validate_channel_topic(topic: &str) -> Result<Option<String>, AppError> {
    let trimmed = topic.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > CHANNEL_TOPIC_MAX_CHARS {
        return Err(AppError::InvalidChannelTopic);
    }
    Ok(Some(trimmed.to_string()))
}

pub(crate) struct ChannelRow {
    pub id: Uuid,
    pub guild_id: Uuid,
    pub name: String,
    pub archived_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub announcement_only: bool,
    pub topic: Option<String>,
    pub public: bool,
}

/// Fetches a channel, 404ing if it doesn't exist or doesn't belong to
/// `guild_id` — the latter matters so `/guilds/{a}/channels/{cid}` can
/// never act on a channel that actually belongs to guild `{b}`.
pub(crate) async fn fetch_channel(
    state: &AppState,
    guild_id: Uuid,
    channel_id: Uuid,
) -> Result<ChannelRow, AppError> {
    let row = sqlx::query(
        "SELECT id, guild_id, name, archived_at, created_at, announcement_only, topic, \"public\" \
         FROM guild_channels WHERE id = $1 AND guild_id = $2",
    )
    .bind(channel_id)
    .bind(guild_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::ChannelNotFound)?;
    Ok(ChannelRow {
        id: row.try_get("id")?,
        guild_id: row.try_get("guild_id")?,
        name: row.try_get("name")?,
        archived_at: row.try_get("archived_at")?,
        created_at: row.try_get("created_at")?,
        announcement_only: row.try_get("announcement_only")?,
        topic: row.try_get("topic")?,
        public: row.try_get("public")?,
    })
}

#[derive(Serialize, ToSchema)]
pub struct ChannelResponse {
    pub id: Uuid,
    pub guild_id: Uuid,
    pub name: String,
    pub archived: bool,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub created_at: OffsetDateTime,
    pub announcement_only: bool,
    pub topic: Option<String>,
    /// Issue #458. Non-member visibility baseline for this channel —
    /// same meaning as `guild_events.public` (#448), just newly added
    /// for channels, which had no non-member visibility concept before
    /// this ticket at all.
    pub public: bool,
}

impl From<ChannelRow> for ChannelResponse {
    fn from(row: ChannelRow) -> Self {
        Self {
            id: row.id,
            guild_id: row.guild_id,
            name: row.name,
            archived: row.archived_at.is_some(),
            created_at: row.created_at,
            announcement_only: row.announcement_only,
            topic: row.topic,
            public: row.public,
        }
    }
}

/// `GET /guilds/{id}/channels` — a member sees every channel they hold
/// `view` on (baseline: all of them, unless a role override says
/// otherwise — issue #458). A non-member of a
/// [`crate::guilds::GuildResponse::public`] guild sees only `public`
/// channels instead of being 403'd outright — same shape
/// `guild_events::list_events` already established for events (#448),
/// extended to channels here since they had no non-member visibility
/// concept before this ticket. A non-member of a non-public guild is
/// still 403'd, unchanged. Lists both active and archived channels; the
/// client distinguishes via `archived`.
#[utoipa::path(
    get,
    path = "/guilds/{id}/channels",
    tag = "guilds",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = Vec<ChannelResponse>)),
)]
pub async fn list_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
) -> Result<Json<Vec<ChannelResponse>>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let is_member = is_guild_member(&state, guild_id, actor).await?;
    if !is_member && !crate::guilds::is_guild_public(&state, guild_id).await? {
        return Err(AppError::NotGuildMember);
    }
    let owner = guild_owner(&state, guild_id).await?;

    let rows = sqlx::query(
        "SELECT id, guild_id, name, archived_at, created_at, announcement_only, topic, \"public\" \
         FROM guild_channels WHERE guild_id = $1 ORDER BY created_at",
    )
    .bind(guild_id)
    .fetch_all(&state.pool)
    .await?;

    let mut channels = Vec::with_capacity(rows.len());
    for row in rows {
        let channel = ChannelRow {
            id: row.try_get("id")?,
            guild_id: row.try_get("guild_id")?,
            name: row.try_get("name")?,
            archived_at: row.try_get("archived_at")?,
            created_at: row.try_get("created_at")?,
            announcement_only: row.try_get("announcement_only")?,
            topic: row.try_get("topic")?,
            public: row.try_get("public")?,
        };
        let can_view = crate::guilds::has_view_permission(
            &state,
            guild_id,
            owner,
            actor,
            avalon_protocol::guilds::GuildResourceKind::Channel,
            channel.id,
            channel.public,
            false,
        )
        .await?;
        if can_view {
            channels.push(ChannelResponse::from(channel));
        }
    }
    Ok(Json(channels))
}

#[derive(Deserialize, ToSchema)]
pub struct CreateChannelRequest {
    pub name: String,
}

/// `POST /guilds/{id}/channels` — requires `manage_channels`.
#[utoipa::path(
    post,
    path = "/guilds/{id}/channels",
    tag = "guilds",
    params(("id" = Uuid, Path)),
    request_body = CreateChannelRequest,
    responses((status = 200, body = ChannelResponse)),
)]
pub async fn create_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
    Json(body): Json<CreateChannelRequest>,
) -> Result<Json<ChannelResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    validate_channel_name(&body.name)?;
    require_manage_channels(&state, guild_id, actor).await?;

    let name = body.name.trim().to_string();
    let mut tx = state.pool.begin().await?;

    let channel_id = Uuid::new_v4();
    let created_at = OffsetDateTime::now_utc();
    sqlx::query(
        "INSERT INTO guild_channels (id, guild_id, name, created_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(channel_id)
    .bind(guild_id)
    .bind(&name)
    .bind(created_at)
    .execute(&mut *tx)
    .await?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::GuildChannelCreated
            .as_str()
            .to_string(),
        issuer: identity_ref(actor, "guild_channel_created"),
        subject: channel_ref(channel_id, "guild_channel_created"),
        payload: serde_json::to_value(GuildChannelCreatedPayload {
            guild_id,
            channel_id,
            name: name.clone(),
            actor,
        })
        .expect("GuildChannelCreatedPayload should serialize"),
        timestamp: created_at,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(ChannelResponse {
        id: channel_id,
        guild_id,
        name,
        archived: false,
        created_at,
        announcement_only: false,
        topic: None,
        public: false,
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateChannelRequest {
    pub name: String,
    /// Issue #250. `None` leaves the existing value untouched, same
    /// partial-update convention `UpdateRoleRequest` uses.
    #[serde(default)]
    pub announcement_only: Option<bool>,
    /// Issue #276. `None` leaves the existing value untouched; `Some("")`
    /// (after trimming) clears it — same three-state convention
    /// `crate::guilds::UpdateGuildRequest::motd` already uses.
    #[serde(default)]
    pub topic: Option<String>,
    /// Issue #458. `None` leaves the existing value untouched, same
    /// convention as `announcement_only` above.
    #[serde(default)]
    pub public: Option<bool>,
}

/// `PATCH /guilds/{id}/channels/{cid}` — rename, retopic, and/or toggle
/// announcement-only/public. Requires `manage_channels` (resource-aware,
/// issue #250). Renaming/retoggling an archived channel is allowed (it's
/// still the same durable channel, just not accepting new posts).
#[utoipa::path(
    patch,
    path = "/guilds/{id}/channels/{cid}",
    tag = "guilds",
    params(("id" = Uuid, Path), ("cid" = Uuid, Path)),
    request_body = UpdateChannelRequest,
    responses((status = 200, body = ChannelResponse)),
)]
pub async fn update_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, channel_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateChannelRequest>,
) -> Result<Json<ChannelResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    validate_channel_name(&body.name)?;
    let channel = fetch_channel(&state, guild_id, channel_id).await?;
    require_manage_channel_resource(&state, guild_id, channel_id, actor).await?;

    let new_name = body.name.trim().to_string();
    let announcement_only = body.announcement_only.unwrap_or(channel.announcement_only);
    let new_topic = match &body.topic {
        Some(raw) => validate_channel_topic(raw)?,
        None => channel.topic.clone(),
    };
    let public = body.public.unwrap_or(channel.public);
    let mut tx = state.pool.begin().await?;

    sqlx::query(
        "UPDATE guild_channels SET name = $3, announcement_only = $4, topic = $5, \"public\" = $6 \
         WHERE id = $1 AND guild_id = $2",
    )
    .bind(channel_id)
    .bind(guild_id)
    .bind(&new_name)
    .bind(announcement_only)
    .bind(&new_topic)
    .bind(public)
    .execute(&mut *tx)
    .await?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::GuildChannelRenamed
            .as_str()
            .to_string(),
        issuer: identity_ref(actor, "guild_channel_renamed"),
        subject: channel_ref(channel_id, "guild_channel_renamed"),
        payload: serde_json::to_value(GuildChannelRenamedPayload {
            guild_id,
            channel_id,
            name: new_name.clone(),
            announcement_only,
            topic: new_topic.clone(),
            public,
            actor,
        })
        .expect("GuildChannelRenamedPayload should serialize"),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(ChannelResponse {
        id: channel_id,
        guild_id,
        name: new_name,
        archived: channel.archived_at.is_some(),
        created_at: channel.created_at,
        announcement_only,
        topic: new_topic,
        public,
    }))
}

/// `POST /guilds/{id}/channels/{cid}/archive` — requires `manage_channels`
/// (resource-aware, issue #250). A soft flag (`archived_at`), not a
/// delete: history and past messages stay reachable, the channel simply
/// stops accepting new posts (enforced in
/// `crate::guild_messages::send_message`).
#[utoipa::path(
    post,
    path = "/guilds/{id}/channels/{cid}/archive",
    tag = "guilds",
    params(("id" = Uuid, Path), ("cid" = Uuid, Path)),
    responses((status = 200, body = ChannelResponse)),
)]
pub async fn archive_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, channel_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<ChannelResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let channel = fetch_channel(&state, guild_id, channel_id).await?;
    require_manage_channel_resource(&state, guild_id, channel_id, actor).await?;

    let archived_at = OffsetDateTime::now_utc();
    let mut tx = state.pool.begin().await?;

    sqlx::query("UPDATE guild_channels SET archived_at = $3 WHERE id = $1 AND guild_id = $2")
        .bind(channel_id)
        .bind(guild_id)
        .bind(archived_at)
        .execute(&mut *tx)
        .await?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::GuildChannelArchived
            .as_str()
            .to_string(),
        issuer: identity_ref(actor, "guild_channel_archived"),
        subject: channel_ref(channel_id, "guild_channel_archived"),
        payload: serde_json::to_value(GuildChannelArchivedPayload {
            guild_id,
            channel_id,
            actor,
        })
        .expect("GuildChannelArchivedPayload should serialize"),
        timestamp: archived_at,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(ChannelResponse {
        id: channel_id,
        guild_id,
        name: channel.name,
        archived: true,
        created_at: channel.created_at,
        announcement_only: channel.announcement_only,
        topic: channel.topic,
        public: channel.public,
    }))
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — pure-logic checks only. The
    //! endpoint-level flows are covered by `crates/server/tests/guild_channels.rs`,
    //! gated `--ignored`.

    use super::*;

    #[test]
    fn validate_channel_name_rejects_empty_and_overlong() {
        assert!(validate_channel_name("general").is_ok());
        assert!(validate_channel_name("   ").is_err());
        assert!(validate_channel_name(&"a".repeat(CHANNEL_NAME_MAX_CHARS + 1)).is_err());
        assert!(validate_channel_name(&"a".repeat(CHANNEL_NAME_MAX_CHARS)).is_ok());
    }

    #[test]
    fn validate_channel_topic_normalizes_blank_to_none_and_rejects_overlong() {
        assert_eq!(validate_channel_topic("").unwrap(), None);
        assert_eq!(validate_channel_topic("   ").unwrap(), None);
        assert_eq!(
            validate_channel_topic("  patch notes & raid planning  ").unwrap(),
            Some("patch notes & raid planning".to_string())
        );
        assert!(validate_channel_topic(&"a".repeat(CHANNEL_TOPIC_MAX_CHARS + 1)).is_err());
        assert!(validate_channel_topic(&"a".repeat(CHANNEL_TOPIC_MAX_CHARS)).is_ok());
    }

    #[test]
    fn channel_ref_namespaces_by_channel_and_verb() {
        let id = Uuid::new_v4();
        let global_id = channel_ref(id, "guild_channel_created");
        assert_eq!(
            global_id.as_str(),
            format!("guild_channel:{id}:self:guild_channel_created")
        );
    }
}
