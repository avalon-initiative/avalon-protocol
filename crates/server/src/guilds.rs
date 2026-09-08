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
//! **Membership and per-member role assignment are issue #21's concern, not
//! this module's.** There is deliberately no `guild_members` table yet, so
//! [`has_guild_permission`] can only ever resolve a non-owner caller to "no
//! permissions" today — it takes an explicit `actor_permissions` slice so
//! it composes cleanly once #21 adds a real membership/role lookup, but
//! until then only the guild's owner can manage anything. That's a
//! deliberate milestone-1 narrowing, not an oversight: the ticket's "owner
//! or `manage_guild`" phrasing anticipates #21, it doesn't require this
//! ticket to build it early.
//!
//! For the same reason, `GET /guilds/{id}`'s `member_count` is reported as
//! `1` (the owner) rather than backed by a real roster — there is no
//! membership table to count yet.

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
/// `permission` present in their own role's permission list. See the
/// module doc comment for why `actor_permissions` is always empty today.
fn has_guild_permission(
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
}

async fn fetch_guild(state: &AppState, guild_id: Uuid) -> Result<GuildRow, AppError> {
    let row = sqlx::query(
        "SELECT id, name, tag, description, owner, created_at FROM guilds WHERE id = $1",
    )
    .bind(guild_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::GuildNotFound)?;
    Ok(GuildRow {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        tag: row.try_get("tag")?,
        description: row.try_get("description")?,
        owner: row.try_get("owner")?,
        created_at: row.try_get("created_at")?,
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
    /// See module doc comment — a stand-in until #21 adds real membership.
    pub member_count: i64,
    pub games: Vec<Uuid>,
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

    Ok(GuildResponse {
        id: guild.id,
        name: guild.name,
        tag: guild.tag,
        description: guild.description,
        owner: guild.owner,
        created_at: guild.created_at,
        member_count: 1,
        games,
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
            },
        )
        .await?,
    ))
}

/// Every permission the given identity holds in this guild today, via its
/// assigned role. Always empty for a non-owner — see module doc comment;
/// this exists as the seam #21 will populate once membership/role
/// assignment is real.
async fn actor_role_permissions(
    _state: &AppState,
    _guild_id: Uuid,
    _actor: Uuid,
) -> Result<Vec<String>, AppError> {
    Ok(Vec::new())
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
}
