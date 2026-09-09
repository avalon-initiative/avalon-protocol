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

use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::guilds::{GuildPermission, GuildResourceKind};
use avalon_protocol::ids::GlobalId;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::AppError;
use crate::guilds::{
    actor_role_permissions as actor_permissions, has_guild_permission, has_resource_permission,
};
use crate::handlers::authenticate;
use crate::outbox;
use crate::state::AppState;

const CHANNEL_NAME_MAX_CHARS: usize = 100;

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

/// True if `identity_id` currently holds membership in `guild_id`. See
/// module doc comment — queries a table this worktree's own migrations
/// never create.
pub(crate) async fn is_guild_member(
    state: &AppState,
    guild_id: Uuid,
    identity_id: Uuid,
) -> Result<bool, AppError> {
    let row = sqlx::query("SELECT 1 FROM guild_members WHERE guild_id = $1 AND identity_id = $2")
        .bind(guild_id)
        .bind(identity_id)
        .fetch_optional(&state.pool)
        .await?;
    Ok(row.is_some())
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

pub(crate) struct ChannelRow {
    pub id: Uuid,
    pub guild_id: Uuid,
    pub name: String,
    pub archived_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub announcement_only: bool,
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
        "SELECT id, guild_id, name, archived_at, created_at, announcement_only FROM guild_channels \
         WHERE id = $1 AND guild_id = $2",
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
    })
}

#[derive(Serialize)]
pub struct ChannelResponse {
    pub id: Uuid,
    pub guild_id: Uuid,
    pub name: String,
    pub archived: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub announcement_only: bool,
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
        }
    }
}

/// `GET /guilds/{id}/channels` — current members only (see module doc
/// comment). Lists both active and archived channels; the client
/// distinguishes via `archived`.
pub async fn list_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
) -> Result<Json<Vec<ChannelResponse>>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    require_member(&state, guild_id, actor).await?;

    let rows = sqlx::query(
        "SELECT id, guild_id, name, archived_at, created_at, announcement_only FROM guild_channels \
         WHERE guild_id = $1 ORDER BY created_at",
    )
    .bind(guild_id)
    .fetch_all(&state.pool)
    .await?;

    let mut channels = Vec::with_capacity(rows.len());
    for row in rows {
        channels.push(ChannelResponse::from(ChannelRow {
            id: row.try_get("id")?,
            guild_id: row.try_get("guild_id")?,
            name: row.try_get("name")?,
            archived_at: row.try_get("archived_at")?,
            created_at: row.try_get("created_at")?,
            announcement_only: row.try_get("announcement_only")?,
        }));
    }
    Ok(Json(channels))
}

#[derive(Deserialize)]
pub struct CreateChannelRequest {
    pub name: String,
}

/// `POST /guilds/{id}/channels` — requires `manage_channels`.
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
        kind: "guild.channel_created".to_string(),
        issuer: identity_ref(actor, "guild_channel_created"),
        subject: channel_ref(channel_id, "guild_channel_created"),
        payload: serde_json::json!({
            "guild_id": guild_id,
            "channel_id": channel_id,
            "name": name,
            "actor": actor,
        }),
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
    }))
}

#[derive(Deserialize)]
pub struct UpdateChannelRequest {
    pub name: String,
    /// Issue #250. `None` leaves the existing value untouched, same
    /// partial-update convention `UpdateRoleRequest` uses.
    #[serde(default)]
    pub announcement_only: Option<bool>,
}

/// `PATCH /guilds/{id}/channels/{cid}` — rename and/or toggle
/// announcement-only. Requires `manage_channels` (resource-aware, issue
/// #250). Renaming/retoggling an archived channel is allowed (it's still
/// the same durable channel, just not accepting new posts).
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
    let mut tx = state.pool.begin().await?;

    sqlx::query(
        "UPDATE guild_channels SET name = $3, announcement_only = $4 WHERE id = $1 AND guild_id = $2",
    )
    .bind(channel_id)
    .bind(guild_id)
    .bind(&new_name)
    .bind(announcement_only)
    .execute(&mut *tx)
    .await?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "guild.channel_renamed".to_string(),
        issuer: identity_ref(actor, "guild_channel_renamed"),
        subject: channel_ref(channel_id, "guild_channel_renamed"),
        payload: serde_json::json!({
            "guild_id": guild_id,
            "channel_id": channel_id,
            "name": new_name,
            "announcement_only": announcement_only,
            "actor": actor,
        }),
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
    }))
}

/// `POST /guilds/{id}/channels/{cid}/archive` — requires `manage_channels`
/// (resource-aware, issue #250). A soft flag (`archived_at`), not a
/// delete: history and past messages stay reachable, the channel simply
/// stops accepting new posts (enforced in
/// `crate::guild_messages::send_message`).
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
        kind: "guild.channel_archived".to_string(),
        issuer: identity_ref(actor, "guild_channel_archived"),
        subject: channel_ref(channel_id, "guild_channel_archived"),
        payload: serde_json::json!({
            "guild_id": guild_id,
            "channel_id": channel_id,
            "actor": actor,
        }),
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
    fn channel_ref_namespaces_by_channel_and_verb() {
        let id = Uuid::new_v4();
        let global_id = channel_ref(id, "guild_channel_created");
        assert_eq!(
            global_id.as_str(),
            format!("guild_channel:{id}:self:guild_channel_created")
        );
    }
}
