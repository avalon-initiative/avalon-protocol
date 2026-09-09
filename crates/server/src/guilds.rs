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
//!
//! **Discovery (issue #154).** `GET /guilds/discover` is a paged,
//! filterable/searchable browse over the same public metadata `GET
//! /guilds/{id}` already exposes (name/tag/description/member_count) —
//! not a new visibility tier. It's a milestone-1 `server`-side stand-in
//! (a direct `guilds` query) for the real read model #42's indexer will
//! eventually own, same pragmatic call #44 documents for reads generally.
//! Cursor pagination here (`cursor=` holding the last-seen guild id, `(sort
//! key, id) < (subquery for that id)` keyset comparison) is the same
//! pattern `guild_messages::list_messages`'s `before=` already established
//! for #22 — just under the field name this ticket's own endpoint spec
//! uses.

use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::guilds::{
    GuildLink, GuildPermission, JoinPolicy, RoleBadge, RoleBadgeColor, RoleBadgeIcon,
};
use avalon_protocol::ids::GlobalId;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, QueryBuilder, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::AppError;
use crate::handlers::{authenticate, is_http_url};
use crate::outbox;
use crate::state::AppState;

const OWNER_ROLE_INDEX: i32 = 0;
const OFFICER_ROLE_INDEX: i32 = 1;
const MEMBER_ROLE_INDEX: i32 = 2;

/// Cap on `GuildRole.description` (issue #152) — same "short, capped text
/// field" treatment as `validate_tag`, just a longer bound since a role
/// description is prose, not a 2-5 character tag.
const MAX_ROLE_DESCRIPTION_LEN: usize = 200;

/// Cap on `Guild.motd` (issue #153) — same order of magnitude as
/// `MAX_BIO_LEN` in `handlers.rs`, since a MOTD is short prose too.
const MAX_GUILD_MOTD_LEN: usize = 500;

/// Cap on `Guild.banner`'s URL length (issue #153) — same bound
/// `MAX_AVATAR_URL_LEN` uses.
const MAX_GUILD_BANNER_URL_LEN: usize = 2048;

/// Cap on the number of entries in `Guild.links` (issue #153) — keeps this
/// from becoming an arbitrary free-form content field, per the ticket.
const MAX_GUILD_LINKS: usize = 5;

/// Cap on a single `GuildLink.label`'s length (issue #153).
const MAX_GUILD_LINK_LABEL_LEN: usize = 60;

/// Cap on a single `GuildLink.url`'s length (issue #153) — same bound
/// `MAX_AVATAR_URL_LEN`/`MAX_GUILD_BANNER_URL_LEN` use.
const MAX_GUILD_LINK_URL_LEN: usize = 2048;

/// Default/maximum page size for `GET /guilds/discover` (issue #154) — same
/// "small default, capped maximum" shape `guild_messages`'s
/// `DEFAULT_MESSAGE_PAGE_SIZE`/`MAX_MESSAGE_PAGE_SIZE` already use.
const DEFAULT_DISCOVER_PAGE_SIZE: i64 = 20;
const MAX_DISCOVER_PAGE_SIZE: i64 = 100;

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

/// `(name_index, name, permissions, description, badge)` per starter role
/// — description/badge defaults added for issue #152, same "sensible
/// defaults for the starter roles" the ticket's design section calls for.
fn starter_roles() -> [(
    i32,
    &'static str,
    &'static [&'static str],
    &'static str,
    RoleBadge,
); 3] {
    [
        (
            OWNER_ROLE_INDEX,
            "owner",
            GuildPermission::ALL_STRS,
            "Full control over the guild.",
            RoleBadge {
                icon: RoleBadgeIcon::Crown,
                color: RoleBadgeColor::Gold,
            },
        ),
        (
            OFFICER_ROLE_INDEX,
            "officer",
            &["manage_members", "manage_channels"],
            "Manages members and channels.",
            RoleBadge {
                icon: RoleBadgeIcon::Shield,
                color: RoleBadgeColor::Blue,
            },
        ),
        (
            MEMBER_ROLE_INDEX,
            "member",
            &[],
            "A guild member.",
            RoleBadge::DEFAULT,
        ),
    ]
}

fn validate_tag(tag: &str) -> Result<(), AppError> {
    let len = tag.chars().count();
    if !(2..=5).contains(&len) {
        return Err(AppError::InvalidGuildTag);
    }
    Ok(())
}

/// Issue #152's invariant: a role description is capped, same as other
/// guild text fields — but unlike `body.description` on `Guild` itself
/// (never actually length-checked today), a role's description *is*
/// validated here, since this ticket is the first one to specify a cap for
/// it explicitly.
fn validate_role_description(description: &str) -> Result<(), AppError> {
    if description.chars().count() > MAX_ROLE_DESCRIPTION_LEN {
        return Err(AppError::InvalidRoleDescription);
    }
    Ok(())
}

/// Same empty-string-clears convention `handlers::validate_bio` uses. A
/// non-empty value must be within [`MAX_GUILD_MOTD_LEN`] characters or the
/// request is rejected — never silently truncated.
fn validate_guild_motd(motd: &str) -> Result<Option<String>, AppError> {
    if motd.is_empty() {
        return Ok(None);
    }
    if motd.chars().count() > MAX_GUILD_MOTD_LEN {
        return Err(AppError::InvalidGuildMotd);
    }
    Ok(Some(motd.to_string()))
}

/// Same empty-string-clears convention `handlers::validate_avatar_url`
/// uses, reusing its `http`/`https`-URL validation
/// ([`crate::handlers::is_http_url`]) rather than re-implementing it.
fn validate_guild_banner(banner: &str) -> Result<Option<String>, AppError> {
    if banner.is_empty() {
        return Ok(None);
    }
    if !is_http_url(banner, MAX_GUILD_BANNER_URL_LEN) {
        return Err(AppError::InvalidGuildBanner);
    }
    Ok(Some(banner.to_string()))
}

/// Wire shape for one entry of `UpdateGuildRequest.links` (issue #153).
#[derive(Deserialize)]
pub struct GuildLinkRequest {
    pub label: String,
    pub url: String,
}

/// Issue #153's invariants: at most [`MAX_GUILD_LINKS`] entries, each
/// label 1-[`MAX_GUILD_LINK_LABEL_LEN`] characters, each url a valid
/// `http`/`https` URL within [`MAX_GUILD_LINK_URL_LEN`] characters. Unlike
/// `motd`/`banner`, a link entry has no "empty means clear" state of its
/// own — an invalid entry is rejected outright, never silently dropped or
/// truncated (the whole `links` list is either accepted or rejected as a
/// unit).
fn validate_guild_links(links: &[GuildLinkRequest]) -> Result<Vec<GuildLink>, AppError> {
    if links.len() > MAX_GUILD_LINKS {
        return Err(AppError::TooManyGuildLinks);
    }
    let mut parsed = Vec::with_capacity(links.len());
    for link in links {
        let label_len = link.label.chars().count();
        if label_len == 0 || label_len > MAX_GUILD_LINK_LABEL_LEN {
            return Err(AppError::InvalidGuildLink);
        }
        if !is_http_url(&link.url, MAX_GUILD_LINK_URL_LEN) {
            return Err(AppError::InvalidGuildLink);
        }
        parsed.push(GuildLink {
            label: link.label.clone(),
            url: link.url.clone(),
        });
    }
    Ok(parsed)
}

/// Wire shape for a badge in a create/update role request — plain strings
/// rather than deserializing straight into `RoleBadgeIcon`/`RoleBadgeColor`,
/// so an unrecognized id goes through the same explicit
/// validate-and-reject path (`AppError::InvalidRoleBadge`) as every other
/// guild input in this module, instead of a generic JSON-deserialization
/// rejection a caller can't distinguish from a malformed request body.
#[derive(Deserialize)]
pub struct RoleBadgeRequest {
    pub icon: String,
    pub color: String,
}

impl RoleBadgeRequest {
    /// Issue #152's invariant: an unrecognized icon or color id is a
    /// rejected request, not silently dropped or coerced to a default —
    /// deliberately different from `normalize_permissions`, which *does*
    /// drop unknown permission strings, since a badge is presentational
    /// and a caller sending an unknown id here is far more likely to be a
    /// real bug (typo, stale client) worth surfacing than a
    /// forward-compatibility case.
    fn into_badge(self) -> Result<RoleBadge, AppError> {
        let icon = RoleBadgeIcon::parse(&self.icon).ok_or(AppError::InvalidRoleBadge)?;
        let color = RoleBadgeColor::parse(&self.color).ok_or(AppError::InvalidRoleBadge)?;
        Ok(RoleBadge { icon, color })
    }
}

fn badge_payload(badge: RoleBadge) -> serde_json::Value {
    serde_json::json!({
        "icon": badge.icon.as_str(),
        "color": badge.color.as_str(),
    })
}

struct GuildRow {
    id: Uuid,
    name: String,
    tag: String,
    description: String,
    owner: Uuid,
    created_at: OffsetDateTime,
    join_policy: JoinPolicy,
    /// Issue #153.
    motd: Option<String>,
    /// Issue #153.
    banner: Option<String>,
    /// Issue #153.
    links: Vec<GuildLink>,
    /// Issue #153.
    recruiting: bool,
}

async fn fetch_guild(state: &AppState, guild_id: Uuid) -> Result<GuildRow, AppError> {
    let row = sqlx::query(
        "SELECT id, name, tag, description, owner, created_at, join_policy, motd, banner, links, recruiting FROM guilds WHERE id = $1",
    )
    .bind(guild_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::GuildNotFound)?;
    let join_policy_raw: String = row.try_get("join_policy")?;
    let links_raw: serde_json::Value = row.try_get("links")?;
    // Falls back to an empty list if the stored JSON is ever somehow
    // unparseable — never a hard failure on a read path, same posture as
    // `join_policy`'s fallback just below.
    let links: Vec<GuildLink> = serde_json::from_value(links_raw).unwrap_or_default();
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
        motd: row.try_get("motd")?,
        banner: row.try_get("banner")?,
        links,
        recruiting: row.try_get("recruiting")?,
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
    /// Issue #153.
    pub motd: Option<String>,
    /// Issue #153.
    pub banner: Option<String>,
    /// Issue #153.
    pub links: Vec<GuildLink>,
    /// Issue #153.
    pub recruiting: bool,
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
        motd: guild.motd,
        banner: guild.banner,
        links: guild.links,
        recruiting: guild.recruiting,
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

    for (name_index, name, permissions, description, badge) in starter_roles() {
        sqlx::query(
            "INSERT INTO guild_roles (guild_id, name_index, name, permissions, description, badge_icon, badge_color) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(guild_id)
        .bind(name_index)
        .bind(name)
        .bind(permissions)
        .bind(description)
        .bind(badge.icon.as_str())
        .bind(badge.color.as_str())
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
                motd: None,
                banner: None,
                links: Vec::new(),
                recruiting: false,
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
    /// Issue #153. Three states, same as `UpdateProfileRequest::bio`:
    /// omitted (untouched), `Some("")` (clear to `NULL`), `Some(nonempty)`
    /// (validate against [`MAX_GUILD_MOTD_LEN`], then set).
    pub motd: Option<String>,
    /// Issue #153. Same three-state convention as `motd`, same
    /// `http`/`https`-URL validation as a profile's `avatar_url`.
    pub banner: Option<String>,
    /// Issue #153. Two states, not three: omitted (untouched) or
    /// `Some(list)`, which always fully replaces the stored list —
    /// including `Some(vec![])` to clear it. Each entry is validated; an
    /// invalid entry rejects the whole request rather than being dropped.
    pub links: Option<Vec<GuildLinkRequest>>,
    /// Issue #153. Omitted leaves it untouched.
    pub recruiting: Option<bool>,
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
    let new_motd = match &body.motd {
        Some(raw) => validate_guild_motd(raw)?,
        None => guild.motd.clone(),
    };
    let new_banner = match &body.banner {
        Some(raw) => validate_guild_banner(raw)?,
        None => guild.banner.clone(),
    };
    let new_links = match &body.links {
        Some(links) => validate_guild_links(links)?,
        None => guild.links.clone(),
    };
    let new_recruiting = body.recruiting.unwrap_or(guild.recruiting);
    let new_links_json =
        serde_json::to_value(&new_links).expect("GuildLink always serializes to JSON");

    let mut tx = state.pool.begin().await?;

    let updated = sqlx::query(
        "UPDATE guilds SET name = $2, tag = $3, description = $4, motd = $5, banner = $6, links = $7, recruiting = $8 WHERE id = $1",
    )
    .bind(guild_id)
    .bind(&new_name)
    .bind(&new_tag)
    .bind(&new_description)
    .bind(&new_motd)
    .bind(&new_banner)
    .bind(&new_links_json)
    .bind(new_recruiting)
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
            "motd": new_motd,
            "banner": new_banner,
            "links": new_links,
            "recruiting": new_recruiting,
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
                motd: new_motd,
                banner: new_banner,
                links: new_links,
                recruiting: new_recruiting,
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
pub(crate) async fn actor_role_permissions(
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
    pub description: String,
    pub badge: RoleBadge,
}

/// Reads `badge_icon`/`badge_color` off a `guild_roles` row and falls back
/// to [`RoleBadge::DEFAULT`] if either is ever somehow unparseable — same
/// "never a hard failure on a read path" posture `fetch_guild`'s
/// `join_policy` parse already takes, since every write path validates
/// these before they ever reach the database.
fn row_badge(row: &sqlx::postgres::PgRow) -> Result<RoleBadge, AppError> {
    let icon_raw: String = row.try_get("badge_icon")?;
    let color_raw: String = row.try_get("badge_color")?;
    Ok(RoleBadge {
        icon: RoleBadgeIcon::parse(&icon_raw).unwrap_or(RoleBadgeIcon::Star),
        color: RoleBadgeColor::parse(&color_raw).unwrap_or(RoleBadgeColor::Gray),
    })
}

async fn fetch_role(
    state: &AppState,
    guild_id: Uuid,
    name_index: i32,
) -> Result<RoleResponse, AppError> {
    let row = sqlx::query(
        "SELECT name_index, name, permissions, description, badge_icon, badge_color FROM guild_roles WHERE guild_id = $1 AND name_index = $2",
    )
    .bind(guild_id)
    .bind(name_index)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::GuildRoleNotFound)?;
    let badge = row_badge(&row)?;
    Ok(RoleResponse {
        name_index: row.try_get("name_index")?,
        name: row.try_get("name")?,
        permissions: row.try_get("permissions")?,
        description: row.try_get("description")?,
        badge,
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
        "SELECT name_index, name, permissions, description, badge_icon, badge_color FROM guild_roles WHERE guild_id = $1 ORDER BY name_index",
    )
    .bind(guild_id)
    .fetch_all(&state.pool)
    .await?;

    let mut roles = Vec::with_capacity(rows.len());
    for row in rows {
        let badge = row_badge(&row)?;
        roles.push(RoleResponse {
            name_index: row.try_get("name_index")?,
            name: row.try_get("name")?,
            permissions: row.try_get("permissions")?,
            description: row.try_get("description")?,
            badge,
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
    /// Issue #152. Defaults to an empty string, same "no explicit
    /// `Option` needed, empty is a valid value" treatment `Guild.description`
    /// already gets.
    #[serde(default)]
    pub description: String,
    /// Issue #152. Omitted entirely (not just an empty object) means
    /// [`RoleBadge::DEFAULT`] — a caller ahead of Hub UI support for
    /// choosing a badge still gets a valid, renderable role.
    pub badge: Option<RoleBadgeRequest>,
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
    validate_role_description(&body.description)?;
    let badge = match body.badge {
        Some(b) => b.into_badge()?,
        None => RoleBadge::DEFAULT,
    };

    let mut tx = state.pool.begin().await?;

    let next_index_row = sqlx::query(
        "SELECT COALESCE(MAX(name_index), -1) + 1 AS next FROM guild_roles WHERE guild_id = $1",
    )
    .bind(guild_id)
    .fetch_one(&mut *tx)
    .await?;
    let name_index: i32 = next_index_row.try_get("next")?;

    sqlx::query(
        "INSERT INTO guild_roles (guild_id, name_index, name, permissions, description, badge_icon, badge_color) VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(guild_id)
    .bind(name_index)
    .bind(&body.name)
    .bind(&permissions)
    .bind(&body.description)
    .bind(badge.icon.as_str())
    .bind(badge.color.as_str())
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
            "description": body.description,
            "badge": badge_payload(badge),
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
        description: body.description,
        badge,
    }))
}

#[derive(Deserialize)]
pub struct UpdateRoleRequest {
    pub name: Option<String>,
    pub permissions: Option<Vec<String>>,
    /// Issue #152. `None` leaves the existing description untouched — same
    /// partial-update convention `name`/`permissions` already use.
    pub description: Option<String>,
    /// Issue #152. `None` leaves the existing badge untouched.
    pub badge: Option<RoleBadgeRequest>,
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
    let new_description = match &body.description {
        Some(d) => {
            validate_role_description(d)?;
            d.clone()
        }
        None => existing.description,
    };
    let new_badge = match body.badge {
        Some(b) => b.into_badge()?,
        None => existing.badge,
    };

    let mut tx = state.pool.begin().await?;

    let updated = sqlx::query(
        "UPDATE guild_roles SET name = $3, permissions = $4, description = $5, badge_icon = $6, badge_color = $7 WHERE guild_id = $1 AND name_index = $2",
    )
    .bind(guild_id)
    .bind(name_index)
    .bind(&new_name)
    .bind(&new_permissions)
    .bind(&new_description)
    .bind(new_badge.icon.as_str())
    .bind(new_badge.color.as_str())
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
            "description": new_description,
            "badge": badge_payload(new_badge),
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
        description: new_description,
        badge: new_badge,
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
                motd: guild.motd,
                banner: guild.banner,
                links: guild.links,
                recruiting: guild.recruiting,
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

/// `sort=` values `GET /guilds/discover` (issue #154) accepts. `MostMembers`
/// depends on #21's roster (`guild_members`), which is real today, so all
/// three ticket-listed options are implemented — no "trending"/engagement
/// ranking, per the ticket's explicit "not yet" on that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscoverSort {
    Newest,
    Alphabetical,
    MostMembers,
}

impl DiscoverSort {
    fn parse(raw: Option<&str>) -> Result<DiscoverSort, AppError> {
        Ok(match raw {
            None | Some("newest") => DiscoverSort::Newest,
            Some("alphabetical") => DiscoverSort::Alphabetical,
            Some("most_members") => DiscoverSort::MostMembers,
            Some(_) => return Err(AppError::InvalidDiscoverQuery),
        })
    }
}

/// Escapes `%`/`_`/backslash in free-text user input before it's embedded
/// in an `ILIKE` pattern (Postgres's default `ILIKE` escape character is
/// backslash) — otherwise a caller's own `q=` or `tag=` value could inject
/// wildcard behavior rather than being matched literally.
fn escape_like(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[derive(Deserialize)]
pub struct DiscoverGuildsQuery {
    /// Free-text search over `name`/`tag`/`description` (case-insensitive
    /// substring).
    pub q: Option<String>,
    /// `true`/`false` to filter exactly; omitted falls back to the
    /// default-visibility rule documented on [`discover_guilds`].
    pub recruiting: Option<bool>,
    /// Case-insensitive exact match on `guilds.tag`.
    pub tag: Option<String>,
    /// Filter to guilds associated (issue #20's `associate_game`) with this
    /// game id.
    pub game: Option<Uuid>,
    /// `newest` (default) | `alphabetical` | `most_members`.
    pub sort: Option<String>,
    pub limit: Option<i64>,
    /// The last guild id from the previous page's results — see the module
    /// doc comment for why this is a bare id (same shape as
    /// `guild_messages::ListMessagesQuery::before`) rather than an opaque
    /// blob.
    pub cursor: Option<Uuid>,
}

#[derive(Serialize)]
pub struct DiscoverGuildSummary {
    pub id: Uuid,
    pub name: String,
    pub tag: String,
    pub description: String,
    pub recruiting: bool,
    pub member_count: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Serialize)]
pub struct DiscoverGuildsResponse {
    pub guilds: Vec<DiscoverGuildSummary>,
    /// `Some(id)` when another page exists — pass it back as `cursor=` to
    /// fetch it. `None` means this was the last page.
    pub next_cursor: Option<Uuid>,
}

/// `GET /guilds/discover?q=&recruiting=&tag=&game=&sort=&limit=&cursor=`
/// (issue #154). Session-authenticated only — any authenticated identity
/// may browse, no membership requirement, matching #20's existing "guild
/// name/tag/description/member_count are readable by any authenticated
/// identity" precedent. Milestone-1 stand-in: a direct query over the
/// `guilds` projection, not yet #42's real indexer read model (see module
/// doc comment and `docs/architecture/guilds.md`).
///
/// Visibility rule for `recruiting`: if the caller passes `recruiting=true`
/// or `recruiting=false` explicitly, that's an exact filter, full stop. If
/// the parameter is omitted, the default is "recruiting guilds, plus any
/// guild the caller is already a member of regardless of its recruiting
/// flag" — a non-recruiting guild never appears in a stranger's browse
/// results, but a member always sees their own guilds' discovery card, same
/// as `GET /guilds/{id}`/`GET /me/guilds` already let them look it up
/// directly. Exact id/tag lookup (`GET /guilds/{id}`) is untouched by any
/// of this — a non-recruiting guild is always reachable that way.
/// Builds the `guilds.discover` query — split out from [`discover_guilds`]
/// so the filter/sort/pagination logic can be unit-tested (via
/// [`sqlx::QueryBuilder::sql`]) without a live Postgres connection.
fn build_discover_query(
    query: &DiscoverGuildsQuery,
    sort: DiscoverSort,
    actor: Uuid,
    limit: i64,
) -> QueryBuilder<Postgres> {
    let mut builder: QueryBuilder<Postgres> = QueryBuilder::new(
        "SELECT g.id, g.name, g.tag, g.description, g.recruiting, g.created_at, \
         (SELECT COUNT(*) FROM guild_members gm WHERE gm.guild_id = g.id) AS member_count \
         FROM guilds g WHERE 1 = 1",
    );

    if let Some(q) = query.q.as_ref().filter(|s| !s.trim().is_empty()) {
        let like = format!("%{}%", escape_like(q));
        builder.push(" AND (g.name ILIKE ");
        builder.push_bind(like.clone());
        builder.push(" OR g.tag ILIKE ");
        builder.push_bind(like.clone());
        builder.push(" OR g.description ILIKE ");
        builder.push_bind(like);
        builder.push(")");
    }

    match query.recruiting {
        Some(want_recruiting) => {
            builder.push(" AND g.recruiting = ");
            builder.push_bind(want_recruiting);
        }
        None => {
            builder.push(
                " AND (g.recruiting = true OR g.id IN (SELECT guild_id FROM guild_members WHERE identity_id = ",
            );
            builder.push_bind(actor);
            builder.push("))");
        }
    }

    if let Some(tag) = query.tag.as_ref().filter(|s| !s.is_empty()) {
        builder.push(" AND g.tag ILIKE ");
        builder.push_bind(escape_like(tag));
    }

    if let Some(game_id) = query.game {
        builder.push(
            " AND EXISTS (SELECT 1 FROM guild_game_associations gga WHERE gga.guild_id = g.id AND gga.game_id = ",
        );
        builder.push_bind(game_id);
        builder.push(")");
    }

    if let Some(cursor_id) = query.cursor {
        match sort {
            DiscoverSort::Newest => {
                builder.push(
                    " AND (g.created_at, g.id) < (SELECT created_at, id FROM guilds WHERE id = ",
                );
                builder.push_bind(cursor_id);
                builder.push(")");
            }
            DiscoverSort::Alphabetical => {
                builder.push(" AND (g.name, g.id) > (SELECT name, id FROM guilds WHERE id = ");
                builder.push_bind(cursor_id);
                builder.push(")");
            }
            DiscoverSort::MostMembers => {
                builder.push(
                    " AND ((SELECT COUNT(*) FROM guild_members gm2 WHERE gm2.guild_id = g.id), g.id) < \
                     ((SELECT COUNT(*) FROM guild_members WHERE guild_id = ",
                );
                builder.push_bind(cursor_id);
                builder.push("), ");
                builder.push_bind(cursor_id);
                builder.push(")");
            }
        }
    }

    match sort {
        DiscoverSort::Newest => builder.push(" ORDER BY g.created_at DESC, g.id DESC"),
        DiscoverSort::Alphabetical => builder.push(" ORDER BY g.name ASC, g.id ASC"),
        DiscoverSort::MostMembers => builder.push(" ORDER BY member_count DESC, g.id DESC"),
    };

    // Fetch one extra row past the page size, purely to know whether a next
    // page exists — trimmed back off before building the response.
    builder.push(" LIMIT ");
    builder.push_bind(limit + 1);

    builder
}

pub async fn discover_guilds(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<DiscoverGuildsQuery>,
) -> Result<Json<DiscoverGuildsResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let sort = DiscoverSort::parse(query.sort.as_deref())?;
    let limit = query
        .limit
        .unwrap_or(DEFAULT_DISCOVER_PAGE_SIZE)
        .clamp(1, MAX_DISCOVER_PAGE_SIZE);

    let mut builder = build_discover_query(&query, sort, actor, limit);
    let rows = builder.build().fetch_all(&state.pool).await?;
    let has_more = rows.len() as i64 > limit;

    let mut guilds = Vec::with_capacity(rows.len().min(limit as usize));
    for row in rows.iter().take(limit as usize) {
        guilds.push(DiscoverGuildSummary {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            tag: row.try_get("tag")?,
            description: row.try_get("description")?,
            recruiting: row.try_get("recruiting")?,
            member_count: row.try_get("member_count")?,
            created_at: row.try_get("created_at")?,
        });
    }
    let next_cursor = if has_more {
        guilds.last().map(|g| g.id)
    } else {
        None
    };

    Ok(Json(DiscoverGuildsResponse {
        guilds,
        next_cursor,
    }))
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

    // --- Issue #152: role description + badge ---

    #[test]
    fn role_description_at_the_cap_is_accepted() {
        let description = "a".repeat(MAX_ROLE_DESCRIPTION_LEN);
        assert!(validate_role_description(&description).is_ok());
    }

    #[test]
    fn role_description_over_the_cap_is_rejected() {
        let description = "a".repeat(MAX_ROLE_DESCRIPTION_LEN + 1);
        assert!(matches!(
            validate_role_description(&description),
            Err(AppError::InvalidRoleDescription)
        ));
    }

    #[test]
    fn empty_role_description_is_accepted() {
        assert!(validate_role_description("").is_ok());
    }

    #[test]
    fn valid_badge_request_parses_into_a_role_badge() {
        let badge = RoleBadgeRequest {
            icon: "crown".to_string(),
            color: "gold".to_string(),
        }
        .into_badge()
        .expect("a known icon/color pair should parse");
        assert_eq!(badge.icon, RoleBadgeIcon::Crown);
        assert_eq!(badge.color, RoleBadgeColor::Gold);
    }

    #[test]
    fn unrecognized_badge_icon_is_rejected_not_dropped() {
        let result = RoleBadgeRequest {
            icon: "not_a_real_icon".to_string(),
            color: "gold".to_string(),
        }
        .into_badge();
        assert!(matches!(result, Err(AppError::InvalidRoleBadge)));
    }

    #[test]
    fn unrecognized_badge_color_is_rejected_not_dropped() {
        let result = RoleBadgeRequest {
            icon: "crown".to_string(),
            color: "not_a_real_color".to_string(),
        }
        .into_badge();
        assert!(matches!(result, Err(AppError::InvalidRoleBadge)));
    }

    #[test]
    fn starter_roles_carry_sensible_description_and_badge_defaults() {
        let roles = starter_roles();
        // owner
        assert_eq!(roles[0].3, "Full control over the guild.");
        assert_eq!(roles[0].4.icon, RoleBadgeIcon::Crown);
        assert_eq!(roles[0].4.color, RoleBadgeColor::Gold);
        // officer
        assert_eq!(roles[1].3, "Manages members and channels.");
        assert_eq!(roles[1].4.icon, RoleBadgeIcon::Shield);
        assert_eq!(roles[1].4.color, RoleBadgeColor::Blue);
        // member falls back to the same default a role without an
        // explicit badge gets.
        assert_eq!(roles[2].4, RoleBadge::DEFAULT);
    }

    #[test]
    fn role_badge_icon_round_trips_through_as_str_and_parse() {
        for icon in RoleBadgeIcon::ALL {
            assert_eq!(RoleBadgeIcon::parse(icon.as_str()), Some(icon));
        }
    }

    #[test]
    fn role_badge_color_round_trips_through_as_str_and_parse() {
        for color in RoleBadgeColor::ALL {
            assert_eq!(RoleBadgeColor::parse(color.as_str()), Some(color));
        }
    }

    // --- Issue #153: guild motd/banner/links/recruiting ---

    #[test]
    fn empty_motd_clears_to_none() {
        assert_eq!(validate_guild_motd("").unwrap(), None);
    }

    #[test]
    fn motd_at_the_cap_is_accepted() {
        let motd = "a".repeat(MAX_GUILD_MOTD_LEN);
        assert_eq!(validate_guild_motd(&motd).unwrap(), Some(motd));
    }

    #[test]
    fn motd_over_the_cap_is_rejected() {
        let motd = "a".repeat(MAX_GUILD_MOTD_LEN + 1);
        assert!(matches!(
            validate_guild_motd(&motd),
            Err(AppError::InvalidGuildMotd)
        ));
    }

    #[test]
    fn empty_banner_clears_to_none() {
        assert_eq!(validate_guild_banner("").unwrap(), None);
    }

    #[test]
    fn valid_https_banner_is_accepted() {
        assert_eq!(
            validate_guild_banner("https://example.com/banner.png").unwrap(),
            Some("https://example.com/banner.png".to_string())
        );
    }

    #[test]
    fn non_http_scheme_banner_is_rejected() {
        assert!(matches!(
            validate_guild_banner("javascript:alert(1)"),
            Err(AppError::InvalidGuildBanner)
        ));
    }

    #[test]
    fn overlong_banner_is_rejected() {
        let overlong = format!(
            "https://example.com/{}",
            "a".repeat(MAX_GUILD_BANNER_URL_LEN)
        );
        assert!(matches!(
            validate_guild_banner(&overlong),
            Err(AppError::InvalidGuildBanner)
        ));
    }

    #[test]
    fn valid_links_round_trip() {
        let links = vec![
            GuildLinkRequest {
                label: "Discord".to_string(),
                url: "https://discord.gg/example".to_string(),
            },
            GuildLinkRequest {
                label: "Website".to_string(),
                url: "https://example.com".to_string(),
            },
        ];
        let parsed = validate_guild_links(&links).expect("valid links should parse");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].label, "Discord");
        assert_eq!(parsed[0].url, "https://discord.gg/example");
    }

    #[test]
    fn empty_links_list_is_accepted() {
        assert_eq!(validate_guild_links(&[]).unwrap(), Vec::new());
    }

    #[test]
    fn too_many_links_is_rejected() {
        let links: Vec<GuildLinkRequest> = (0..MAX_GUILD_LINKS + 1)
            .map(|i| GuildLinkRequest {
                label: format!("Link {i}"),
                url: "https://example.com".to_string(),
            })
            .collect();
        assert!(matches!(
            validate_guild_links(&links),
            Err(AppError::TooManyGuildLinks)
        ));
    }

    #[test]
    fn link_count_at_the_cap_is_accepted() {
        let links: Vec<GuildLinkRequest> = (0..MAX_GUILD_LINKS)
            .map(|i| GuildLinkRequest {
                label: format!("Link {i}"),
                url: "https://example.com".to_string(),
            })
            .collect();
        assert!(validate_guild_links(&links).is_ok());
    }

    #[test]
    fn link_with_empty_label_is_rejected() {
        let links = vec![GuildLinkRequest {
            label: "".to_string(),
            url: "https://example.com".to_string(),
        }];
        assert!(matches!(
            validate_guild_links(&links),
            Err(AppError::InvalidGuildLink)
        ));
    }

    #[test]
    fn link_with_overlong_label_is_rejected() {
        let links = vec![GuildLinkRequest {
            label: "a".repeat(MAX_GUILD_LINK_LABEL_LEN + 1),
            url: "https://example.com".to_string(),
        }];
        assert!(matches!(
            validate_guild_links(&links),
            Err(AppError::InvalidGuildLink)
        ));
    }

    #[test]
    fn link_with_invalid_url_is_rejected() {
        let links = vec![GuildLinkRequest {
            label: "Discord".to_string(),
            url: "not a url".to_string(),
        }];
        assert!(matches!(
            validate_guild_links(&links),
            Err(AppError::InvalidGuildLink)
        ));
    }

    #[test]
    fn link_with_non_http_scheme_url_is_rejected() {
        let links = vec![GuildLinkRequest {
            label: "Discord".to_string(),
            url: "javascript:alert(1)".to_string(),
        }];
        assert!(matches!(
            validate_guild_links(&links),
            Err(AppError::InvalidGuildLink)
        ));
    }

    // -- Issue #154: discovery board query building/filtering --

    #[test]
    fn discover_sort_parses_known_values_and_defaults_to_newest() {
        assert_eq!(DiscoverSort::parse(None).unwrap(), DiscoverSort::Newest);
        assert_eq!(
            DiscoverSort::parse(Some("newest")).unwrap(),
            DiscoverSort::Newest
        );
        assert_eq!(
            DiscoverSort::parse(Some("alphabetical")).unwrap(),
            DiscoverSort::Alphabetical
        );
        assert_eq!(
            DiscoverSort::parse(Some("most_members")).unwrap(),
            DiscoverSort::MostMembers
        );
    }

    #[test]
    fn discover_sort_rejects_unknown_value() {
        assert!(matches!(
            DiscoverSort::parse(Some("trending")),
            Err(AppError::InvalidDiscoverQuery)
        ));
    }

    #[test]
    fn escape_like_neutralizes_wildcard_characters() {
        assert_eq!(escape_like("100%_evil\\"), "100\\%\\_evil\\\\");
        assert_eq!(escape_like("normal tag"), "normal tag");
    }

    fn empty_discover_query() -> DiscoverGuildsQuery {
        DiscoverGuildsQuery {
            q: None,
            recruiting: None,
            tag: None,
            game: None,
            sort: None,
            limit: None,
            cursor: None,
        }
    }

    /// With `recruiting` omitted, the query must fall back to "recruiting
    /// guilds, or a guild the caller already belongs to" rather than every
    /// guild — this is the piece of ticket #154's invariant ("non-recruiting
    /// guilds are excluded from the general browse/search results unless
    /// the caller is a member") that lives in query construction, so it's
    /// asserted here at the SQL-shape level; the end-to-end behavior (a
    /// non-recruiting guild is actually absent from a stranger's results,
    /// exact tag/id lookup still finds it) is covered by
    /// `crates/server/tests/guilds.rs`, gated `--ignored`.
    #[test]
    fn omitted_recruiting_filter_falls_back_to_recruiting_or_member() {
        let query = empty_discover_query();
        let actor = Uuid::new_v4();
        let builder = build_discover_query(&query, DiscoverSort::Newest, actor, 20);
        let sql = builder.sql();
        let sql = sql.as_str();
        assert!(sql.contains("g.recruiting = true OR g.id IN"));
        assert!(!sql.contains("g.recruiting = $"));
    }

    #[test]
    fn explicit_recruiting_true_filters_exactly_and_skips_membership_fallback() {
        let mut query = empty_discover_query();
        query.recruiting = Some(true);
        let actor = Uuid::new_v4();
        let builder = build_discover_query(&query, DiscoverSort::Newest, actor, 20);
        let sql = builder.sql();
        let sql = sql.as_str();
        assert!(sql.contains("g.recruiting = $"));
        assert!(!sql.contains("g.recruiting = true OR"));
    }

    #[test]
    fn explicit_recruiting_false_is_also_an_exact_filter() {
        let mut query = empty_discover_query();
        query.recruiting = Some(false);
        let actor = Uuid::new_v4();
        let builder = build_discover_query(&query, DiscoverSort::Newest, actor, 20);
        let sql = builder.sql();
        let sql = sql.as_str();
        assert!(sql.contains("g.recruiting = $"));
        assert!(!sql.contains("g.recruiting = true OR"));
    }

    #[test]
    fn text_search_matches_name_tag_and_description() {
        let mut query = empty_discover_query();
        query.q = Some("dragons".to_string());
        let actor = Uuid::new_v4();
        let builder = build_discover_query(&query, DiscoverSort::Newest, actor, 20);
        let sql = builder.sql();
        let sql = sql.as_str();
        assert!(sql.contains("g.name ILIKE"));
        assert!(sql.contains("g.tag ILIKE"));
        assert!(sql.contains("g.description ILIKE"));
    }

    #[test]
    fn blank_search_term_is_dropped_rather_than_matching_everything() {
        let mut query = empty_discover_query();
        query.q = Some("   ".to_string());
        let actor = Uuid::new_v4();
        let builder = build_discover_query(&query, DiscoverSort::Newest, actor, 20);
        assert!(!builder.sql().as_str().contains("ILIKE"));
    }

    #[test]
    fn tag_filter_is_present_only_when_given() {
        let actor = Uuid::new_v4();
        let without_tag =
            build_discover_query(&empty_discover_query(), DiscoverSort::Newest, actor, 20);
        assert!(!without_tag.sql().as_str().contains("g.tag ILIKE"));

        let mut query = empty_discover_query();
        query.tag = Some("ABC".to_string());
        let with_tag = build_discover_query(&query, DiscoverSort::Newest, actor, 20);
        assert!(with_tag.sql().as_str().contains("g.tag ILIKE"));
    }

    #[test]
    fn game_filter_adds_association_exists_clause() {
        let mut query = empty_discover_query();
        query.game = Some(Uuid::new_v4());
        let actor = Uuid::new_v4();
        let builder = build_discover_query(&query, DiscoverSort::Newest, actor, 20);
        assert!(builder
            .sql()
            .as_str()
            .contains("EXISTS (SELECT 1 FROM guild_game_associations"));
    }

    #[test]
    fn sort_selects_expected_order_by_clause() {
        let actor = Uuid::new_v4();
        let query = empty_discover_query();

        let newest = build_discover_query(&query, DiscoverSort::Newest, actor, 20);
        assert!(newest
            .sql()
            .as_str()
            .contains("ORDER BY g.created_at DESC, g.id DESC"));

        let alpha = build_discover_query(&query, DiscoverSort::Alphabetical, actor, 20);
        assert!(alpha
            .sql()
            .as_str()
            .contains("ORDER BY g.name ASC, g.id ASC"));

        let most_members = build_discover_query(&query, DiscoverSort::MostMembers, actor, 20);
        assert!(most_members
            .sql()
            .as_str()
            .contains("ORDER BY member_count DESC, g.id DESC"));
    }

    #[test]
    fn cursor_adds_keyset_pagination_clause_matching_the_active_sort() {
        let mut query = empty_discover_query();
        query.cursor = Some(Uuid::new_v4());
        let actor = Uuid::new_v4();

        let newest = build_discover_query(&query, DiscoverSort::Newest, actor, 20);
        assert!(newest
            .sql()
            .as_str()
            .contains("(g.created_at, g.id) < (SELECT created_at, id FROM guilds WHERE id ="));

        let alpha = build_discover_query(&query, DiscoverSort::Alphabetical, actor, 20);
        assert!(alpha
            .sql()
            .as_str()
            .contains("(g.name, g.id) > (SELECT name, id FROM guilds WHERE id ="));
    }

    #[test]
    fn no_cursor_means_no_keyset_pagination_clause() {
        let query = empty_discover_query();
        let actor = Uuid::new_v4();
        let builder = build_discover_query(&query, DiscoverSort::Newest, actor, 20);
        assert!(!builder.sql().as_str().contains("WHERE id ="));
    }
}
