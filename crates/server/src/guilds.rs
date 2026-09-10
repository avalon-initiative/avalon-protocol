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
    GuildLink, GuildPermission, GuildResourceKind, JoinPolicy, RoleBadge, RoleBadgeColor,
    RoleBadgeIcon,
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

/// Cap on `Guild.icon`'s URL length (issue #246) — a small badge image is
/// no more likely to need a longer URL than a banner, so this uses the
/// same bound as [`MAX_GUILD_BANNER_URL_LEN`], as its own named constant
/// rather than reusing the banner's, in case the two ever need to diverge.
const MAX_GUILD_ICON_URL_LEN: usize = 2048;

/// Cap on the number of entries in `Guild.links` (issue #153) — keeps this
/// from becoming an arbitrary free-form content field, per the ticket.
const MAX_GUILD_LINKS: usize = 5;

/// Cap on a single `GuildLink.label`'s length (issue #153).
const MAX_GUILD_LINK_LABEL_LEN: usize = 60;

/// Cap on a single `GuildLink.url`'s length (issue #153) — same bound
/// `MAX_AVATAR_URL_LEN`/`MAX_GUILD_BANNER_URL_LEN` use.
const MAX_GUILD_LINK_URL_LEN: usize = 2048;

/// Cap on the number of entries in a guild's curated favorite-games pin
/// list (issue #207) — same "top N, not a free-form list" shape
/// [`MAX_GUILD_LINKS`] already uses, capped at a smaller number since this
/// is meant to be a deliberately curated highlight, not a catalog.
const MAX_GUILD_FAVORITE_GAMES: usize = 5;

/// Default/maximum page size for `GET /guilds/discover` (issue #154) — same
/// "small default, capped maximum" shape `guild_messages`'s
/// `DEFAULT_MESSAGE_PAGE_SIZE`/`MAX_MESSAGE_PAGE_SIZE` already use.
const DEFAULT_DISCOVER_PAGE_SIZE: i64 = 20;
const MAX_DISCOVER_PAGE_SIZE: i64 = 100;

/// Cap on `GuildJoinRequest.message` (issue #242) — a short free-text note
/// from the applicant, not an essay; same order of magnitude as
/// `MAX_ROLE_DESCRIPTION_LEN`.
const MAX_JOIN_REQUEST_MESSAGE_LEN: usize = 300;

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

/// Pure resolution of a resource-scoped permission check (issue #250,
/// shape decided by #243): owner always passes, same as
/// [`has_guild_permission`]. Otherwise, when an override exists for this
/// exact (role, resource, permission) triple it decides the outcome
/// outright — `Some(true)` grants even if the base list lacks the
/// permission, `Some(false)` denies even if the base list has it. With no
/// override (`None`), falls back to the role's flat base
/// `actor_permissions` list, same as [`has_guild_permission`]. Factored
/// out as a pure function (no DB access) so every combination of
/// owner/override/base state is unit-testable without a live Postgres —
/// see the tests below.
pub(crate) fn resolve_resource_permission(
    is_owner: bool,
    base_permissions: &[String],
    override_allow: Option<bool>,
    permission: GuildPermission,
) -> bool {
    if is_owner {
        return true;
    }
    match override_allow {
        Some(allow) => allow,
        None => base_permissions.iter().any(|p| p == permission.as_str()),
    }
}

/// `actor`'s current `(role_index, base permissions)` in `guild_id`, or
/// `None` if they aren't currently a member. Sibling of
/// [`actor_role_permissions`] that also returns the role index overrides
/// are keyed on — [`has_resource_permission`] needs both.
pub(crate) async fn actor_role(
    state: &AppState,
    guild_id: Uuid,
    actor: Uuid,
) -> Result<Option<(i32, Vec<String>)>, AppError> {
    let row = sqlx::query(
        r#"
        SELECT gr.name_index, gr.permissions FROM guild_members gm
        JOIN guild_roles gr ON gr.guild_id = gm.guild_id AND gr.name_index = gm.role_index
        WHERE gm.guild_id = $1 AND gm.identity_id = $2
        "#,
    )
    .bind(guild_id)
    .bind(actor)
    .fetch_optional(&state.pool)
    .await?;
    match row {
        Some(row) => Ok(Some((
            row.try_get("name_index")?,
            row.try_get("permissions")?,
        ))),
        None => Ok(None),
    }
}

/// The override row's `allow` value for an exact (role, resource,
/// permission) triple, or `None` if no override exists there — the
/// "inert" case from the ticket's invariants also covers a since-deleted
/// resource, since every caller of [`has_resource_permission`] already
/// fetches (and 404s on) the resource before ever reaching this.
async fn fetch_override(
    state: &AppState,
    guild_id: Uuid,
    role_index: i32,
    resource_kind: GuildResourceKind,
    resource_id: Uuid,
    permission: GuildPermission,
) -> Result<Option<bool>, AppError> {
    let row = sqlx::query(
        "SELECT allow FROM guild_permission_overrides \
         WHERE guild_id = $1 AND role_index = $2 AND resource_kind = $3 \
         AND resource_id = $4 AND permission = $5",
    )
    .bind(guild_id)
    .bind(role_index)
    .bind(resource_kind.as_str())
    .bind(resource_id)
    .bind(permission.as_str())
    .fetch_optional(&state.pool)
    .await?;
    match row {
        Some(row) => Ok(Some(row.try_get("allow")?)),
        None => Ok(None),
    }
}

/// Resource-aware sibling of [`has_guild_permission`] (issue #250): same
/// owner bypass, but for a non-owner actor it consults
/// `guild_permission_overrides` for this exact resource before falling
/// back to the role's flat base permission list (via
/// [`resolve_resource_permission`]). An actor with no current guild
/// membership (no role) is never granted anything, same "no role, no
/// permission" posture the flat check has via an empty `actor_permissions`
/// slice. `pub(crate)` for the same cross-module reuse
/// [`has_guild_permission`] already has (`channels.rs`, `guild_events.rs`,
/// `guild_messages.rs`).
pub(crate) async fn has_resource_permission(
    state: &AppState,
    guild_id: Uuid,
    guild_owner: Uuid,
    actor: Uuid,
    resource_kind: GuildResourceKind,
    resource_id: Uuid,
    permission: GuildPermission,
) -> Result<bool, AppError> {
    if actor == guild_owner {
        return Ok(true);
    }
    let Some((role_index, base_permissions)) = actor_role(state, guild_id, actor).await? else {
        return Ok(false);
    };
    let override_allow = fetch_override(
        state,
        guild_id,
        role_index,
        resource_kind,
        resource_id,
        permission,
    )
    .await?;
    Ok(resolve_resource_permission(
        false,
        &base_permissions,
        override_allow,
        permission,
    ))
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
            // `event_manage` added alongside `manage_channels` by #250 —
            // before the split, officers could manage events purely
            // because event endpoints piggybacked on `manage_channels`;
            // this keeps that same authority explicit now that
            // `event_manage` is its own permission.
            &["manage_members", "manage_channels", "event_manage"],
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

/// Same empty-string-clears convention `validate_guild_banner` uses,
/// reusing the same `http`/`https`-URL validation — mirrors it exactly,
/// just for `Guild.icon` (issue #246) rather than `Guild.banner`.
fn validate_guild_icon(icon: &str) -> Result<Option<String>, AppError> {
    if icon.is_empty() {
        return Ok(None);
    }
    if !is_http_url(icon, MAX_GUILD_ICON_URL_LEN) {
        return Err(AppError::InvalidGuildIcon);
    }
    Ok(Some(icon.to_string()))
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
    /// Issue #246.
    icon: Option<String>,
    /// Issue #153.
    links: Vec<GuildLink>,
    /// Issue #153.
    recruiting: bool,
    /// Issue #206 — whether the game affinity breakdown (see
    /// [`game_breakdown`]) is shown on this guild's public profile /
    /// discovery card. Always visible to a `manage_guild` holder
    /// regardless of this flag; it only gates *public* exposure.
    game_breakdown_public: bool,
}

async fn fetch_guild(state: &AppState, guild_id: Uuid) -> Result<GuildRow, AppError> {
    let row = sqlx::query(
        "SELECT id, name, tag, description, owner, created_at, join_policy, motd, banner, icon, links, recruiting, game_breakdown_public FROM guilds WHERE id = $1",
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
        icon: row.try_get("icon")?,
        links,
        recruiting: row.try_get("recruiting")?,
        game_breakdown_public: row.try_get("game_breakdown_public")?,
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
    /// Issue #246.
    pub icon: Option<String>,
    /// Issue #153.
    pub links: Vec<GuildLink>,
    /// Issue #153.
    pub recruiting: bool,
    /// Issue #206. Whether the game affinity breakdown
    /// (`GET /guilds/{id}/game-breakdown`) is shown on this guild's public
    /// profile — a `manage_guild` holder can always fetch the breakdown
    /// regardless of this flag; it only gates exposure to everyone else.
    pub game_breakdown_public: bool,
    /// Issue #207. The guild's curated top-5 favorite games, in display
    /// order, each flagged `stale` if it no longer has an actively-bound
    /// member. Unlike `game_breakdown_public`'s full breakdown, this
    /// curated subset is always part of the guild's public profile — it's
    /// the guild's own deliberate choice of what to show, same "always
    /// public" treatment `links`/`motd` already get.
    pub favorite_games: Vec<FavoriteGameEntry>,
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

    let favorite_games = fetch_favorite_games(state, guild.id).await?;

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
        icon: guild.icon,
        links: guild.links,
        recruiting: guild.recruiting,
        game_breakdown_public: guild.game_breakdown_public,
        favorite_games,
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
                icon: None,
                links: Vec::new(),
                recruiting: false,
                game_breakdown_public: false,
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
    /// Issue #246. Same three-state convention as `banner`, same
    /// `http`/`https`-URL validation.
    pub icon: Option<String>,
    /// Issue #153. Two states, not three: omitted (untouched) or
    /// `Some(list)`, which always fully replaces the stored list —
    /// including `Some(vec![])` to clear it. Each entry is validated; an
    /// invalid entry rejects the whole request rather than being dropped.
    pub links: Option<Vec<GuildLinkRequest>>,
    /// Issue #153. Omitted leaves it untouched.
    pub recruiting: Option<bool>,
    /// "invite_only" or "open" (see [`JoinPolicy`]) — omitted leaves it
    /// untouched. `Open` lets any authenticated identity join instantly via
    /// `POST /guilds/{id}/join` (`can_join_directly`/`join_guild`), bypassing
    /// the invite (#21) and join-request/approval (#242) flows entirely.
    pub join_policy: Option<String>,
    /// Issue #206. Omitted leaves it untouched. Controls only whether the
    /// game affinity breakdown is shown on this guild's *public* profile —
    /// a `manage_guild` holder can always see it internally either way.
    pub game_breakdown_public: Option<bool>,
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
    let new_icon = match &body.icon {
        Some(raw) => validate_guild_icon(raw)?,
        None => guild.icon.clone(),
    };
    let new_links = match &body.links {
        Some(links) => validate_guild_links(links)?,
        None => guild.links.clone(),
    };
    let new_recruiting = body.recruiting.unwrap_or(guild.recruiting);
    let new_join_policy = match &body.join_policy {
        Some(raw) => JoinPolicy::parse(raw).ok_or(AppError::InvalidJoinPolicy)?,
        None => guild.join_policy,
    };
    let new_game_breakdown_public = body
        .game_breakdown_public
        .unwrap_or(guild.game_breakdown_public);
    let new_links_json =
        serde_json::to_value(&new_links).expect("GuildLink always serializes to JSON");

    let mut tx = state.pool.begin().await?;

    let updated = sqlx::query(
        "UPDATE guilds SET name = $2, tag = $3, description = $4, motd = $5, banner = $6, icon = $7, links = $8, recruiting = $9, game_breakdown_public = $10, join_policy = $11 WHERE id = $1",
    )
    .bind(guild_id)
    .bind(&new_name)
    .bind(&new_tag)
    .bind(&new_description)
    .bind(&new_motd)
    .bind(&new_banner)
    .bind(&new_icon)
    .bind(&new_links_json)
    .bind(new_recruiting)
    .bind(new_game_breakdown_public)
    .bind(new_join_policy.as_str())
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
            "icon": new_icon,
            "links": new_links,
            "recruiting": new_recruiting,
            "game_breakdown_public": new_game_breakdown_public,
            "join_policy": new_join_policy.as_str(),
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
                join_policy: new_join_policy,
                motd: new_motd,
                banner: new_banner,
                icon: new_icon,
                links: new_links,
                recruiting: new_recruiting,
                game_breakdown_public: new_game_breakdown_public,
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

    let inserted = sqlx::query(
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
    .await;
    if let Err(sqlx::Error::Database(db_err)) = &inserted {
        if db_err.is_unique_violation() {
            return Err(AppError::GuildRoleNameTaken);
        }
    }
    inserted?;

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
    // row's permission list (see `has_guild_permission`) — changing its
    // permissions here would look meaningful but change nothing about who
    // can actually act as owner, which is exactly the kind of silent-no-op
    // footgun this repo avoids. Its name/description/badge are harmless,
    // cosmetic-only fields with no bearing on authority, so those stay
    // editable — only a `permissions` change on this specific role index
    // is rejected.
    if name_index == OWNER_ROLE_INDEX && body.permissions.is_some() {
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
    .await;
    if let Err(sqlx::Error::Database(db_err)) = &updated {
        if db_err.is_unique_violation() {
            return Err(AppError::GuildRoleNameTaken);
        }
    }
    let updated = updated?;
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

/// The owner role's authority comes from `guilds.owner`, and the member
/// role is the hardcoded fallback every `add_member` call (invite accept,
/// join-request approval, direct open-guild join) assigns — deleting
/// either would either be a meaningless no-op (owner) or strand every one
/// of those code paths (member). Any other role, including the seeded
/// "Officer" default, is deletable.
fn is_base_role(name_index: i32) -> bool {
    name_index == OWNER_ROLE_INDEX || name_index == MEMBER_ROLE_INDEX
}

pub async fn delete_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, name_index)): Path<(Uuid, i32)>,
) -> Result<(), AppError> {
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

    if is_base_role(name_index) {
        return Err(AppError::CannotDeleteBaseRole);
    }

    let mut tx = state.pool.begin().await?;

    // `guild_members(guild_id, role_index)` has a (deliberately ON DELETE
    // RESTRICT, i.e. plain) foreign key into this table — a member still
    // holding this role blocks the delete at the database level rather
    // than needing an app-side existence check first (and a second
    // round trip) that could race a concurrent role change.
    let deleted = sqlx::query("DELETE FROM guild_roles WHERE guild_id = $1 AND name_index = $2")
        .bind(guild_id)
        .bind(name_index)
        .execute(&mut *tx)
        .await;
    if let Err(sqlx::Error::Database(db_err)) = &deleted {
        if db_err.is_foreign_key_violation() {
            return Err(AppError::RoleHasMembers);
        }
    }
    let deleted = deleted?;
    if deleted.rows_affected() == 0 {
        return Err(AppError::GuildRoleNotFound);
    }

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "guild.role_deleted".to_string(),
        issuer: identity_ref(actor, "guild_role_deleted"),
        subject: guild_ref(guild_id, "guild_role_deleted"),
        payload: serde_json::json!({
            "guild_id": guild_id,
            "name_index": name_index,
            "actor": actor,
        }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(())
}

// --- Per-resource permission overrides (issue #250) --------------------
//
// Not durable/outbox history — same "hot, editable configuration, not an
// append-only fact" posture `guild_roles.permissions` itself already has
// (role permission edits don't get their own history either, only
// `guild.role_defined` snapshots). Every route below requires
// `manage_roles` guild-wide: setting a per-resource override is role
// management, the same authority tier as editing a role's base
// permission list.

#[derive(Serialize)]
pub struct PermissionOverrideResponse {
    pub id: Uuid,
    pub role_index: i32,
    pub resource_kind: String,
    pub resource_id: Uuid,
    pub permission: String,
    pub allow: bool,
}

async fn require_manage_roles(
    state: &AppState,
    guild: &GuildRow,
    actor: Uuid,
) -> Result<(), AppError> {
    let actor_permissions = actor_role_permissions(state, guild.id, actor).await?;
    if has_guild_permission(
        guild.owner,
        actor,
        &actor_permissions,
        GuildPermission::ManageRoles,
    ) {
        Ok(())
    } else {
        Err(AppError::MissingGuildPermission)
    }
}

/// 404s if the resource doesn't exist (or doesn't belong to this guild) —
/// overrides may only be written against a live channel/event, per the
/// ticket's "inert, not an error" invariant applying to *reads* of a
/// since-deleted resource, not to writing a fresh override against one
/// that never existed.
async fn require_live_resource(
    state: &AppState,
    guild_id: Uuid,
    resource_kind: GuildResourceKind,
    resource_id: Uuid,
) -> Result<(), AppError> {
    match resource_kind {
        GuildResourceKind::Channel => {
            crate::channels::fetch_channel(state, guild_id, resource_id).await?;
        }
        GuildResourceKind::Event => {
            crate::guild_events::fetch_event_for_override_check(state, guild_id, resource_id)
                .await?;
        }
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct SetPermissionOverrideRequest {
    pub role_index: i32,
    pub resource_kind: String,
    pub resource_id: Uuid,
    pub permission: String,
    pub allow: bool,
}

/// `PUT /guilds/{id}/permission-overrides` — set (upsert) a grant/deny
/// override for one role on one resource. Requires `manage_roles`.
pub async fn set_permission_override(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
    Json(body): Json<SetPermissionOverrideRequest>,
) -> Result<Json<PermissionOverrideResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let guild = fetch_guild(&state, guild_id).await?;
    require_manage_roles(&state, &guild, actor).await?;

    // 404s if the role doesn't exist in this guild.
    fetch_role(&state, guild_id, body.role_index).await?;
    let resource_kind =
        GuildResourceKind::parse(&body.resource_kind).ok_or(AppError::InvalidResourceKind)?;
    let permission = GuildPermission::parse(&body.permission).ok_or(AppError::InvalidPermission)?;
    require_live_resource(&state, guild_id, resource_kind, body.resource_id).await?;

    let row = sqlx::query(
        "INSERT INTO guild_permission_overrides \
         (guild_id, role_index, resource_kind, resource_id, permission, allow) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (guild_id, role_index, resource_kind, resource_id, permission) \
         DO UPDATE SET allow = EXCLUDED.allow \
         RETURNING id",
    )
    .bind(guild_id)
    .bind(body.role_index)
    .bind(resource_kind.as_str())
    .bind(body.resource_id)
    .bind(permission.as_str())
    .bind(body.allow)
    .fetch_one(&state.pool)
    .await?;

    Ok(Json(PermissionOverrideResponse {
        id: row.try_get("id")?,
        role_index: body.role_index,
        resource_kind: resource_kind.as_str().to_string(),
        resource_id: body.resource_id,
        permission: permission.as_str().to_string(),
        allow: body.allow,
    }))
}

#[derive(Deserialize)]
pub struct ListPermissionOverridesQuery {
    pub resource_kind: String,
    pub resource_id: Uuid,
}

/// `GET /guilds/{id}/permission-overrides?resource_kind=&resource_id=` —
/// every role's override rows for one resource. Requires `manage_roles`.
pub async fn list_permission_overrides(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
    Query(query): Query<ListPermissionOverridesQuery>,
) -> Result<Json<Vec<PermissionOverrideResponse>>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let guild = fetch_guild(&state, guild_id).await?;
    require_manage_roles(&state, &guild, actor).await?;

    let resource_kind =
        GuildResourceKind::parse(&query.resource_kind).ok_or(AppError::InvalidResourceKind)?;

    let rows = sqlx::query(
        "SELECT id, role_index, resource_kind, resource_id, permission, allow \
         FROM guild_permission_overrides \
         WHERE guild_id = $1 AND resource_kind = $2 AND resource_id = $3 \
         ORDER BY role_index",
    )
    .bind(guild_id)
    .bind(resource_kind.as_str())
    .bind(query.resource_id)
    .fetch_all(&state.pool)
    .await?;

    let mut overrides = Vec::with_capacity(rows.len());
    for row in rows {
        overrides.push(PermissionOverrideResponse {
            id: row.try_get("id")?,
            role_index: row.try_get("role_index")?,
            resource_kind: row.try_get("resource_kind")?,
            resource_id: row.try_get("resource_id")?,
            permission: row.try_get("permission")?,
            allow: row.try_get("allow")?,
        });
    }
    Ok(Json(overrides))
}

/// `DELETE /guilds/{id}/permission-overrides/{override_id}` — clears an
/// override, reverting that (role, resource, permission) triple back to
/// the role's base permission list. Requires `manage_roles`.
pub async fn delete_permission_override(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, override_id)): Path<(Uuid, Uuid)>,
) -> Result<(), AppError> {
    let actor = authenticate(&state, &headers).await?;
    let guild = fetch_guild(&state, guild_id).await?;
    require_manage_roles(&state, &guild, actor).await?;

    let deleted =
        sqlx::query("DELETE FROM guild_permission_overrides WHERE id = $1 AND guild_id = $2")
            .bind(override_id)
            .bind(guild_id)
            .execute(&state.pool)
            .await?;
    if deleted.rows_affected() == 0 {
        return Err(AppError::PermissionOverrideNotFound);
    }
    Ok(())
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
                icon: guild.icon,
                links: guild.links,
                recruiting: guild.recruiting,
                game_breakdown_public: guild.game_breakdown_public,
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

/// Inserts `identity_id` into `guild_members` at `role_index` and enqueues
/// the durable `guild.member_added` event, in the given transaction. The
/// one membership-add code path, shared by [`accept_invite`],
/// [`join_guild`], and [`approve_join_request`] (issue #242) so approving a
/// join request can't drift from what invites/direct-join already do.
async fn add_member(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    guild_id: Uuid,
    identity_id: Uuid,
    role_index: i32,
    actor: Uuid,
    via: &str,
) -> Result<OffsetDateTime, AppError> {
    let joined_at = OffsetDateTime::now_utc();
    let inserted = sqlx::query(
        "INSERT INTO guild_members (guild_id, identity_id, role_index, joined_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(guild_id)
    .bind(identity_id)
    .bind(role_index)
    .bind(joined_at)
    .execute(&mut **tx)
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
            "identity_id": identity_id,
            "role_index": role_index,
            "via": via,
            "actor": actor,
        }),
        timestamp: joined_at,
        version: 1,
    };
    outbox::enqueue(tx, &event).await?;

    Ok(joined_at)
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

    let joined_at =
        add_member(&mut tx, guild_id, actor, MEMBER_ROLE_INDEX, actor, "invite").await?;

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

    let joined_at = add_member(&mut tx, guild_id, actor, MEMBER_ROLE_INDEX, actor, "join").await?;

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

// --- Join requests (issue #242) --------------------------------------------
//
// Symmetric to `guild_invites` above but initiated by the applicant instead
// of a manager: a stranger browsing the Discover board (#154) applies to a
// `recruiting` guild instead of waiting to be invited. `guild_join_requests`
// is a projection, same durability posture as `guild_invites` — a
// pending/approved/rejected/withdrawn transition is not itself durable
// history; only the resulting membership-add (via [`add_member`], on
// approval) is.

fn validate_join_request_message(message: &str) -> Result<(), AppError> {
    if message.chars().count() > MAX_JOIN_REQUEST_MESSAGE_LEN {
        return Err(AppError::InvalidJoinRequestMessage);
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct CreateJoinRequestRequest {
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Serialize)]
pub struct GuildJoinRequestResponse {
    pub id: Uuid,
    pub guild_id: Uuid,
    pub applicant: Uuid,
    pub message: Option<String>,
    pub status: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub decided_at: Option<OffsetDateTime>,
    pub decided_by: Option<Uuid>,
}

fn join_request_response(
    row: &sqlx::postgres::PgRow,
) -> Result<GuildJoinRequestResponse, AppError> {
    Ok(GuildJoinRequestResponse {
        id: row.try_get("id")?,
        guild_id: row.try_get("guild_id")?,
        applicant: row.try_get("applicant")?,
        message: row.try_get("message")?,
        status: row.try_get("status")?,
        created_at: row.try_get("created_at")?,
        decided_at: row.try_get("decided_at")?,
        decided_by: row.try_get("decided_by")?,
    })
}

/// `POST /guilds/{id}/join-requests` — any authenticated identity not
/// already a member may apply to a `recruiting` guild. Applying to a
/// non-recruiting guild is rejected, same gating #154's own discovery board
/// applies to strangers browsing it. A second apply while one is already
/// pending is idempotent (returns the existing pending row) rather than an
/// error or a duplicate, same posture [`create_invite`] takes for a
/// duplicate invite.
pub async fn create_join_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
    Json(body): Json<CreateJoinRequestRequest>,
) -> Result<Json<GuildJoinRequestResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let guild = fetch_guild(&state, guild_id).await?;

    if !guild.recruiting {
        return Err(AppError::GuildNotRecruiting);
    }

    if let Some(message) = &body.message {
        validate_join_request_message(message)?;
    }

    let already_member =
        sqlx::query("SELECT 1 FROM guild_members WHERE guild_id = $1 AND identity_id = $2")
            .bind(guild_id)
            .bind(actor)
            .fetch_optional(&state.pool)
            .await?;
    if already_member.is_some() {
        return Err(AppError::AlreadyGuildMember);
    }

    let request_id = Uuid::new_v4();
    let created_at = OffsetDateTime::now_utc();
    let inserted = sqlx::query(
        "INSERT INTO guild_join_requests (id, guild_id, applicant, message, status, created_at) \
         VALUES ($1, $2, $3, $4, 'pending', $5)",
    )
    .bind(request_id)
    .bind(guild_id)
    .bind(actor)
    .bind(&body.message)
    .bind(created_at)
    .execute(&state.pool)
    .await;
    if let Err(sqlx::Error::Database(db_err)) = &inserted {
        if db_err.is_unique_violation() {
            // A pending request from this applicant already exists —
            // idempotent by design (ticket: "duplicate apply idempotent"),
            // so return the existing row rather than erroring or
            // duplicating it.
            let existing = sqlx::query(
                "SELECT id, guild_id, applicant, message, status, created_at, decided_at, decided_by \
                 FROM guild_join_requests WHERE guild_id = $1 AND applicant = $2 AND status = 'pending'",
            )
            .bind(guild_id)
            .bind(actor)
            .fetch_one(&state.pool)
            .await?;
            return Ok(Json(join_request_response(&existing)?));
        }
    }
    inserted?;

    Ok(Json(GuildJoinRequestResponse {
        id: request_id,
        guild_id,
        applicant: actor,
        message: body.message,
        status: "pending".to_string(),
        created_at,
        decided_at: None,
        decided_by: None,
    }))
}

#[derive(Deserialize)]
pub struct ListJoinRequestsQuery {
    /// Defaults to `pending`-only; pass `all` to include every status.
    #[serde(default)]
    pub status: Option<String>,
}

/// `GET /guilds/{id}/join-requests` — `manage_members`-gated. Lists
/// pending requests by default (`?status=all` for every status).
pub async fn list_join_requests(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
    Query(query): Query<ListJoinRequestsQuery>,
) -> Result<Json<Vec<GuildJoinRequestResponse>>, AppError> {
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

    let show_all = query.status.as_deref() == Some("all");
    let rows = if show_all {
        sqlx::query(
            "SELECT id, guild_id, applicant, message, status, created_at, decided_at, decided_by \
             FROM guild_join_requests WHERE guild_id = $1 ORDER BY created_at DESC",
        )
        .bind(guild_id)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query(
            "SELECT id, guild_id, applicant, message, status, created_at, decided_at, decided_by \
             FROM guild_join_requests WHERE guild_id = $1 AND status = 'pending' ORDER BY created_at ASC",
        )
        .bind(guild_id)
        .fetch_all(&state.pool)
        .await?
    };

    rows.iter()
        .map(join_request_response)
        .collect::<Result<Vec<_>, _>>()
        .map(Json)
}

/// `GET /guilds/{id}/join-requests/mine` — issue #256. Any authenticated
/// caller, no `manage_members` gate: this is the caller's own data, not a
/// moderation view, unlike [`list_join_requests`]. Returns the caller's own
/// pending join request for this guild if one exists, or `null` if it
/// doesn't — same `Json<Option<T>>` "single item belonging to the caller,
/// or none" shape `recovery::my_recovery_status` already established,
/// rather than a 404 for the "none" case.
pub async fn my_join_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
) -> Result<Json<Option<GuildJoinRequestResponse>>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    // 404s if the guild doesn't exist, same as GET /guilds/{id}.
    fetch_guild(&state, guild_id).await?;

    let row = sqlx::query(
        "SELECT id, guild_id, applicant, message, status, created_at, decided_at, decided_by \
         FROM guild_join_requests WHERE guild_id = $1 AND applicant = $2 AND status = 'pending'",
    )
    .bind(guild_id)
    .bind(actor)
    .fetch_optional(&state.pool)
    .await?;

    match row {
        Some(row) => Ok(Json(Some(join_request_response(&row)?))),
        None => Ok(Json(None)),
    }
}

struct PendingJoinRequest {
    applicant: Uuid,
}

async fn fetch_pending_join_request(
    state: &AppState,
    guild_id: Uuid,
    request_id: Uuid,
) -> Result<PendingJoinRequest, AppError> {
    let row = sqlx::query(
        "SELECT applicant FROM guild_join_requests WHERE id = $1 AND guild_id = $2 AND status = 'pending'",
    )
    .bind(request_id)
    .bind(guild_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::GuildJoinRequestNotFound)?;
    Ok(PendingJoinRequest {
        applicant: row.try_get("applicant")?,
    })
}

/// `POST /guilds/{id}/join-requests/{request_id}/approve` —
/// `manage_members`-gated. Adds the applicant as a member (base role)
/// through [`add_member`] — the same membership-add path [`accept_invite`]
/// and [`join_guild`] already use, not a second one — and marks the
/// request approved.
pub async fn approve_join_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, request_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<GuildMemberResponse>, AppError> {
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

    let request = fetch_pending_join_request(&state, guild_id, request_id).await?;

    let mut tx = state.pool.begin().await?;

    let resolved = sqlx::query(
        "UPDATE guild_join_requests SET status = 'approved', decided_at = now(), decided_by = $2 \
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(request_id)
    .bind(actor)
    .execute(&mut *tx)
    .await?;
    if resolved.rows_affected() == 0 {
        // Resolved by a concurrent request between the fetch above and here.
        return Err(AppError::GuildJoinRequestNotFound);
    }

    let joined_at = add_member(
        &mut tx,
        guild_id,
        request.applicant,
        MEMBER_ROLE_INDEX,
        actor,
        "join_request",
    )
    .await?;

    tx.commit().await?;

    Ok(Json(GuildMemberResponse {
        guild_id,
        identity_id: request.applicant,
        role_index: MEMBER_ROLE_INDEX,
        joined_at,
    }))
}

/// `POST /guilds/{id}/join-requests/{request_id}/reject` —
/// `manage_members`-gated. Not durable history — see this section's module
/// doc comment.
pub async fn reject_join_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, request_id)): Path<(Uuid, Uuid)>,
) -> Result<(), AppError> {
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

    fetch_pending_join_request(&state, guild_id, request_id).await?;

    let resolved = sqlx::query(
        "UPDATE guild_join_requests SET status = 'rejected', decided_at = now(), decided_by = $2 \
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(request_id)
    .bind(actor)
    .execute(&state.pool)
    .await?;
    if resolved.rows_affected() == 0 {
        return Err(AppError::GuildJoinRequestNotFound);
    }

    Ok(())
}

/// `DELETE /guilds/{id}/join-requests/{request_id}` — the applicant
/// withdrawing their own pending request only; unlike approve/reject this
/// is not `manage_members`-gated, same "consent from the other side" shape
/// as `decline_invite`, just from the opposite party.
pub async fn withdraw_join_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, request_id)): Path<(Uuid, Uuid)>,
) -> Result<(), AppError> {
    let actor = authenticate(&state, &headers).await?;
    let request = fetch_pending_join_request(&state, guild_id, request_id).await?;
    if actor != request.applicant {
        return Err(AppError::GuildJoinRequestNotFound);
    }

    let resolved = sqlx::query(
        "UPDATE guild_join_requests SET status = 'withdrawn', decided_at = now(), decided_by = $2 \
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(request_id)
    .bind(actor)
    .execute(&state.pool)
    .await?;
    if resolved.rows_affected() == 0 {
        return Err(AppError::GuildJoinRequestNotFound);
    }

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
/// wildcard behavior rather than being matched literally. `pub(crate)` since
/// `crates/server/src/discovery.rs`'s `GET /identities/search` (#205) reuses
/// it for the exact same reason rather than re-implementing the escape.
pub(crate) fn escape_like(input: &str) -> String {
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
    /// Issue #258: same already-public fields `GET /guilds/{id}` returns
    /// (#153/#246) — `null` when unset, no new visibility exposure.
    pub banner: Option<String>,
    pub icon: Option<String>,
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
/// Visibility rule for `recruiting`: a non-recruiting guild must never
/// appear in a stranger's browse/search results, in any filter combination
/// — only exact id/tag lookup (`GET /guilds/{id}`) reaches it, same as
/// before this endpoint existed.
///
/// - `recruiting=true` is a plain exact filter: recruiting guilds are
///   already public-by-design (#20), so no membership gate is needed.
/// - `recruiting` omitted: "recruiting guilds, plus any guild the caller is
///   already a member of regardless of its recruiting flag" — a member
///   always sees their own guilds' discovery card, same as
///   `GET /guilds/{id}`/`GET /me/guilds` already let them look it up
///   directly.
/// - `recruiting=false` explicitly: **still membership-gated**, not a raw
///   exact filter — it only returns the caller's own non-recruiting
///   guilds. Without this gate a stranger could pass `recruiting=false` to
///   bulk-enumerate every non-recruiting guild's public metadata, which is
///   exactly the "reachable only by exact id/tag" invariant this endpoint
///   must not violate.
///
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
         g.banner, g.icon, \
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
        Some(true) => {
            builder.push(" AND g.recruiting = true");
        }
        Some(false) => {
            builder.push(
                " AND g.recruiting = false AND g.id IN (SELECT guild_id FROM guild_members WHERE identity_id = ",
            );
            builder.push_bind(actor);
            builder.push(")");
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
            banner: row.try_get("banner")?,
            icon: row.try_get("icon")?,
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

// --- Game affinity breakdown (issue #206, implementing decision #160) -----

/// One game's slice of a guild's game affinity breakdown: how many of the
/// guild's current members hold an active [`GameBinding`](avalon_protocol::games::GameBinding)
/// to it. Never includes a game with zero bound members — there's no
/// "add" action here, only real binding data feeds this (see the module
/// doc comment and `docs/architecture/guilds.md`).
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct GameBreakdownEntry {
    pub game_id: Uuid,
    pub game_slug: String,
    pub game_name: String,
    /// Distinct guild members with an active binding to this game.
    pub member_count: i64,
}

#[derive(Debug, Serialize)]
pub struct GameBreakdownResponse {
    pub guild_id: Uuid,
    /// Total current guild membership — the denominator for a
    /// "N of M members play X" display. Not the same as summing
    /// `breakdown[].member_count`, since a member can be bound to zero,
    /// one, or several games.
    pub total_members: i64,
    /// No minimum-member threshold and no fixed cap — every game with at
    /// least one bound member appears, ordered by member count descending
    /// (ties broken alphabetically by name for a stable, readable order).
    /// This is a display of real counts, not a system verdict, per #160.
    pub breakdown: Vec<GameBreakdownEntry>,
}

/// True if `actor` (holding `actor_permissions` in a guild owned by
/// `guild_owner`) may view the game affinity breakdown: either they hold
/// `manage_guild` (or are the owner, via [`has_guild_permission`]'s
/// structural check) — the authority deciding whether to expose the
/// breakdown, who can always see it internally — or the guild has opted
/// into showing it on its public profile (`game_breakdown_public`), in
/// which case anyone (including a non-member) may view it. Pure and
/// unit-testable independent of any query.
fn can_view_game_breakdown(
    guild_owner: Uuid,
    actor: Uuid,
    actor_permissions: &[String],
    game_breakdown_public: bool,
) -> bool {
    game_breakdown_public
        || has_guild_permission(
            guild_owner,
            actor,
            actor_permissions,
            GuildPermission::ManageGuild,
        )
}

/// Builds the aggregation query behind [`game_breakdown`] — split out so
/// the shape of the query can be unit-tested via [`sqlx::QueryBuilder::sql`]
/// without a live Postgres connection, same pattern [`build_discover_query`]
/// already established for #154.
///
/// Groups the guild's current members (`guild_members`) by their active
/// `bindings` (`ended_at IS NULL`, issue #83), joined against `games` for
/// display name/slug. A member with no active binding to any game
/// contributes to no row; a member bound to several games contributes to
/// each. No `HAVING` / minimum-count filter — every game with at least one
/// bound member is included, per #160's "no minimum-member threshold"
/// invariant.
fn build_game_breakdown_query(guild_id: Uuid) -> QueryBuilder<Postgres> {
    let mut builder: QueryBuilder<Postgres> = QueryBuilder::new(
        "SELECT b.game_id, g.slug AS game_slug, g.name AS game_name, \
         COUNT(DISTINCT b.identity_id) AS member_count \
         FROM guild_members gm \
         JOIN bindings b ON b.identity_id = gm.identity_id AND b.ended_at IS NULL \
         JOIN games g ON g.id = b.game_id \
         WHERE gm.guild_id = ",
    );
    builder.push_bind(guild_id);
    builder.push(
        " GROUP BY b.game_id, g.slug, g.name \
         ORDER BY member_count DESC, g.name ASC",
    );
    builder
}

/// `GET /guilds/{id}/game-breakdown` (issue #206, implementing decision
/// #160). Milestone-1 stand-in: a direct query over `guild_members` JOIN
/// `bindings` JOIN `games`, same precedent [`discover_guilds`] (#154)
/// already set, not #42's real indexer read model. Derived/computed on
/// every read — no protocol event, no durable table backs this (see the
/// module doc comment).
///
/// Gated by [`can_view_game_breakdown`]: a `manage_guild` holder (or the
/// owner) can always see it; anyone else only when the guild has set
/// `game_breakdown_public`. A non-member with neither gets
/// [`AppError::MissingGuildPermission`], same 403 the rest of this module
/// already uses for "authenticated fine, just not authorized here".
pub async fn game_breakdown(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
) -> Result<Json<GameBreakdownResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let guild = fetch_guild(&state, guild_id).await?;

    let actor_permissions = actor_role_permissions(&state, guild_id, actor).await?;
    if !can_view_game_breakdown(
        guild.owner,
        actor,
        &actor_permissions,
        guild.game_breakdown_public,
    ) {
        return Err(AppError::MissingGuildPermission);
    }

    let total_members_row =
        sqlx::query("SELECT COUNT(*) AS count FROM guild_members WHERE guild_id = $1")
            .bind(guild_id)
            .fetch_one(&state.pool)
            .await?;
    let total_members: i64 = total_members_row.try_get("count")?;

    let mut builder = build_game_breakdown_query(guild_id);
    let rows = builder.build().fetch_all(&state.pool).await?;
    let mut breakdown = Vec::with_capacity(rows.len());
    for row in rows {
        breakdown.push(GameBreakdownEntry {
            game_id: row.try_get("game_id")?,
            game_slug: row.try_get("game_slug")?,
            game_name: row.try_get("game_name")?,
            member_count: row.try_get("member_count")?,
        });
    }

    Ok(Json(GameBreakdownResponse {
        guild_id,
        total_members,
        breakdown,
    }))
}

// --- Favorite games: curated top-5 pin list (issue #207, implementing -----
// --- decision #160) --------------------------------------------------------

/// Every game the guild currently has a real affinity for, per #206's
/// aggregation (`build_game_breakdown_query`): at least one current member
/// holds an active binding to it. This is the *only* source of truth a pin
/// may be validated against — reused as-is (not a separate query) so
/// "pinnable" can never drift from "what the breakdown itself would show".
async fn guild_bound_game_ids(
    state: &AppState,
    guild_id: Uuid,
) -> Result<std::collections::HashSet<Uuid>, AppError> {
    let mut builder = build_game_breakdown_query(guild_id);
    let rows = builder.build().fetch_all(&state.pool).await?;
    let mut ids = std::collections::HashSet::with_capacity(rows.len());
    for row in rows {
        ids.insert(row.try_get::<Uuid, _>("game_id")?);
    }
    Ok(ids)
}

/// One entry in a guild's favorite-games pin list, as read back.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct FavoriteGameEntry {
    pub game_id: Uuid,
    pub game_slug: String,
    pub game_name: String,
    /// 0-indexed display order — the guild's curated ranking, not a
    /// popularity/member-count sort.
    pub position: i16,
    /// True when this game no longer has any actively-bound guild member
    /// (per [`guild_bound_game_ids`]) — its last bound member left/unbound
    /// since the pin was added. Per #207's design, a stale pin is never
    /// auto-removed (that would churn the guild's public display on a
    /// single member's binding change); it's surfaced here so a
    /// `manage_guild` holder can choose to unpin it.
    pub stale: bool,
}

#[derive(Debug, Serialize)]
pub struct FavoriteGamesResponse {
    pub guild_id: Uuid,
    pub favorites: Vec<FavoriteGameEntry>,
}

/// Shared by `GET /guilds/{id}/favorite-games` and [`guild_response`] (the
/// list embedded in `GET /guilds/{id}`) so both read paths compute
/// staleness identically, against the same live data.
async fn fetch_favorite_games(
    state: &AppState,
    guild_id: Uuid,
) -> Result<Vec<FavoriteGameEntry>, AppError> {
    let rows = sqlx::query(
        "SELECT gfg.game_id, gfg.position, g.slug AS game_slug, g.name AS game_name \
         FROM guild_favorite_games gfg \
         JOIN games g ON g.id = gfg.game_id \
         WHERE gfg.guild_id = $1 \
         ORDER BY gfg.position ASC",
    )
    .bind(guild_id)
    .fetch_all(&state.pool)
    .await?;

    let bound_ids = guild_bound_game_ids(state, guild_id).await?;

    let mut favorites = Vec::with_capacity(rows.len());
    for row in rows {
        let game_id: Uuid = row.try_get("game_id")?;
        favorites.push(FavoriteGameEntry {
            game_id,
            game_slug: row.try_get("game_slug")?,
            game_name: row.try_get("game_name")?,
            position: row.try_get("position")?,
            stale: !bound_ids.contains(&game_id),
        });
    }
    Ok(favorites)
}

/// `GET /guilds/{id}/favorite-games` (issue #207). Same "any authenticated
/// identity may read a guild's public metadata" visibility as `GET
/// /guilds/{id}` itself (see that handler's doc comment) — the favorites
/// list is exactly the curated subset of affinity data a guild has chosen
/// to put on public display, so it carries no additional gate beyond
/// session authentication.
pub async fn list_favorite_games(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
) -> Result<Json<FavoriteGamesResponse>, AppError> {
    authenticate(&state, &headers).await?;
    // 404s on a missing guild rather than returning an empty list.
    fetch_guild(&state, guild_id).await?;
    let favorites = fetch_favorite_games(&state, guild_id).await?;
    Ok(Json(FavoriteGamesResponse {
        guild_id,
        favorites,
    }))
}

#[derive(Deserialize)]
pub struct SetFavoriteGamesRequest {
    /// The full desired ordered list of pinned game ids — always a full
    /// replace, never a per-entry patch, same "resend the whole list"
    /// convention `UpdateGuildRequest::links` already established for #153.
    /// Position in this array is the new display order.
    pub game_ids: Vec<Uuid>,
}

/// [`MAX_GUILD_FAVORITE_GAMES`]-capped, order-preserving, no duplicates, and
/// every entry must currently appear in `bound_ids` (#206's live affinity
/// breakdown) — the one invariant this ticket exists to enforce: a pin can
/// never manufacture an association with a game the guild has no real,
/// currently-bound connection to.
fn validate_favorite_game_ids(
    game_ids: &[Uuid],
    bound_ids: &std::collections::HashSet<Uuid>,
) -> Result<(), AppError> {
    if game_ids.len() > MAX_GUILD_FAVORITE_GAMES {
        return Err(AppError::TooManyFavoriteGames);
    }
    let mut seen = std::collections::HashSet::with_capacity(game_ids.len());
    for game_id in game_ids {
        if !seen.insert(*game_id) {
            return Err(AppError::DuplicateFavoriteGame);
        }
        if !bound_ids.contains(game_id) {
            return Err(AppError::FavoriteGameNotBound);
        }
    }
    Ok(())
}

/// `PUT /guilds/{id}/favorite-games` (issue #207). Gated by the same
/// `manage_guild`/owner permission as #206's breakdown-visibility toggle
/// (via [`has_guild_permission`]) — reuses that check rather than inventing
/// a new one, per the ticket. Validates every id against the guild's real,
/// current affinity (see [`validate_favorite_game_ids`]) before writing
/// anything; on success, replaces the stored list atomically (delete +
/// reinsert, same "small enough this doesn't need per-row diffing" call
/// `update_guild`'s `links` replace already makes) and records a
/// `guild.favorite_games_updated` outbox event, matching every other guild
/// mutation in this module.
pub async fn set_favorite_games(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
    Json(body): Json<SetFavoriteGamesRequest>,
) -> Result<Json<FavoriteGamesResponse>, AppError> {
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

    let bound_ids = guild_bound_game_ids(&state, guild_id).await?;
    validate_favorite_game_ids(&body.game_ids, &bound_ids)?;

    let mut tx = state.pool.begin().await?;

    sqlx::query("DELETE FROM guild_favorite_games WHERE guild_id = $1")
        .bind(guild_id)
        .execute(&mut *tx)
        .await?;
    for (position, game_id) in body.game_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO guild_favorite_games (guild_id, game_id, position) VALUES ($1, $2, $3)",
        )
        .bind(guild_id)
        .bind(game_id)
        .bind(position as i16)
        .execute(&mut *tx)
        .await?;
    }

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "guild.favorite_games_updated".to_string(),
        issuer: identity_ref(actor, "guild_favorite_games_updated"),
        subject: guild_ref(guild_id, "guild_favorite_games_updated"),
        payload: serde_json::json!({
            "guild_id": guild_id,
            "game_ids": body.game_ids,
            "actor": actor,
        }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    let favorites = fetch_favorite_games(&state, guild_id).await?;
    Ok(Json(FavoriteGamesResponse {
        guild_id,
        favorites,
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

    // --- Issue #246: guild icon ---

    #[test]
    fn empty_icon_clears_to_none() {
        assert_eq!(validate_guild_icon("").unwrap(), None);
    }

    #[test]
    fn valid_https_icon_is_accepted() {
        assert_eq!(
            validate_guild_icon("https://example.com/icon.png").unwrap(),
            Some("https://example.com/icon.png".to_string())
        );
    }

    #[test]
    fn non_http_scheme_icon_is_rejected() {
        assert!(matches!(
            validate_guild_icon("javascript:alert(1)"),
            Err(AppError::InvalidGuildIcon)
        ));
    }

    #[test]
    fn overlong_icon_is_rejected() {
        let overlong = format!("https://example.com/{}", "a".repeat(MAX_GUILD_ICON_URL_LEN));
        assert!(matches!(
            validate_guild_icon(&overlong),
            Err(AppError::InvalidGuildIcon)
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
    fn explicit_recruiting_true_filters_exactly_no_membership_gate_needed() {
        // Recruiting guilds are already public-by-design (#20), so this is a
        // plain exact filter with no membership subquery.
        let mut query = empty_discover_query();
        query.recruiting = Some(true);
        let actor = Uuid::new_v4();
        let builder = build_discover_query(&query, DiscoverSort::Newest, actor, 20);
        let sql = builder.sql();
        let sql = sql.as_str();
        assert!(sql.contains("g.recruiting = true"));
        assert!(!sql.contains("g.id IN (SELECT guild_id FROM guild_members"));
    }

    /// `recruiting=false` must NOT be a raw exact filter — that would let a
    /// stranger bulk-enumerate every non-recruiting guild's public metadata,
    /// violating #154's "non-recruiting guilds are reachable only by exact
    /// id/tag" invariant. It has to stay membership-gated: only the caller's
    /// own non-recruiting guilds come back.
    #[test]
    fn explicit_recruiting_false_stays_membership_gated_not_a_bulk_leak() {
        let mut query = empty_discover_query();
        query.recruiting = Some(false);
        let actor = Uuid::new_v4();
        let builder = build_discover_query(&query, DiscoverSort::Newest, actor, 20);
        let sql = builder.sql();
        let sql = sql.as_str();
        assert!(sql.contains("g.recruiting = false"));
        assert!(sql.contains("g.id IN (SELECT guild_id FROM guild_members WHERE identity_id = "));
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

    /// Issue #258: `banner`/`icon` are already-public fields on a guild
    /// (readable via `GET /guilds/{id}` since #153/#246) — Discover's
    /// `SELECT` must include them too so `DiscoverGuildSummary` can carry
    /// them without a second round trip.
    #[test]
    fn select_list_includes_banner_and_icon() {
        let actor = Uuid::new_v4();
        let builder =
            build_discover_query(&empty_discover_query(), DiscoverSort::Newest, actor, 20);
        let sql = builder.sql();
        let sql = sql.as_str();
        assert!(sql.contains("g.banner"));
        assert!(sql.contains("g.icon"));
    }

    // --- Issue #206: game affinity breakdown -------------------------------

    #[test]
    fn breakdown_query_aggregates_over_active_bindings_only_no_minimum_threshold() {
        let guild_id = Uuid::new_v4();
        let builder = build_game_breakdown_query(guild_id);
        let sql_owned = builder.sql();
        let sql = sql_owned.as_str();
        assert!(sql
            .contains("JOIN bindings b ON b.identity_id = gm.identity_id AND b.ended_at IS NULL"));
        assert!(sql.contains("JOIN games g ON g.id = b.game_id"));
        assert!(sql.contains("COUNT(DISTINCT b.identity_id) AS member_count"));
        assert!(sql.contains("WHERE gm.guild_id ="));
        assert!(sql.contains("GROUP BY b.game_id, g.slug, g.name"));
        // No #160 "minimum member count" gate — every game with at least
        // one bound member appears, so there must be no HAVING clause.
        assert!(!sql.contains("HAVING"));
    }

    #[test]
    fn breakdown_query_orders_by_member_count_descending() {
        let builder = build_game_breakdown_query(Uuid::new_v4());
        assert!(builder
            .sql()
            .as_str()
            .contains("ORDER BY member_count DESC, g.name ASC"));
    }

    #[test]
    fn owner_can_always_view_breakdown_even_when_not_public() {
        let owner = Uuid::new_v4();
        assert!(can_view_game_breakdown(owner, owner, &[], false));
    }

    #[test]
    fn manage_guild_holder_can_view_breakdown_even_when_not_public() {
        let owner = Uuid::new_v4();
        let officer = Uuid::new_v4();
        assert!(can_view_game_breakdown(
            owner,
            officer,
            &["manage_guild".to_string()],
            false
        ));
    }

    #[test]
    fn plain_member_without_manage_guild_cannot_view_a_non_public_breakdown() {
        let owner = Uuid::new_v4();
        let member = Uuid::new_v4();
        assert!(!can_view_game_breakdown(
            owner,
            member,
            &["manage_members".to_string()],
            false
        ));
    }

    #[test]
    fn a_non_member_can_view_the_breakdown_once_the_guild_makes_it_public() {
        let owner = Uuid::new_v4();
        let stranger = Uuid::new_v4();
        assert!(can_view_game_breakdown(owner, stranger, &[], true));
    }

    // --- Issue #207: favorite games pin list --------------------------------

    #[test]
    fn a_sixth_pin_is_rejected() {
        let bound: std::collections::HashSet<Uuid> = (0..MAX_GUILD_FAVORITE_GAMES + 1)
            .map(|_| Uuid::new_v4())
            .collect();
        let game_ids: Vec<Uuid> = bound.iter().copied().collect();
        assert_eq!(game_ids.len(), MAX_GUILD_FAVORITE_GAMES + 1);
        assert!(matches!(
            validate_favorite_game_ids(&game_ids, &bound),
            Err(AppError::TooManyFavoriteGames)
        ));
    }

    #[test]
    fn exactly_five_pins_is_allowed() {
        let bound: std::collections::HashSet<Uuid> = (0..MAX_GUILD_FAVORITE_GAMES)
            .map(|_| Uuid::new_v4())
            .collect();
        let game_ids: Vec<Uuid> = bound.iter().copied().collect();
        assert_eq!(game_ids.len(), MAX_GUILD_FAVORITE_GAMES);
        assert!(validate_favorite_game_ids(&game_ids, &bound).is_ok());
    }

    #[test]
    fn pinning_a_game_with_zero_bound_members_is_rejected() {
        let bound: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
        let unbound_game = Uuid::new_v4();
        assert!(matches!(
            validate_favorite_game_ids(&[unbound_game], &bound),
            Err(AppError::FavoriteGameNotBound)
        ));
    }

    #[test]
    fn pinning_the_same_game_twice_is_rejected_as_duplicate() {
        let game_id = Uuid::new_v4();
        let bound: std::collections::HashSet<Uuid> = [game_id].into_iter().collect();
        assert!(matches!(
            validate_favorite_game_ids(&[game_id, game_id], &bound),
            Err(AppError::DuplicateFavoriteGame)
        ));
    }

    #[test]
    fn empty_pin_list_is_valid() {
        let bound: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
        assert!(validate_favorite_game_ids(&[], &bound).is_ok());
    }

    #[test]
    fn join_request_message_within_the_cap_is_valid() {
        assert!(validate_join_request_message("would love to join!").is_ok());
    }

    #[test]
    fn join_request_message_over_the_cap_is_rejected() {
        let too_long = "x".repeat(MAX_JOIN_REQUEST_MESSAGE_LEN + 1);
        assert!(matches!(
            validate_join_request_message(&too_long),
            Err(AppError::InvalidJoinRequestMessage)
        ));
    }

    // --- resolve_resource_permission (issue #250) -----------------------
    //
    // Pure-function coverage of every state the ticket's Tests section
    // calls out. `has_resource_permission`/`fetch_override` themselves
    // need a live Postgres (covered by `crates/server/tests/guilds.rs`,
    // gated `--ignored`) — this is the DB-free model of their exact
    // grant/deny/absent resolution logic.

    #[test]
    fn resource_permission_base_only_no_override() {
        // No override row (`None`) with the permission present in the
        // base list: allowed.
        assert!(resolve_resource_permission(
            false,
            &["channel_post".to_string()],
            None,
            GuildPermission::ChannelPost,
        ));
        // No override, not in the base list: denied.
        assert!(!resolve_resource_permission(
            false,
            &[],
            None,
            GuildPermission::ChannelPost,
        ));
    }

    #[test]
    fn resource_permission_explicit_grant_beats_base_absence() {
        // Base list lacks the permission entirely, but an override grants
        // it for this resource — grant wins.
        assert!(resolve_resource_permission(
            false,
            &[],
            Some(true),
            GuildPermission::ChannelPost,
        ));
    }

    #[test]
    fn resource_permission_explicit_deny_beats_base_grant() {
        // Base list has the permission, but an override denies it for
        // this resource — deny wins.
        assert!(!resolve_resource_permission(
            false,
            &["channel_post".to_string()],
            Some(false),
            GuildPermission::ChannelPost,
        ));
    }

    #[test]
    fn resource_permission_on_a_deleted_resource_is_inert() {
        // A since-deleted resource has no override row left to find —
        // `fetch_override` would return `None`, same as "no override was
        // ever set." Modeled here directly at the resolution layer: with
        // `None`, the outcome is exactly the base-only case, never an
        // error.
        assert!(resolve_resource_permission(
            false,
            &["channel_post".to_string()],
            None,
            GuildPermission::ChannelPost,
        ));
        assert!(!resolve_resource_permission(
            false,
            &[],
            None,
            GuildPermission::ChannelPost,
        ));
    }

    #[test]
    fn resource_permission_owner_bypass_survives_a_deny_override() {
        // The owner's structural bypass is untouched by any override,
        // deny included.
        assert!(resolve_resource_permission(
            true,
            &[],
            Some(false),
            GuildPermission::ChannelPost,
        ));
    }
}
