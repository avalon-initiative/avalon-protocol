//! Guild events calendar + RSVP (issue #169) — deliberately NOT protocol
//! history, same posture as `crate::guild_messages`. See
//! `docs/architecture/guilds-implementation-log.md`'s "Guild events
//! calendar + RSVP" section for the durability call, and "`event_manage`
//! (issue #250)" for the per-resource-override authorization model.

use avalon_protocol::guilds::{GuildPermission, GuildResourceKind, RsvpStatus};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::channels::{guild_owner, require_member};
use crate::error::AppError;
use crate::guilds::{
    actor_role_permissions as actor_permissions, has_guild_permission, has_resource_permission,
};
use crate::handlers::authenticate;
use crate::state::AppState;

const EVENT_TITLE_MAX_CHARS: usize = 200;
const EVENT_DESCRIPTION_MAX_CHARS: usize = 4000;

/// Guild-wide `event_manage` check, used only where there's no event yet
/// to scope a resource-aware check to (creation). See module doc comment.
async fn require_manage_events(
    state: &AppState,
    guild_id: Uuid,
    actor: Uuid,
) -> Result<(), AppError> {
    let owner = guild_owner(state, guild_id).await?;
    let permissions = actor_permissions(state, guild_id, actor).await?;
    if has_guild_permission(owner, actor, &permissions, GuildPermission::EventManage) {
        Ok(())
    } else {
        Err(AppError::MissingGuildPermission)
    }
}

/// Resource-aware `event_manage` check against one specific event — used
/// by `update_event`/`delete_event`, which already have an `event_id` to
/// scope a per-resource override to (issue #250).
async fn require_manage_event_resource(
    state: &AppState,
    guild_id: Uuid,
    event_id: Uuid,
    actor: Uuid,
) -> Result<(), AppError> {
    let owner = guild_owner(state, guild_id).await?;
    let allowed = has_resource_permission(
        state,
        guild_id,
        owner,
        actor,
        GuildResourceKind::Event,
        event_id,
        GuildPermission::EventManage,
    )
    .await?;
    if allowed {
        Ok(())
    } else {
        Err(AppError::MissingGuildPermission)
    }
}

fn validate_title(title: &str) -> Result<(), AppError> {
    let trimmed = title.trim();
    if trimmed.is_empty() || trimmed.chars().count() > EVENT_TITLE_MAX_CHARS {
        return Err(AppError::InvalidEventTitle);
    }
    Ok(())
}

fn validate_description(description: &Option<String>) -> Result<(), AppError> {
    match description {
        Some(d) if d.chars().count() > EVENT_DESCRIPTION_MAX_CHARS => {
            Err(AppError::InvalidEventDescription)
        }
        _ => Ok(()),
    }
}

/// `ends_at`, if present, must not be before `starts_at`.
fn validate_time_range(
    starts_at: OffsetDateTime,
    ends_at: Option<OffsetDateTime>,
) -> Result<(), AppError> {
    if let Some(ends_at) = ends_at {
        if ends_at < starts_at {
            return Err(AppError::InvalidEventTimeRange);
        }
    }
    Ok(())
}

pub(crate) struct EventRow {
    pub id: Uuid,
    pub guild_id: Uuid,
    pub channel_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub starts_at: OffsetDateTime,
    pub ends_at: Option<OffsetDateTime>,
    pub created_by: Uuid,
    pub created_at: OffsetDateTime,
    pub public: bool,
}

/// `pub(crate)` re-export of [`fetch_event`]'s existence check, for
/// `crate::guilds::require_live_resource` (issue #250) — a permission
/// override can only be written against a live event, same "resource
/// must currently exist" rule channel overrides get via
/// `crate::channels::fetch_channel`.
pub(crate) async fn fetch_event_for_override_check(
    state: &AppState,
    guild_id: Uuid,
    event_id: Uuid,
) -> Result<(), AppError> {
    fetch_event(state, guild_id, event_id).await?;
    Ok(())
}

async fn fetch_event(
    state: &AppState,
    guild_id: Uuid,
    event_id: Uuid,
) -> Result<EventRow, AppError> {
    let row = sqlx::query(
        r#"SELECT id, guild_id, channel_id, title, description, starts_at, ends_at,
         created_by, created_at, "public" FROM guild_events WHERE id = $1 AND guild_id = $2"#,
    )
    .bind(event_id)
    .bind(guild_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::GuildEventNotFound)?;
    Ok(EventRow {
        id: row.try_get("id")?,
        guild_id: row.try_get("guild_id")?,
        channel_id: row.try_get("channel_id")?,
        title: row.try_get("title")?,
        description: row.try_get("description")?,
        starts_at: row.try_get("starts_at")?,
        ends_at: row.try_get("ends_at")?,
        created_by: row.try_get("created_by")?,
        created_at: row.try_get("created_at")?,
        public: row.try_get("public")?,
    })
}

#[derive(Serialize)]
pub struct RsvpCounts {
    pub going: i64,
    pub maybe: i64,
    pub not_going: i64,
}

#[derive(Serialize)]
pub struct EventResponse {
    pub id: Uuid,
    pub guild_id: Uuid,
    pub channel_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub starts_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub ends_at: Option<OffsetDateTime>,
    pub created_by: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub rsvp_counts: RsvpCounts,
    /// Issue #448. `false` (the default) keeps this event member-only even
    /// in a [`crate::guilds::GuildResponse::public`] guild.
    pub public: bool,
    /// Issue #463. The caller's own RSVP status for this event, or `None`
    /// if they haven't RSVP'd — never another identity's. Lets a client
    /// pre-select `AvalonRsvpControl` correctly instead of always
    /// rendering unset, even after the caller has already responded.
    pub my_rsvp: Option<String>,
    /// Issue #458. `false` when the caller has `view` but not
    /// `view_details` on this event: the event's existence is visible
    /// (`id`/`guild_id`/`title`/`starts_at`/`ends_at`/`created_by`/
    /// `created_at`/`public` are real), but `channel_id`/`description`/
    /// `rsvp_counts`/`my_rsvp` are placeholder values, not real data —
    /// never a 403, since existence itself is meant to stay visible.
    /// Always `true` for every event this module's other endpoints
    /// (create/update/RSVP) return, since those all require the actor to
    /// already hold `event_manage` or be RSVPing to their own record.
    pub details_visible: bool,
}

/// The caller's own `guild_event_rsvps` row for `event_id`, or `None` if
/// they haven't RSVP'd — self-only, same posture [`upsert_rsvp`] takes.
/// Read back as the raw stored string rather than re-parsed through
/// [`RsvpStatus`]: every write path already validates it, so a read
/// failure here would mean data corruption, not a client error — same
/// "never a hard failure on a read path" precedent `guilds::row_badge`
/// documents for role badges.
async fn my_rsvp(
    state: &AppState,
    event_id: Uuid,
    actor: Uuid,
) -> Result<Option<String>, AppError> {
    let status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM guild_event_rsvps WHERE event_id = $1 AND identity_id = $2",
    )
    .bind(event_id)
    .bind(actor)
    .fetch_optional(&state.pool)
    .await?;
    Ok(status)
}

async fn rsvp_counts(state: &AppState, event_id: Uuid) -> Result<RsvpCounts, AppError> {
    let rows = sqlx::query(
        "SELECT status, COUNT(*) AS n FROM guild_event_rsvps WHERE event_id = $1 GROUP BY status",
    )
    .bind(event_id)
    .fetch_all(&state.pool)
    .await?;
    let mut counts = RsvpCounts {
        going: 0,
        maybe: 0,
        not_going: 0,
    };
    for row in rows {
        let status: String = row.try_get("status")?;
        let n: i64 = row.try_get("n")?;
        match RsvpStatus::parse(&status) {
            Some(RsvpStatus::Going) => counts.going = n,
            Some(RsvpStatus::Maybe) => counts.maybe = n,
            Some(RsvpStatus::NotGoing) => counts.not_going = n,
            None => {}
        }
    }
    Ok(counts)
}

/// `view_details = false` skips the RSVP-data queries entirely (there's
/// nothing to strip after the fact if it was never fetched) and returns
/// existence-only fields — see [`EventResponse::details_visible`].
async fn event_response(
    state: &AppState,
    row: EventRow,
    actor: Uuid,
    view_details: bool,
) -> Result<EventResponse, AppError> {
    if !view_details {
        return Ok(EventResponse {
            id: row.id,
            guild_id: row.guild_id,
            channel_id: None,
            title: row.title,
            description: None,
            starts_at: row.starts_at,
            ends_at: row.ends_at,
            created_by: row.created_by,
            created_at: row.created_at,
            rsvp_counts: RsvpCounts {
                going: 0,
                maybe: 0,
                not_going: 0,
            },
            public: row.public,
            my_rsvp: None,
            details_visible: false,
        });
    }
    let rsvp_counts = rsvp_counts(state, row.id).await?;
    let my_rsvp = my_rsvp(state, row.id, actor).await?;
    Ok(EventResponse {
        id: row.id,
        guild_id: row.guild_id,
        channel_id: row.channel_id,
        title: row.title,
        description: row.description,
        starts_at: row.starts_at,
        ends_at: row.ends_at,
        created_by: row.created_by,
        created_at: row.created_at,
        rsvp_counts,
        public: row.public,
        my_rsvp,
        details_visible: true,
    })
}

#[derive(Deserialize)]
pub struct ListEventsQuery {
    /// Inclusive lower bound on `starts_at`.
    #[serde(default)]
    #[serde(with = "time::serde::rfc3339::option")]
    pub from: Option<OffsetDateTime>,
    /// Inclusive upper bound on `starts_at`.
    #[serde(default)]
    #[serde(with = "time::serde::rfc3339::option")]
    pub to: Option<OffsetDateTime>,
}

/// `GET /guilds/{id}/events?from=&to=` — current members see every event.
/// A non-member of a [`crate::guilds::GuildResponse::public`] guild (issue
/// #448) sees only `public` events instead of being 403'd outright — the
/// same "guild-level flag widens exposure of an otherwise-gated resource"
/// shape `list_members`'s roster override already established for #449,
/// scoped per-event here since (unlike a roster) some events genuinely
/// need to stay internal even in a public guild. A non-member of a
/// non-public guild is still 403'd, unchanged. Optionally filtered to a
/// `starts_at` date range; omitted bounds are unbounded.
pub async fn list_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
    Query(query): Query<ListEventsQuery>,
) -> Result<Json<Vec<EventResponse>>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let is_member = crate::channels::is_guild_member(&state, guild_id, actor).await?;
    if !is_member && !crate::guilds::is_guild_public(&state, guild_id).await? {
        return Err(AppError::NotGuildMember);
    }
    let owner = crate::channels::guild_owner(&state, guild_id).await?;

    let rows = sqlx::query(
        r#"SELECT id, guild_id, channel_id, title, description, starts_at, ends_at,
         created_by, created_at, "public" FROM guild_events
         WHERE guild_id = $1
         AND ($2::timestamptz IS NULL OR starts_at >= $2)
         AND ($3::timestamptz IS NULL OR starts_at <= $3)
         AND ($4 OR "public" = true)
         ORDER BY starts_at"#,
    )
    .bind(guild_id)
    .bind(query.from)
    .bind(query.to)
    .bind(is_member)
    .fetch_all(&state.pool)
    .await?;

    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        let event = EventRow {
            id: row.try_get("id")?,
            guild_id: row.try_get("guild_id")?,
            channel_id: row.try_get("channel_id")?,
            title: row.try_get("title")?,
            description: row.try_get("description")?,
            starts_at: row.try_get("starts_at")?,
            ends_at: row.try_get("ends_at")?,
            created_by: row.try_get("created_by")?,
            created_at: row.try_get("created_at")?,
            public: row.try_get("public")?,
        };
        // Issue #458: the guild-level member/public gate above already
        // decided whether this event was fetched from the DB at all
        // (unchanged); this layers the finer-grained role-override system
        // on top — a member denied `view` on this specific event never
        // sees it, and one denied only `view_details` sees it with its
        // content stripped.
        let can_view = crate::guilds::has_view_permission(
            &state,
            guild_id,
            owner,
            actor,
            avalon_protocol::guilds::GuildResourceKind::Event,
            event.id,
            event.public,
            false,
        )
        .await?;
        if !can_view {
            continue;
        }
        let view_details = crate::guilds::has_view_permission(
            &state,
            guild_id,
            owner,
            actor,
            avalon_protocol::guilds::GuildResourceKind::Event,
            event.id,
            event.public,
            true,
        )
        .await?;
        events.push(event_response(&state, event, actor, view_details).await?);
    }
    Ok(Json(events))
}

#[derive(Deserialize)]
pub struct CreateEventRequest {
    pub channel_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub starts_at: OffsetDateTime,
    #[serde(default)]
    #[serde(with = "time::serde::rfc3339::option")]
    pub ends_at: Option<OffsetDateTime>,
    /// Issue #448. Omitted defaults to `false` — member-only, same as
    /// every event before this field existed. Gated by the same
    /// `event_manage` check as the rest of this request, no new
    /// permission needed.
    #[serde(default)]
    pub public: bool,
}

/// `POST /guilds/{id}/events` — requires `event_manage`. No outbox
/// write — see module doc comment.
pub async fn create_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
    Json(body): Json<CreateEventRequest>,
) -> Result<Json<EventResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    validate_title(&body.title)?;
    validate_description(&body.description)?;
    validate_time_range(body.starts_at, body.ends_at)?;
    require_manage_events(&state, guild_id, actor).await?;

    if let Some(channel_id) = body.channel_id {
        // Reuses channels' own fetch, which 404s if the channel doesn't
        // belong to this guild.
        crate::channels::fetch_channel(&state, guild_id, channel_id).await?;
    }

    let title = body.title.trim().to_string();
    let event_id = Uuid::new_v4();
    let created_at = OffsetDateTime::now_utc();
    sqlx::query(
        r#"INSERT INTO guild_events (id, guild_id, channel_id, title, description, starts_at,
         ends_at, created_by, created_at, "public") VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"#,
    )
    .bind(event_id)
    .bind(guild_id)
    .bind(body.channel_id)
    .bind(&title)
    .bind(&body.description)
    .bind(body.starts_at)
    .bind(body.ends_at)
    .bind(actor)
    .bind(created_at)
    .bind(body.public)
    .execute(&state.pool)
    .await?;

    event_response(
        &state,
        EventRow {
            id: event_id,
            guild_id,
            channel_id: body.channel_id,
            title,
            description: body.description,
            starts_at: body.starts_at,
            ends_at: body.ends_at,
            created_by: actor,
            created_at,
            public: body.public,
        },
        actor,
        true,
    )
    .await
    .map(Json)
}

#[derive(Deserialize)]
pub struct UpdateEventRequest {
    pub channel_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub starts_at: OffsetDateTime,
    #[serde(default)]
    #[serde(with = "time::serde::rfc3339::option")]
    pub ends_at: Option<OffsetDateTime>,
    /// Issue #448. Full replace like the rest of this request — always
    /// resent, not three-state.
    #[serde(default)]
    pub public: bool,
}

/// `PATCH /guilds/{id}/events/{eid}` — reschedule/edit. Requires
/// `event_manage` (resource-aware, issue #250). Full replace of the mutable fields, same "resend the
/// whole thing" convention other guild PATCH endpoints use.
pub async fn update_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, event_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateEventRequest>,
) -> Result<Json<EventResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    validate_title(&body.title)?;
    validate_description(&body.description)?;
    validate_time_range(body.starts_at, body.ends_at)?;
    let existing = fetch_event(&state, guild_id, event_id).await?;
    require_manage_event_resource(&state, guild_id, event_id, actor).await?;

    if let Some(channel_id) = body.channel_id {
        crate::channels::fetch_channel(&state, guild_id, channel_id).await?;
    }

    let title = body.title.trim().to_string();
    sqlx::query(
        r#"UPDATE guild_events SET channel_id = $3, title = $4, description = $5,
         starts_at = $6, ends_at = $7, "public" = $8 WHERE id = $1 AND guild_id = $2"#,
    )
    .bind(event_id)
    .bind(guild_id)
    .bind(body.channel_id)
    .bind(&title)
    .bind(&body.description)
    .bind(body.starts_at)
    .bind(body.ends_at)
    .bind(body.public)
    .execute(&state.pool)
    .await?;

    event_response(
        &state,
        EventRow {
            id: event_id,
            guild_id,
            channel_id: body.channel_id,
            title,
            description: body.description,
            starts_at: body.starts_at,
            ends_at: body.ends_at,
            created_by: existing.created_by,
            created_at: existing.created_at,
            public: body.public,
        },
        actor,
        true,
    )
    .await
    .map(Json)
}

/// `DELETE /guilds/{id}/events/{eid}` — requires `event_manage` (resource-
/// aware, issue #250). A real hard delete: events aren't history (see
/// module doc comment). Removes its RSVPs too, via the `ON DELETE CASCADE`
/// FK on `guild_event_rsvps` (migration 0028) — nothing app-level to do
/// here beyond deleting the event row itself.
pub async fn delete_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, event_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    fetch_event(&state, guild_id, event_id).await?;
    require_manage_event_resource(&state, guild_id, event_id, actor).await?;

    let deleted = sqlx::query("DELETE FROM guild_events WHERE id = $1 AND guild_id = $2")
        .bind(event_id)
        .bind(guild_id)
        .execute(&state.pool)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(AppError::GuildEventNotFound);
    }

    Ok(Json(serde_json::json!({ "deleted": true })))
}

#[derive(Deserialize)]
pub struct RsvpRequest {
    pub status: String,
}

#[derive(Serialize)]
pub struct RsvpResponse {
    pub event_id: Uuid,
    pub identity_id: Uuid,
    pub status: String,
    #[serde(with = "time::serde::rfc3339")]
    pub responded_at: OffsetDateTime,
}

/// `PUT /guilds/{id}/events/{eid}/rsvp` — requires current guild
/// membership. Self-service only: always upserts the caller's own row,
/// there is no way to target another identity's RSVP through this route.
/// Idempotent per (event, identity): a second call with a new status
/// replaces the row in place, never inserts a duplicate — enforced by the
/// `(event_id, identity_id)` primary key from migration 0028 plus
/// `ON CONFLICT DO UPDATE` below.
pub async fn upsert_rsvp(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, event_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<RsvpRequest>,
) -> Result<Json<RsvpResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let status = RsvpStatus::parse(&body.status).ok_or(AppError::InvalidRsvpStatus)?;
    require_member(&state, guild_id, actor).await?;
    fetch_event(&state, guild_id, event_id).await?;

    let responded_at = OffsetDateTime::now_utc();
    sqlx::query(
        "INSERT INTO guild_event_rsvps (event_id, identity_id, status, responded_at) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (event_id, identity_id) \
         DO UPDATE SET status = EXCLUDED.status, responded_at = EXCLUDED.responded_at",
    )
    .bind(event_id)
    .bind(actor)
    .bind(status.as_str())
    .bind(responded_at)
    .execute(&state.pool)
    .await?;

    Ok(Json(RsvpResponse {
        event_id,
        identity_id: actor,
        status: status.as_str().to_string(),
        responded_at,
    }))
}

#[derive(Serialize)]
pub struct RsvpRosterEntry {
    pub identity_id: Uuid,
    pub status: String,
    #[serde(with = "time::serde::rfc3339")]
    pub responded_at: OffsetDateTime,
}

/// `GET /guilds/{id}/events/{eid}/rsvps` — any current guild member.
/// Returns every `guild_event_rsvps` row for the event (`identity_id`,
/// `status`, `responded_at`), unaggregated — the per-member roster behind
/// `rsvp_counts`. Same membership gate as `list_events`/`rsvp_counts`'s
/// query, no `manage_*` permission required: RSVP status is ordinary
/// guild-internal social info, same posture the member roster already
/// takes (see module doc comment).
pub async fn list_rsvps(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, event_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Vec<RsvpRosterEntry>>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    require_member(&state, guild_id, actor).await?;
    fetch_event(&state, guild_id, event_id).await?;

    let rows = sqlx::query(
        "SELECT identity_id, status, responded_at FROM guild_event_rsvps \
         WHERE event_id = $1 ORDER BY responded_at",
    )
    .bind(event_id)
    .fetch_all(&state.pool)
    .await?;

    let mut roster = Vec::with_capacity(rows.len());
    for row in rows {
        roster.push(RsvpRosterEntry {
            identity_id: row.try_get("identity_id")?,
            status: row.try_get("status")?,
            responded_at: row.try_get("responded_at")?,
        });
    }
    Ok(Json(roster))
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — pure-logic checks only. The
    //! endpoint-level flows (membership gating, RSVP idempotency, delete
    //! cascading to RSVPs) are covered by
    //! `crates/server/tests/guild_events.rs`, gated `--ignored`.

    use super::*;

    #[test]
    fn validate_title_rejects_empty_and_overlong() {
        assert!(validate_title("Raid night").is_ok());
        assert!(validate_title("   ").is_err());
        assert!(validate_title(&"a".repeat(EVENT_TITLE_MAX_CHARS + 1)).is_err());
        assert!(validate_title(&"a".repeat(EVENT_TITLE_MAX_CHARS)).is_ok());
    }

    #[test]
    fn validate_description_rejects_overlong_but_allows_none() {
        assert!(validate_description(&None).is_ok());
        assert!(validate_description(&Some("short".to_string())).is_ok());
        assert!(validate_description(&Some("a".repeat(EVENT_DESCRIPTION_MAX_CHARS + 1))).is_err());
    }

    #[test]
    fn validate_time_range_rejects_end_before_start() {
        let start = OffsetDateTime::now_utc();
        let before = start - time::Duration::seconds(1);
        let after = start + time::Duration::seconds(1);
        assert!(validate_time_range(start, None).is_ok());
        assert!(validate_time_range(start, Some(after)).is_ok());
        assert!(validate_time_range(start, Some(start)).is_ok());
        assert!(validate_time_range(start, Some(before)).is_err());
    }

    #[test]
    fn rsvp_status_round_trips_through_as_str_and_parse() {
        for status in RsvpStatus::ALL {
            assert_eq!(RsvpStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(RsvpStatus::parse("not-a-status"), None);
    }
}
