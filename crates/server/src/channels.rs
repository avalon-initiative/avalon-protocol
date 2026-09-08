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
//! currently be a member of the guild. Issue #21 (guild membership) is
//! being built concurrently in a separate branch and is not yet on `main`
//! — there is no real `guild_members` table in this worktree. The
//! membership check below ([`is_guild_member`]) queries
//! `guild_members (guild_id, identity_id, role_index, joined_at)`, the
//! exact schema #21 is expected to produce (matching `GuildMember` in
//! `crates/protocol/src/guilds.rs`), without creating that table in this
//! module's own migration (`crates/server/db/migrations/0010_guild_channels`
//! only owns `guild_channels`/`guild_messages`). Since this repo uses
//! runtime-checked `sqlx::query` rather than `sqlx::query!`, this compiles
//! today and will only actually run once both issues are merged together.
//!
//! `manage_channels` gates create/rename/archive, via
//! `crate::guilds::has_guild_permission` — reused rather than duplicated,
//! made `pub(crate)` for exactly this. Its `actor_permissions` argument is
//! always empty until #21 lands real role assignment, same stub/rationale
//! `guilds::actor_role_permissions` documents; duplicated locally as
//! [`actor_permissions`] rather than widening that private function's
//! visibility, to avoid touching `guilds.rs` any further while #21 is
//! concurrently editing it.

use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::guilds::GuildPermission;
use avalon_protocol::ids::GlobalId;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::AppError;
use crate::guilds::has_guild_permission;
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

/// Every permission `actor` holds in `guild_id` today. Always empty — see
/// module doc comment. Exists as the seam #21 will populate, mirroring
/// `guilds::actor_role_permissions`.
pub(crate) async fn actor_permissions(
    _state: &AppState,
    _guild_id: Uuid,
    _actor: Uuid,
) -> Result<Vec<String>, AppError> {
    Ok(Vec::new())
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
        "SELECT id, guild_id, name, archived_at, created_at FROM guild_channels \
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
}

impl From<ChannelRow> for ChannelResponse {
    fn from(row: ChannelRow) -> Self {
        Self {
            id: row.id,
            guild_id: row.guild_id,
            name: row.name,
            archived: row.archived_at.is_some(),
            created_at: row.created_at,
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
        "SELECT id, guild_id, name, archived_at, created_at FROM guild_channels \
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
    }))
}

#[derive(Deserialize)]
pub struct UpdateChannelRequest {
    pub name: String,
}

/// `PATCH /guilds/{id}/channels/{cid}` — rename. Requires
/// `manage_channels`. Renaming an archived channel is allowed (it's still
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
    require_manage_channels(&state, guild_id, actor).await?;

    let new_name = body.name.trim().to_string();
    let mut tx = state.pool.begin().await?;

    sqlx::query("UPDATE guild_channels SET name = $3 WHERE id = $1 AND guild_id = $2")
        .bind(channel_id)
        .bind(guild_id)
        .bind(&new_name)
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
    }))
}

/// `POST /guilds/{id}/channels/{cid}/archive` — requires `manage_channels`.
/// A soft flag (`archived_at`), not a delete: history and past messages
/// stay reachable, the channel simply stops accepting new posts (enforced
/// in `crate::guild_messages::send_message`).
pub async fn archive_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, channel_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<ChannelResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let channel = fetch_channel(&state, guild_id, channel_id).await?;
    require_manage_channels(&state, guild_id, actor).await?;

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
