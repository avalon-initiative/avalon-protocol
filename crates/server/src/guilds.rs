//! Guild creation, roles, ownership transfer, and game association
//! (issue #20).
//!
//! Every mutation here requires the caller's own player session — same
//! "no game-credential auth path exists in this repo" reasoning
//! `crates/server/src/friends.rs`'s module doc comment already lays out,
//! so "a game cannot act on a guild's behalf" is satisfied simply by these
//! routes only ever accepting a session bearer token.
//!
//! A guild is a network-level primitive, not a game's (issue #74) — see
//! `docs/architecture/guilds.md`. `guild.created`, `guild.updated`,
//! `guild.role_defined`, and `guild.owner_transferred` are promised-durable
//! history, written into the outbox in the same transaction as the
//! `guilds`/`guild_roles` projection change, same pattern
//! `handlers::register_finish` and `friends.rs` already established. The
//! `guilds`/`guild_roles`/`guild_game_associations` tables are projections,
//! rebuildable from that history — nothing here treats them as canonical.
//!
//! **Membership lifecycle (issue #21).** `guild_members`/`guild_invites`
//! are projections, same durability posture as everything else in this
//! module: `guild.member_added`, `guild.member_removed`, and
//! `guild.role_changed` are the durable history, written into the outbox in
//! the same transaction as the row change. Invites, declines, and
//! withdrawals are deliberately NOT durable — resolving one is a plain
//! projection update, no event, same pattern `friends.rs` uses for
//! declined/withdrawn friend requests. [`actor_role_permissions`] now does
//! a real `guild_members` JOIN `guild_roles` lookup, so
//! [`has_guild_permission`] resolves real permissions for non-owner callers
//! too, not just the owner. `GET /guilds/{id}`'s `member_count` is a real
//! `COUNT(*)` over `guild_members`.
//!
//! Whether a guild is invite-only or open (`join_policy`, on `guilds` and
//! `avalon_protocol::guilds::Guild`) governs `POST /guilds/{id}/join`; it
//! is not itself exposed for editing by any route in this module.

use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::guilds::{GuildPermission, JoinPolicy};
use avalon_protocol::ids::GlobalId;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::AppError;
use crate::handlers::authenticate;
use crate::outbox;
use crate::state::AppState;

const OWNER_ROLE_INDEX: i32 = 0;
const OFFICER_ROLE_INDEX: i32 = 1;
const MEMBER_ROLE_INDEX: i32 = 2;

fn identity_ref(identity_id: Uuid, verb: &str) -> GlobalId {
    GlobalId::new("identity", &identity_id.to_string(), "self", verb)
}

fn guild_ref(guild_id: Uuid, verb: &str) -> GlobalId {
    GlobalId::new("guild", &guild_id.to_string(), "self", verb)
}

/// True if `actor` may exercise `permission` on a guild owned by
/// `guild_owner`. The owner always can, regardless of `actor_permissions` —
/// ownership is structural (the `guilds.owner` column), not a role grant,
/// so it can never be revoked by editing a role row. Anyone else needs
/// `permission` present in their own role's permission list, resolved by
/// [`actor_role_permissions`]. `pub(crate)` so `channels.rs`/`guild_messages.rs`
/// (#22) can reuse it for `manage_channels` checks.
pub(crate) fn has_guild_permission(
    guild_owner: Uuid,
    actor: Uuid,
    actor_permissions: &[String],
    permission: GuildPermission,
) -> bool {
    if actor == guild_owner {
        return true;
    }
    actor_permissions.iter().any(|p| p == permission.as_str())
}

fn starter_roles() -> [(i32, &'static str, &'static [&'static str]); 3] {
    [
        (OWNER_ROLE_INDEX, "owner", GuildPermission::ALL_STRS),
        (
            OFFICER_ROLE_INDEX,
            "officer",
            &["manage_members", "manage_channels"],
        ),
        (MEMBER_ROLE_INDEX, "member", &[]),
    ]
}

fn validate_tag(tag: &str) -> Result<(), AppError> {
    let len = tag.chars().count();
    if !(2..=5).contains(&len) {
        return Err(AppError::InvalidGuildTag);
    }
    Ok(())
}

struct GuildRow {
    id: Uuid,
    name: String,
    tag: String,
    description: String,
    owner: Uuid,
    created_at: OffsetDateTime,
    join_policy: JoinPolicy,
}

async fn fetch_guild(state: &AppState, guild_id: Uuid) -> Result<GuildRow, AppError> {
    let row = sqlx::query(
        "SELECT id, name, tag, description, owner, created_at, join_policy FROM guilds WHERE id = $1",
    )
    .bind(guild_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::GuildNotFound)?;
    let join_policy_raw: String = row.try_get("join_policy")?;
    Ok(GuildRow {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        tag: row.try_get("tag")?,
        description: row.try_get("description")?,
        owner: row.try_get("owner")?,
        created_at: row.try_get("created_at")?,
        // Falls back to the column's own DEFAULT if it's ever somehow
        // unparseable — never a hard failure on a read path.
        join_policy: JoinPolicy::parse(&join_policy_raw).unwrap_or(JoinPolicy::InviteOnly),
    })
}

#[derive(Serialize)]
pub struct GuildResponse {
    pub id: Uuid,
    pub name: String,
    pub tag: String,
    pub description: String,
    pub owner: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub member_count: i64,
    pub games: Vec<Uuid>,
    pub join_policy: String,
}

async fn guild_response(state: &AppState, guild: GuildRow) -> Result<GuildResponse, AppError> {
    let game_rows = sqlx::query("SELECT game_id FROM guild_game_associations WHERE guild_id = $1")
        .bind(guild.id)
        .fetch_all(&state.pool)
        .await?;
    let mut games = Vec::with_capacity(game_rows.len());
    for row in game_rows {
        games.push(row.try_get("game_id")?);
    }

    let member_count_row =
        sqlx::query("SELECT COUNT(*) AS count FROM guild_members WHERE guild_id = $1")
            .bind(guild.id)
            .fetch_one(&state.pool)
            .await?;
    let member_count: i64 = member_count_row.try_get("count")?;

    Ok(GuildResponse {
        id: guild.id,
        name: guild.name,
        tag: guild.tag,
        description: guild.description,
        owner: guild.owner,
        created_at: guild.created_at,
        member_count,
        games,
        join_policy: guild.join_policy.as_str().to_string(),
    })
}

#[derive(Deserialize)]
pub struct CreateGuildRequest {
    pub name: String,
    pub tag: String,
    #[serde(default)]
    pub description: String,
}

pub async fn create_guild(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateGuildRequest>,
) -> Result<Json<GuildResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    validate_tag(&body.tag)?;

    let mut tx = state.pool.begin().await?;

    let guild_id = Uuid::new_v4();
    let created_at = OffsetDateTime::now_utc();
    let inserted = sqlx::query(
        r#"
        INSERT INTO guilds (id, name, tag, description, owner, created_at)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(guild_id)
    .bind(&body.name)
    .bind(&body.tag)
    .bind(&body.description)
    .bind(actor)
    .bind(created_at)
    .execute(&mut *tx)
    .await;
    if let Err(sqlx::Error::Database(db_err)) = &inserted {
        if db_err.is_unique_violation() {
            let is_tag = db_err
                .constraint()
                .map(|c| c.contains("tag"))
                .unwrap_or(false);
            return Err(if is_tag {
                AppError::GuildTagTaken
            } else {
                AppError::GuildNameTaken
            });
        }
    }
    inserted?;

    for (name_index, name, permissions) in starter_roles() {
        sqlx::query(
            "INSERT INTO guild_roles (guild_id, name_index, name, permissions) VALUES ($1, $2, $3, $4)",
        )
        .bind(guild_id)
        .bind(name_index)
        .bind(name)
        .bind(permissions)
        .execute(&mut *tx)
        .await?;
    }

    // The owner gets a `guild_members` row too (role_index 0) — see issue
    // #21's design note: #20 couldn't do this because this table didn't
    // exist yet. Folded into `guild.created`'s existing event rather than
    // a separate `guild.member_added`; owner membership is implied by
    // guild creation itself, not a distinct durable fact.
    sqlx::query(
        "INSERT INTO guild_members (guild_id, identity_id, role_index, joined_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(guild_id)
    .bind(actor)
    .bind(OWNER_ROLE_INDEX)
    .bind(created_at)
    .execute(&mut *tx)
    .await?;

    // Issue #22: every guild gets a default `general` channel on creation.
    // Deliberately just the row insert here, no `guild.channel_created`
    // event — see `crates/server/src/channels.rs`'s module doc comment for
    // why this stays a minimal, localized addition to this transaction.
    sqlx::query("INSERT INTO guild_channels (id, guild_id, name) VALUES ($1, $2, $3)")
        .bind(Uuid::new_v4())
        .bind(guild_id)
        .bind("general")
        .execute(&mut *tx)
        .await?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "guild.created".to_string(),
        issuer: identity_ref(actor, "guild_created"),
        subject: guild_ref(guild_id, "guild_created"),
        payload: serde_json::json!({
            "guild_id": guild_id,
            "name": body.name,
            "tag": body.tag,
            "description": body.description,
            "owner": actor,
        }),
        timestamp: created_at,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(
        guild_response(
            &state,
            GuildRow {
                id: guild_id,
                name: body.name,
                tag: body.tag,
                description: body.description,
                owner: actor,
                created_at,
                join_policy: JoinPolicy::InviteOnly,
            },
        )
        .await?,
    ))
}

pub async fn get_guild(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
) -> Result<Json<GuildResponse>, AppError> {
    // Public metadata is readable by any authenticated identity (see
    // module doc comment / ticket "Read visibility") — the roster itself
    // is #21's concern, and isn't returned here.
    authenticate(&state, &headers).await?;
    let guild = fetch_guild(&state, guild_id).await?;
    Ok(Json(guild_response(&state, guild).await?))
}

#[derive(Deserialize)]
pub struct UpdateGuildRequest {
    pub name: Option<String>,
    pub tag: Option<String>,
    pub description: Option<String>,
}

pub async fn update_guild(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
    Json(body): Json<UpdateGuildRequest>,
) -> Result<Json<GuildResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let guild = fetch_guild(&state, guild_id).await?;

    let actor_permissions = actor_role_permissions(&state, guild_id, actor).await?;
    if !has_guild_permission(
        guild.owner,
        actor,
        &actor_permissions,
        GuildPermission::ManageGuild,
    ) {
        return Err(AppError::MissingGuildPermission);
    }

    if let Some(tag) = &body.tag {
        validate_tag(tag)?;
    }

    let new_name = body.name.clone().unwrap_or_else(|| guild.name.clone());
    let new_tag = body.tag.clone().unwrap_or_else(|| guild.tag.clone());
    let new_description = body
        .description
        .clone()
        .unwrap_or_else(|| guild.description.clone());

    let mut tx = state.pool.begin().await?;

    let updated =
        sqlx::query("UPDATE guilds SET name = $2, tag = $3, description = $4 WHERE id = $1")
            .bind(guild_id)
            .bind(&new_name)
            .bind(&new_tag)
            .bind(&new_description)
            .execute(&mut *tx)
            .await;
    if let Err(sqlx::Error::Database(db_err)) = &updated {
        if db_err.is_unique_violation() {
            let is_tag = db_err
                .constraint()
                .map(|c| c.contains("tag"))
                .unwrap_or(false);
            return Err(if is_tag {
                AppError::GuildTagTaken
            } else {
                AppError::GuildNameTaken
            });
        }
    }
    updated?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "guild.updated".to_string(),
        issuer: identity_ref(actor, "guild_updated"),
        subject: guild_ref(guild_id, "guild_updated"),
        payload: serde_json::json!({
            "guild_id": guild_id,
            "name": new_name,
            "tag": new_tag,
            "description": new_description,
            "actor": actor,
        }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(
        guild_response(
            &state,
            GuildRow {
                id: guild_id,
                name: new_name,
                tag: new_tag,
                description: new_description,
                owner: guild.owner,
                created_at: guild.created_at,
                join_policy: guild.join_policy,
            },
        )
        .await?,
    ))
}

/// Every permission the given identity holds in this guild today, via its
/// assigned role — a `guild_members` JOIN `guild_roles` lookup by
/// `(guild_id, actor)`. Empty (not an error) if `actor` isn't a member at
/// all; the owner's authority never flows through this (see
/// [`has_guild_permission`]'s structural owner check), so a non-member
/// owner-check still works even though this returns nothing for them.
async fn actor_role_permissions(
    state: &AppState,
    guild_id: Uuid,
    actor: Uuid,
) -> Result<Vec<String>, AppError> {
    let row = sqlx::query(
        r#"
        SELECT gr.permissions FROM guild_members gm
        JOIN guild_roles gr ON gr.guild_id = gm.guild_id AND gr.name_index = gm.role_index
        WHERE gm.guild_id = $1 AND gm.identity_id = $2
        "#,
    )
    .bind(guild_id)
    .bind(actor)
    .fetch_optional(&state.pool)
    .await?;
    match row {
        Some(row) => Ok(row.try_get("permissions")?),
        None => Ok(Vec::new()),
    }
}

#[derive(Serialize)]
pub struct RoleResponse {
    pub name_index: i32,
    pub name: String,
    pub permissions: Vec<String>,
}

async fn fetch_role(
    state: &AppState,
    guild_id: Uuid,
    name_index: i32,
) -> Result<RoleResponse, AppError> {
    let row = sqlx::query(
        "SELECT name_index, name, permissions FROM guild_roles WHERE guild_id = $1 AND name_index = $2",
    )
    .bind(guild_id)
    .bind(name_index)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::GuildRoleNotFound)?;
    Ok(RoleResponse {
        name_index: row.try_get("name_index")?,
        name: row.try_get("name")?,
        permissions: row.try_get("permissions")?,
    })
}

pub async fn list_roles(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
) -> Result<Json<Vec<RoleResponse>>, AppError> {
    authenticate(&state, &headers).await?;
    // 404s if the guild doesn't exist, same as GET /guilds/{id}.
    fetch_guild(&state, guild_id).await?;

    let rows = sqlx::query(
        "SELECT name_index, name, permissions FROM guild_roles WHERE guild_id = $1 ORDER BY name_index",
    )
    .bind(guild_id)
    .fetch_all(&state.pool)
    .await?;

    let mut roles = Vec::with_capacity(rows.len());
    for row in rows {
        roles.push(RoleResponse {
            name_index: row.try_get("name_index")?,
            name: row.try_get("name")?,
            permissions: row.try_get("permissions")?,
        });
    }
    Ok(Json(roles))
}

fn normalize_permissions(raw: &[String]) -> Vec<String> {
    // Silently drops anything outside the fixed milestone-1 vocabulary
    // rather than erroring — matches "the permission set is not
    // extensible in milestone 1" without making an unrecognized string a
    // hard failure for a caller that's ahead of a future permission add.
    raw.iter()
        .filter_map(|p| GuildPermission::parse(p))
        .map(|p| p.as_str().to_string())
        .collect()
}

#[derive(Deserialize)]
pub struct CreateRoleRequest {
    pub name: String,
    #[serde(default)]
    pub permissions: Vec<String>,
}

pub async fn create_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
    Json(body): Json<CreateRoleRequest>,
) -> Result<Json<RoleResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let guild = fetch_guild(&state, guild_id).await?;

    let actor_permissions = actor_role_permissions(&state, guild_id, actor).await?;
    if !has_guild_permission(
        guild.owner,
        actor,
        &actor_permissions,
        GuildPermission::ManageRoles,
    ) {
        return Err(AppError::MissingGuildPermission);
    }

    let permissions = normalize_permissions(&body.permissions);

    let mut tx = state.pool.begin().await?;

    let next_index_row = sqlx::query(
        "SELECT COALESCE(MAX(name_index), -1) + 1 AS next FROM guild_roles WHERE guild_id = $1",
    )
    .bind(guild_id)
    .fetch_one(&mut *tx)
    .await?;
    let name_index: i32 = next_index_row.try_get("next")?;

    sqlx::query(
        "INSERT INTO guild_roles (guild_id, name_index, name, permissions) VALUES ($1, $2, $3, $4)",
    )
    .bind(guild_id)
    .bind(name_index)
    .bind(&body.name)
    .bind(&permissions)
    .execute(&mut *tx)
    .await?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "guild.role_defined".to_string(),
        issuer: identity_ref(actor, "guild_role_defined"),
        subject: guild_ref(guild_id, "guild_role_defined"),
        payload: serde_json::json!({
            "guild_id": guild_id,
            "name_index": name_index,
            "name": body.name,
            "permissions": permissions,
            "actor": actor,
        }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(RoleResponse {
        name_index,
        name: body.name,
        permissions,
    }))
}

#[derive(Deserialize)]
pub struct UpdateRoleRequest {
    pub name: Option<String>,
    pub permissions: Option<Vec<String>>,
}

pub async fn update_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, name_index)): Path<(Uuid, i32)>,
    Json(body): Json<UpdateRoleRequest>,
) -> Result<Json<RoleResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let guild = fetch_guild(&state, guild_id).await?;

    let actor_permissions = actor_role_permissions(&state, guild_id, actor).await?;
    if !has_guild_permission(
        guild.owner,
        actor,
        &actor_permissions,
        GuildPermission::ManageRoles,
    ) {
        return Err(AppError::MissingGuildPermission);
    }

    // The owner role's authority comes from `guilds.owner`, not from this
    // row's permission list (see `has_guild_permission`) — editing it here
    // would look meaningful but change nothing about who can actually act
    // as owner, which is exactly the kind of silent-no-op footgun this
    // repo avoids. Simplest fix: it isn't editable through this endpoint.
    if name_index == OWNER_ROLE_INDEX {
        return Err(AppError::CannotModifyOwnerRole);
    }

    let existing = fetch_role(&state, guild_id, name_index).await?;
    let new_name = body.name.clone().unwrap_or(existing.name);
    let new_permissions = match &body.permissions {
        Some(p) => normalize_permissions(p),
        None => existing.permissions,
    };

    let mut tx = state.pool.begin().await?;

    let updated = sqlx::query(
        "UPDATE guild_roles SET name = $3, permissions = $4 WHERE guild_id = $1 AND name_index = $2",
    )
    .bind(guild_id)
    .bind(name_index)
    .bind(&new_name)
    .bind(&new_permissions)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(AppError::GuildRoleNotFound);
    }

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "guild.role_defined".to_string(),
        issuer: identity_ref(actor, "guild_role_defined"),
        subject: guild_ref(guild_id, "guild_role_defined"),
        payload: serde_json::json!({
            "guild_id": guild_id,
            "name_index": name_index,
            "name": new_name,
            "permissions": new_permissions,
            "actor": actor,
        }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(RoleResponse {
        name_index,
        name: new_name,
        permissions: new_permissions,
    }))
}

#[derive(Deserialize)]
pub struct TransferOwnershipRequest {
    pub to: Uuid,
}

pub async fn transfer_ownership(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
    Json(body): Json<TransferOwnershipRequest>,
) -> Result<Json<GuildResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let guild = fetch_guild(&state, guild_id).await?;

    // Owner-only, no permission-list fallback — ownership transfer is not
    // delegable via `manage_guild`, unlike ordinary guild updates.
    if actor != guild.owner {
        return Err(AppError::MissingGuildPermission);
    }
    if body.to == guild.owner {
        return Err(AppError::AlreadyGuildOwner);
    }

    let target_exists = sqlx::query("SELECT 1 FROM identities WHERE id = $1")
        .bind(body.to)
        .fetch_optional(&state.pool)
        .await?;
    if target_exists.is_none() {
        return Err(AppError::IdentityNotFound);
    }

    let mut tx = state.pool.begin().await?;

    // A single UPDATE of the one `owner` column: the guild is never
    // observably ownerless or dual-owned between statements, satisfying
    // "a guild never has zero or two owners" (issue #20's invariant).
    sqlx::query("UPDATE guilds SET owner = $2 WHERE id = $1")
        .bind(guild_id)
        .bind(body.to)
        .execute(&mut *tx)
        .await?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "guild.owner_transferred".to_string(),
        issuer: identity_ref(actor, "guild_owner_transferred"),
        subject: guild_ref(guild_id, "guild_owner_transferred"),
        payload: serde_json::json!({
            "guild_id": guild_id,
            "from": guild.owner,
            "to": body.to,
        }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(
        guild_response(
            &state,
            GuildRow {
                id: guild.id,
                name: guild.name,
                tag: guild.tag,
                description: guild.description,
                owner: body.to,
                created_at: guild.created_at,
                join_policy: guild.join_policy,
            },
        )
        .await?,
    ))
}

pub async fn associate_game(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, game_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<GuildResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let guild = fetch_guild(&state, guild_id).await?;

    // A guild manager associates a game, never the game itself (ticket
    // "Game association") — `manage_guild` is the closest fit among the
    // fixed milestone-1 permission set.
    let actor_permissions = actor_role_permissions(&state, guild_id, actor).await?;
    if !has_guild_permission(
        guild.owner,
        actor,
        &actor_permissions,
        GuildPermission::ManageGuild,
    ) {
        return Err(AppError::MissingGuildPermission);
    }

    let mut tx = state.pool.begin().await?;

    sqlx::query(
        "INSERT INTO guild_game_associations (guild_id, game_id) VALUES ($1, $2) \
         ON CONFLICT (guild_id, game_id) DO NOTHING",
    )
    .bind(guild_id)
    .bind(game_id)
    .execute(&mut *tx)
    .await?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "guild.game_associated".to_string(),
        issuer: identity_ref(actor, "guild_game_associated"),
        subject: guild_ref(guild_id, "guild_game_associated"),
        payload: serde_json::json!({
            "guild_id": guild_id,
            "game_id": game_id,
            "actor": actor,
        }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(guild_response(&state, guild).await?))
}

// --- Membership lifecycle (issue #21) -------------------------------------

/// True if `actor` may leave a guild owned by `guild_owner` without first
/// transferring ownership away — false only for the owner themself.
fn can_leave(actor: Uuid, guild_owner: Uuid) -> bool {
    actor != guild_owner
}

/// True if `target` may be removed from a guild owned by `guild_owner` at
/// all — never the owner, regardless of who's asking or what permissions
/// they hold.
fn can_be_removed(target: Uuid, guild_owner: Uuid) -> bool {
    target != guild_owner
}

/// Whether `actor` (holding `actor_permissions`) may remove a member who
/// holds `target_role_index`. Plain `manage_members` is enough to remove a
/// plain member; removing anyone holding an elevated (non-member) role —
/// an officer removing another officer, say — additionally requires
/// `manage_roles`, so role authority alone can't be used to purge a peer at
/// the same tier.
fn can_remove_member(
    guild_owner: Uuid,
    actor: Uuid,
    actor_permissions: &[String],
    target_role_index: i32,
) -> bool {
    if !has_guild_permission(
        guild_owner,
        actor,
        actor_permissions,
        GuildPermission::ManageMembers,
    ) {
        return false;
    }
    if target_role_index == MEMBER_ROLE_INDEX {
        return true;
    }
    actor == guild_owner
        || has_guild_permission(
            guild_owner,
            actor,
            actor_permissions,
            GuildPermission::ManageRoles,
        )
}

/// True if `join_policy` permits `POST /guilds/{id}/join` directly, without
/// an invite.
fn can_join_directly(join_policy: JoinPolicy) -> bool {
    join_policy == JoinPolicy::Open
}

#[derive(Serialize)]
pub struct GuildMemberResponse {
    pub guild_id: Uuid,
    pub identity_id: Uuid,
    pub role_index: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub joined_at: OffsetDateTime,
}

async fn member_role_index(
    state: &AppState,
    guild_id: Uuid,
    identity_id: Uuid,
) -> Result<i32, AppError> {
    let row = sqlx::query(
        "SELECT role_index FROM guild_members WHERE guild_id = $1 AND identity_id = $2",
    )
    .bind(guild_id)
    .bind(identity_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotGuildMember)?;
    Ok(row.try_get("role_index")?)
}

#[derive(Deserialize)]
pub struct CreateGuildInviteRequest {
    pub to: Uuid,
}

#[derive(Serialize)]
pub struct GuildInviteResponse {
    pub id: Uuid,
    pub guild_id: Uuid,
    pub to: Uuid,
    pub from: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

pub async fn create_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
    Json(body): Json<CreateGuildInviteRequest>,
) -> Result<Json<GuildInviteResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let guild = fetch_guild(&state, guild_id).await?;

    let actor_permissions = actor_role_permissions(&state, guild_id, actor).await?;
    if !has_guild_permission(
        guild.owner,
        actor,
        &actor_permissions,
        GuildPermission::ManageMembers,
    ) {
        return Err(AppError::MissingGuildPermission);
    }

    let target_exists = sqlx::query("SELECT 1 FROM identities WHERE id = $1")
        .bind(body.to)
        .fetch_optional(&state.pool)
        .await?;
    if target_exists.is_none() {
        return Err(AppError::IdentityNotFound);
    }

    let already_member =
        sqlx::query("SELECT 1 FROM guild_members WHERE guild_id = $1 AND identity_id = $2")
            .bind(guild_id)
            .bind(body.to)
            .fetch_optional(&state.pool)
            .await?;
    if already_member.is_some() {
        return Err(AppError::AlreadyGuildMember);
    }

    // Not durable history — see module doc comment. No transaction/outbox
    // entry, same as `friends.rs`'s request creation.
    let invite_id = Uuid::new_v4();
    let created_at = OffsetDateTime::now_utc();
    let inserted = sqlx::query(
        r#"INSERT INTO guild_invites (id, guild_id, "to", "from", created_at) VALUES ($1, $2, $3, $4, $5)"#,
    )
    .bind(invite_id)
    .bind(guild_id)
    .bind(body.to)
    .bind(actor)
    .bind(created_at)
    .execute(&state.pool)
    .await;
    if let Err(sqlx::Error::Database(db_err)) = &inserted {
        if db_err.is_unique_violation() {
            // A pending invite to this identity already exists — idempotent
            // by design (ticket: "duplicate invite idempotent"), so return
            // the existing row rather than erroring or duplicating it.
            let existing = sqlx::query(
                r#"SELECT id, guild_id, "to", "from", created_at FROM guild_invites
                   WHERE guild_id = $1 AND "to" = $2 AND resolved_at IS NULL"#,
            )
            .bind(guild_id)
            .bind(body.to)
            .fetch_one(&state.pool)
            .await?;
            return Ok(Json(GuildInviteResponse {
                id: existing.try_get("id")?,
                guild_id: existing.try_get("guild_id")?,
                to: existing.try_get("to")?,
                from: existing.try_get("from")?,
                created_at: existing.try_get("created_at")?,
            }));
        }
    }
    inserted?;

    Ok(Json(GuildInviteResponse {
        id: invite_id,
        guild_id,
        to: body.to,
        from: actor,
        created_at,
    }))
}

struct PendingGuildInvite {
    to: Uuid,
}

async fn fetch_pending_invite(
    state: &AppState,
    guild_id: Uuid,
    invite_id: Uuid,
) -> Result<PendingGuildInvite, AppError> {
    let row = sqlx::query(
        r#"SELECT "to" FROM guild_invites WHERE id = $1 AND guild_id = $2 AND resolved_at IS NULL"#,
    )
    .bind(invite_id)
    .bind(guild_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::GuildInviteNotFound)?;
    Ok(PendingGuildInvite {
        to: row.try_get("to")?,
    })
}

pub async fn accept_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, invite_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<GuildMemberResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let invite = fetch_pending_invite(&state, guild_id, invite_id).await?;
    // Only the invited identity can accept — same "consent from the other
    // side" reasoning as `friends.rs::accept_friend_request`.
    if actor != invite.to {
        return Err(AppError::GuildInviteNotFound);
    }

    let mut tx = state.pool.begin().await?;

    let resolved = sqlx::query(
        "UPDATE guild_invites SET resolved_at = now(), outcome = 'accepted' WHERE id = $1",
    )
    .bind(invite_id)
    .execute(&mut *tx)
    .await?;
    if resolved.rows_affected() == 0 {
        // Resolved by a concurrent request between the fetch above and here.
        return Err(AppError::GuildInviteNotFound);
    }

    let joined_at = OffsetDateTime::now_utc();
    let inserted = sqlx::query(
        "INSERT INTO guild_members (guild_id, identity_id, role_index, joined_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(guild_id)
    .bind(actor)
    .bind(MEMBER_ROLE_INDEX)
    .bind(joined_at)
    .execute(&mut *tx)
    .await;
    if let Err(sqlx::Error::Database(db_err)) = &inserted {
        if db_err.is_unique_violation() {
            return Err(AppError::AlreadyGuildMember);
        }
    }
    inserted?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "guild.member_added".to_string(),
        issuer: identity_ref(actor, "guild_member_added"),
        subject: guild_ref(guild_id, "guild_member_added"),
        payload: serde_json::json!({
            "guild_id": guild_id,
            "identity_id": actor,
            "role_index": MEMBER_ROLE_INDEX,
            "via": "invite",
            "actor": actor,
        }),
        timestamp: joined_at,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(GuildMemberResponse {
        guild_id,
        identity_id: actor,
        role_index: MEMBER_ROLE_INDEX,
        joined_at,
    }))
}

pub async fn decline_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, invite_id)): Path<(Uuid, Uuid)>,
) -> Result<(), AppError> {
    let actor = authenticate(&state, &headers).await?;
    let invite = fetch_pending_invite(&state, guild_id, invite_id).await?;
    if actor != invite.to {
        return Err(AppError::GuildInviteNotFound);
    }

    // Not durable history — see module doc comment. A single projection
    // update, no outbox entry, no transaction needed.
    sqlx::query("UPDATE guild_invites SET resolved_at = now(), outcome = 'declined' WHERE id = $1")
        .bind(invite_id)
        .execute(&state.pool)
        .await?;

    Ok(())
}

pub async fn join_guild(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
) -> Result<Json<GuildMemberResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let guild = fetch_guild(&state, guild_id).await?;

    if !can_join_directly(guild.join_policy) {
        return Err(AppError::GuildNotOpen);
    }

    let mut tx = state.pool.begin().await?;

    let joined_at = OffsetDateTime::now_utc();
    let inserted = sqlx::query(
        "INSERT INTO guild_members (guild_id, identity_id, role_index, joined_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(guild_id)
    .bind(actor)
    .bind(MEMBER_ROLE_INDEX)
    .bind(joined_at)
    .execute(&mut *tx)
    .await;
    if let Err(sqlx::Error::Database(db_err)) = &inserted {
        if db_err.is_unique_violation() {
            return Err(AppError::AlreadyGuildMember);
        }
    }
    inserted?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "guild.member_added".to_string(),
        issuer: identity_ref(actor, "guild_member_added"),
        subject: guild_ref(guild_id, "guild_member_added"),
        payload: serde_json::json!({
            "guild_id": guild_id,
            "identity_id": actor,
            "role_index": MEMBER_ROLE_INDEX,
            "via": "join",
            "actor": actor,
        }),
        timestamp: joined_at,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(GuildMemberResponse {
        guild_id,
        identity_id: actor,
        role_index: MEMBER_ROLE_INDEX,
        joined_at,
    }))
}

pub async fn leave_guild(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
) -> Result<(), AppError> {
    let actor = authenticate(&state, &headers).await?;
    let guild = fetch_guild(&state, guild_id).await?;

    if !can_leave(actor, guild.owner) {
        return Err(AppError::OwnerMustTransferBeforeLeaving);
    }

    let mut tx = state.pool.begin().await?;

    let removed = sqlx::query("DELETE FROM guild_members WHERE guild_id = $1 AND identity_id = $2")
        .bind(guild_id)
        .bind(actor)
        .execute(&mut *tx)
        .await?;
    if removed.rows_affected() == 0 {
        return Err(AppError::NotGuildMember);
    }

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "guild.member_removed".to_string(),
        issuer: identity_ref(actor, "guild_member_removed"),
        subject: guild_ref(guild_id, "guild_member_removed"),
        payload: serde_json::json!({
            "guild_id": guild_id,
            "identity_id": actor,
            "reason": "left",
            "actor": actor,
        }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(())
}

pub async fn remove_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, identity_id)): Path<(Uuid, Uuid)>,
) -> Result<(), AppError> {
    let actor = authenticate(&state, &headers).await?;
    let guild = fetch_guild(&state, guild_id).await?;

    if !can_be_removed(identity_id, guild.owner) {
        return Err(AppError::CannotRemoveOwner);
    }

    let actor_permissions = actor_role_permissions(&state, guild_id, actor).await?;
    let target_role_index = member_role_index(&state, guild_id, identity_id).await?;
    if !can_remove_member(guild.owner, actor, &actor_permissions, target_role_index) {
        return Err(AppError::MissingGuildPermission);
    }

    let mut tx = state.pool.begin().await?;

    let removed = sqlx::query("DELETE FROM guild_members WHERE guild_id = $1 AND identity_id = $2")
        .bind(guild_id)
        .bind(identity_id)
        .execute(&mut *tx)
        .await?;
    if removed.rows_affected() == 0 {
        return Err(AppError::NotGuildMember);
    }

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "guild.member_removed".to_string(),
        issuer: identity_ref(actor, "guild_member_removed"),
        subject: guild_ref(guild_id, "guild_member_removed"),
        payload: serde_json::json!({
            "guild_id": guild_id,
            "identity_id": identity_id,
            "reason": "removed",
            "actor": actor,
        }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(())
}

#[derive(Deserialize)]
pub struct UpdateGuildMemberRequest {
    pub role_index: i32,
}

pub async fn update_member_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, identity_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateGuildMemberRequest>,
) -> Result<Json<GuildMemberResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let guild = fetch_guild(&state, guild_id).await?;

    let actor_permissions = actor_role_permissions(&state, guild_id, actor).await?;
    if !has_guild_permission(
        guild.owner,
        actor,
        &actor_permissions,
        GuildPermission::ManageRoles,
    ) {
        return Err(AppError::MissingGuildPermission);
    }

    // Owner-role assignment is transfer-ownership's job (#20's endpoint),
    // not this one's — the owner's authority comes from `guilds.owner`, not
    // a `guild_members.role_index` value.
    if body.role_index == OWNER_ROLE_INDEX || identity_id == guild.owner {
        return Err(AppError::CannotAssignOwnerRole);
    }

    // 404s if the target role doesn't exist for this guild.
    fetch_role(&state, guild_id, body.role_index).await?;

    let mut tx = state.pool.begin().await?;

    let updated = sqlx::query(
        "UPDATE guild_members SET role_index = $3 WHERE guild_id = $1 AND identity_id = $2",
    )
    .bind(guild_id)
    .bind(identity_id)
    .bind(body.role_index)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(AppError::NotGuildMember);
    }

    let joined_at_row =
        sqlx::query("SELECT joined_at FROM guild_members WHERE guild_id = $1 AND identity_id = $2")
            .bind(guild_id)
            .bind(identity_id)
            .fetch_one(&mut *tx)
            .await?;
    let joined_at: OffsetDateTime = joined_at_row.try_get("joined_at")?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "guild.role_changed".to_string(),
        issuer: identity_ref(actor, "guild_role_changed"),
        subject: identity_ref(identity_id, "guild_role_changed"),
        payload: serde_json::json!({
            "guild_id": guild_id,
            "identity_id": identity_id,
            "role_index": body.role_index,
            "actor": actor,
        }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(GuildMemberResponse {
        guild_id,
        identity_id,
        role_index: body.role_index,
        joined_at,
    }))
}

pub async fn list_members(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
) -> Result<Json<Vec<GuildMemberResponse>>, AppError> {
    authenticate(&state, &headers).await?;
    // 404s if the guild doesn't exist, same as GET /guilds/{id}. No
    // presence yet (see ticket) — just identity_id + role + joined_at.
    fetch_guild(&state, guild_id).await?;

    let rows = sqlx::query(
        "SELECT guild_id, identity_id, role_index, joined_at FROM guild_members WHERE guild_id = $1 ORDER BY joined_at",
    )
    .bind(guild_id)
    .fetch_all(&state.pool)
    .await?;

    let mut members = Vec::with_capacity(rows.len());
    for row in rows {
        members.push(GuildMemberResponse {
            guild_id: row.try_get("guild_id")?,
            identity_id: row.try_get("identity_id")?,
            role_index: row.try_get("role_index")?,
            joined_at: row.try_get("joined_at")?,
        });
    }
    Ok(Json(members))
}

#[derive(Serialize)]
pub struct MyGuildMembershipResponse {
    pub guild_id: Uuid,
    pub role_index: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub joined_at: OffsetDateTime,
}

pub async fn list_my_guilds(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<MyGuildMembershipResponse>>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let rows = sqlx::query(
        "SELECT guild_id, role_index, joined_at FROM guild_members WHERE identity_id = $1 ORDER BY joined_at",
    )
    .bind(identity_id)
    .fetch_all(&state.pool)
    .await?;

    let mut memberships = Vec::with_capacity(rows.len());
    for row in rows {
        memberships.push(MyGuildMembershipResponse {
            guild_id: row.try_get("guild_id")?,
            role_index: row.try_get("role_index")?,
            joined_at: row.try_get("joined_at")?,
        });
    }
    Ok(Json(memberships))
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — pure-logic checks only. The
    //! endpoint-level flows (create → owner, tag/name uniqueness, transfer
    //! leaves exactly one owner) are covered by
    //! `crates/server/tests/guilds.rs`, gated `--ignored`.

    use super::*;

    #[test]
    fn owner_always_has_every_permission_regardless_of_role() {
        let owner = Uuid::new_v4();
        assert!(has_guild_permission(
            owner,
            owner,
            &[],
            GuildPermission::ManageRoles
        ));
    }

    #[test]
    fn non_owner_without_manage_roles_is_rejected() {
        let owner = Uuid::new_v4();
        let actor = Uuid::new_v4();
        assert!(!has_guild_permission(
            owner,
            actor,
            &[],
            GuildPermission::ManageRoles
        ));
        assert!(!has_guild_permission(
            owner,
            actor,
            &["manage_members".to_string()],
            GuildPermission::ManageRoles
        ));
    }

    #[test]
    fn non_owner_with_the_specific_permission_is_allowed() {
        let owner = Uuid::new_v4();
        let actor = Uuid::new_v4();
        assert!(has_guild_permission(
            owner,
            actor,
            &["manage_roles".to_string()],
            GuildPermission::ManageRoles
        ));
    }

    #[test]
    fn validate_tag_enforces_two_to_five_characters() {
        assert!(validate_tag("ab").is_ok());
        assert!(validate_tag("abcde").is_ok());
        assert!(validate_tag("a").is_err());
        assert!(validate_tag("abcdef").is_err());
    }

    #[test]
    fn normalize_permissions_drops_unknown_strings() {
        let normalized = normalize_permissions(&[
            "manage_guild".to_string(),
            "not_a_real_permission".to_string(),
        ]);
        assert_eq!(normalized, vec!["manage_guild".to_string()]);
    }

    #[test]
    fn starter_roles_are_owner_officer_member_in_order() {
        let roles = starter_roles();
        assert_eq!(roles[0].0, OWNER_ROLE_INDEX);
        assert_eq!(roles[0].1, "owner");
        assert_eq!(roles[1].0, OFFICER_ROLE_INDEX);
        assert_eq!(roles[1].1, "officer");
        assert_eq!(roles[2].0, MEMBER_ROLE_INDEX);
        assert_eq!(roles[2].1, "member");
    }

    #[test]
    fn identity_ref_namespaces_by_identity_and_verb() {
        let id = Uuid::new_v4();
        let global_id = identity_ref(id, "guild_created");
        assert_eq!(
            global_id.as_str(),
            format!("identity:{id}:self:guild_created")
        );
    }

    #[test]
    fn guild_ref_namespaces_by_guild_and_verb() {
        let id = Uuid::new_v4();
        let global_id = guild_ref(id, "guild_created");
        assert_eq!(global_id.as_str(), format!("guild:{id}:self:guild_created"));
    }

    #[test]
    fn owner_cannot_leave_without_transferring() {
        let owner = Uuid::new_v4();
        assert!(!can_leave(owner, owner));
        let member = Uuid::new_v4();
        assert!(can_leave(member, owner));
    }

    #[test]
    fn owner_cannot_be_removed() {
        let owner = Uuid::new_v4();
        assert!(!can_be_removed(owner, owner));
        let member = Uuid::new_v4();
        assert!(can_be_removed(member, owner));
    }

    #[test]
    fn officer_with_manage_members_can_remove_a_plain_member() {
        let owner = Uuid::new_v4();
        let officer = Uuid::new_v4();
        assert!(can_remove_member(
            owner,
            officer,
            &["manage_members".to_string()],
            MEMBER_ROLE_INDEX,
        ));
    }

    #[test]
    fn officer_cannot_remove_another_officer_without_manage_roles() {
        let owner = Uuid::new_v4();
        let officer = Uuid::new_v4();
        assert!(!can_remove_member(
            owner,
            officer,
            &["manage_members".to_string()],
            OFFICER_ROLE_INDEX,
        ));
        assert!(can_remove_member(
            owner,
            officer,
            &["manage_members".to_string(), "manage_roles".to_string()],
            OFFICER_ROLE_INDEX,
        ));
    }

    #[test]
    fn owner_can_remove_an_officer_without_holding_manage_roles_explicitly() {
        let owner = Uuid::new_v4();
        // `has_guild_permission`'s structural owner check makes this true
        // even with an empty permission list.
        assert!(can_remove_member(owner, owner, &[], OFFICER_ROLE_INDEX));
    }

    #[test]
    fn removal_requires_manage_members_regardless_of_target_role() {
        let owner = Uuid::new_v4();
        let actor = Uuid::new_v4();
        assert!(!can_remove_member(owner, actor, &[], MEMBER_ROLE_INDEX));
    }

    #[test]
    fn join_is_allowed_only_for_open_guilds() {
        assert!(!can_join_directly(JoinPolicy::InviteOnly));
        assert!(can_join_directly(JoinPolicy::Open));
    }
}
