//! Guild events calendar + RSVP (issue #169) — deliberately NOT protocol
//! history, same posture as `crate::guild_messages`.
//!
//! `GuildEvent`/`GuildEventRsvp` rows (`crates/protocol/src/guilds.rs`)
//! live in `guild_events`/`guild_event_rsvps` and never touch the ledger
//! commit path or the outbox module — no `guild.event_*` protocol event
//! kind exists anywhere, on purpose. This module never imports the outbox
//! or the chain crate.
//!
//! **Durability call, made explicitly (not silently assumed) per the
//! ticket:** a scheduled event is closer to `guild_messages` than to
//! `guild_channels`. A channel is durable *structure* — `crate::channels`
//! writes a `guild.channel_*` event on create/rename/archive because a
//! channel is something worth reconstructing history for. A guild event is
//! not: a raid night that gets rescheduled three times and cancelled isn't
//! history worth preserving the way membership or channel structure is —
//! it's closer to "hot state" like presence or chat. So both the event row
//! *and* its RSVPs are plain projections here, unlike channels (durable
//! structure) vs. messages (ephemeral content) which split that
//! distinction within a single feature. See
//! `crates/server/db/migrations/0028_guild_events/up.sql` and
//! `docs/architecture/guilds.md` for the same call stated for readers of
//! the migration and the architecture doc respectively.
//!
//! **Membership.** Reading, creating, or RSVPing to events requires the
//! caller to currently be a member of the guild — `crate::channels::
//! require_member` (issue #21's real `guild_members` table).
//!
//! **Authorization.** `manage_channels` gates create/update/delete,
//! reusing the fixed milestone-1 `GuildPermission` set from issue #20
//! rather than inventing a new "manage_events" permission — that set is
//! explicitly not-yet-extensible for milestone 1 (see
//! `crates/protocol/src/guilds.rs::GuildPermission`), and event scheduling
//! is the same kind of "structural guild content" moderation channel
//! management already covers. RSVPing is self-service: any current member
//! may set or change their own RSVP, never anyone else's.

use avalon_protocol::guilds::{GuildPermission, RsvpStatus};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::channels::{guild_owner, require_member};
use crate::error::AppError;
use crate::guilds::{actor_role_permissions as actor_permissions, has_guild_permission};
use crate::handlers::authenticate;
use crate::state::AppState;

const EVENT_TITLE_MAX_CHARS: usize = 200;
const EVENT_DESCRIPTION_MAX_CHARS: usize = 4000;

async fn require_manage_events(
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
}

async fn fetch_event(
    state: &AppState,
    guild_id: Uuid,
    event_id: Uuid,
) -> Result<EventRow, AppError> {
    let row = sqlx::query(
        "SELECT id, guild_id, channel_id, title, description, starts_at, ends_at, \
         created_by, created_at FROM guild_events WHERE id = $1 AND guild_id = $2",
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

async fn event_response(state: &AppState, row: EventRow) -> Result<EventResponse, AppError> {
    let rsvp_counts = rsvp_counts(state, row.id).await?;
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

/// `GET /guilds/{id}/events?from=&to=` — current members only. Optionally
/// filtered to a `starts_at` date range; omitted bounds are unbounded.
pub async fn list_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<Uuid>,
    Query(query): Query<ListEventsQuery>,
) -> Result<Json<Vec<EventResponse>>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    require_member(&state, guild_id, actor).await?;

    let rows = sqlx::query(
        "SELECT id, guild_id, channel_id, title, description, starts_at, ends_at, \
         created_by, created_at FROM guild_events \
         WHERE guild_id = $1 \
         AND ($2::timestamptz IS NULL OR starts_at >= $2) \
         AND ($3::timestamptz IS NULL OR starts_at <= $3) \
         ORDER BY starts_at",
    )
    .bind(guild_id)
    .bind(query.from)
    .bind(query.to)
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
        };
        events.push(event_response(&state, event).await?);
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
}

/// `POST /guilds/{id}/events` — requires `manage_channels`. No outbox
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
        "INSERT INTO guild_events (id, guild_id, channel_id, title, description, starts_at, \
         ends_at, created_by, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
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
        },
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
}

/// `PATCH /guilds/{id}/events/{eid}` — reschedule/edit. Requires
/// `manage_channels`. Full replace of the mutable fields, same "resend the
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
    require_manage_events(&state, guild_id, actor).await?;

    if let Some(channel_id) = body.channel_id {
        crate::channels::fetch_channel(&state, guild_id, channel_id).await?;
    }

    let title = body.title.trim().to_string();
    sqlx::query(
        "UPDATE guild_events SET channel_id = $3, title = $4, description = $5, \
         starts_at = $6, ends_at = $7 WHERE id = $1 AND guild_id = $2",
    )
    .bind(event_id)
    .bind(guild_id)
    .bind(body.channel_id)
    .bind(&title)
    .bind(&body.description)
    .bind(body.starts_at)
    .bind(body.ends_at)
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
        },
    )
    .await
    .map(Json)
}

/// `DELETE /guilds/{id}/events/{eid}` — requires `manage_channels`. A real
/// hard delete: events aren't history (see module doc comment). Removes
/// its RSVPs too, via the `ON DELETE CASCADE` FK on `guild_event_rsvps`
/// (migration 0028) — nothing app-level to do here beyond deleting the
/// event row itself.
pub async fn delete_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, event_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    fetch_event(&state, guild_id, event_id).await?;
    require_manage_events(&state, guild_id, actor).await?;

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
