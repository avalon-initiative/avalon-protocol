//! Guild chat messages (issue #22) — deliberately NOT protocol history:
//! never touches the ledger/outbox, high-volume, non-interoperable. See
//! `docs/architecture/guilds.md` ("Guild chat is a network primitive",
//! "Today in the repo") and `docs/architecture/guilds-implementation-log.md`
//! for the archive-tier retention (#253), announcement-only channels
//! (#250), and moderation-deletion-vs-archive semantics.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::channels::{
    fetch_channel, guild_owner, require_manage_channel_resource, require_member,
};
use crate::error::AppError;
use crate::guilds::{has_resource_permission, has_view_permission};
use crate::handlers::authenticate;
use crate::state::AppState;
use avalon_protocol::guilds::{GuildPermission, GuildResourceKind};

const MESSAGE_BODY_MAX_CHARS: usize = 4000;
const DEFAULT_MESSAGE_PAGE_SIZE: i64 = 50;
const MAX_MESSAGE_PAGE_SIZE: i64 = 200;
const DEFAULT_MESSAGE_CAP: i64 = 10_000;
/// ~2 years. The "long retention window" #193 decided the archive tier
/// needs, distinct from (and much longer than) the live cap above.
const DEFAULT_ARCHIVE_RETENTION_DAYS: i64 = 730;
/// How often the background worker re-checks for archive rows past their
/// retention window. Coarse on purpose, same reasoning as
/// `crate::retention::PRUNE_INTERVAL` — this is a slow-moving maintenance
/// boundary, not a latency-sensitive path.
const ARCHIVE_EXPIRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Reads `GUILD_CHANNEL_MESSAGE_CAP`, otherwise `DEFAULT_MESSAGE_CAP`. Any
/// non-positive or unparseable value falls back to the default rather than
/// disabling pruning.
fn message_cap() -> i64 {
    std::env::var("GUILD_CHANNEL_MESSAGE_CAP")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MESSAGE_CAP)
}

/// Reads `GUILD_MESSAGE_ARCHIVE_RETENTION_DAYS`, otherwise
/// `DEFAULT_ARCHIVE_RETENTION_DAYS` — same non-positive/unparseable
/// fallback pattern as [`message_cap`], so a bad env value never silently
/// disables expiry (which would turn the archive into unbounded storage).
fn archive_retention_days() -> i64 {
    std::env::var("GUILD_MESSAGE_ARCHIVE_RETENTION_DAYS")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_ARCHIVE_RETENTION_DAYS)
}

/// `view_details` gate for reading a channel's message content (issue
/// #458) — the resource-aware sibling of `channels::require_member`, used
/// by both [`list_messages`] and [`list_archive`]. `channel_public` is
/// the caller's own already-fetched `guild_channels.public` for this
/// exact channel.
async fn require_channel_view_details(
    state: &AppState,
    guild_id: Uuid,
    channel_id: Uuid,
    channel_public: bool,
    actor: Uuid,
) -> Result<(), AppError> {
    let owner = guild_owner(state, guild_id).await?;
    let allowed = has_view_permission(
        state,
        guild_id,
        owner,
        actor,
        GuildResourceKind::Channel,
        channel_id,
        channel_public,
        true,
    )
    .await?;
    if allowed {
        Ok(())
    } else {
        Err(AppError::MissingGuildPermission)
    }
}

fn validate_message_body(body: &str) -> Result<(), AppError> {
    if body.trim().is_empty() || body.chars().count() > MESSAGE_BODY_MAX_CHARS {
        return Err(AppError::MessageTooLong);
    }
    Ok(())
}

#[derive(Serialize, Deserialize, Clone, ToSchema)]
pub struct MessageResponse {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub author: Uuid,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub sent_at: OffsetDateTime,
}

#[derive(Deserialize, IntoParams)]
pub struct ListMessagesQuery {
    /// Cursor: a message id already seen by the caller. Results are the
    /// next page strictly older than it (by `sent_at`, `id` as tiebreak).
    pub before: Option<Uuid>,
    pub limit: Option<i64>,
}

/// `GET /guilds/{id}/channels/{cid}/messages?before=&limit=` — newest
/// first, cursor-paginated. Requires `view_details` on this channel
/// (issue #458) — baseline for a member is exactly the old plain
/// membership gate (unchanged for a channel with no overrides), and a
/// non-member of a `public` channel in a public guild can now read it
/// too, same "public flag widens exposure" shape events already have.
/// Works for archived channels too (history stays readable — only
/// posting stops).
#[utoipa::path(
    get,
    path = "/guilds/{id}/channels/{cid}/messages",
    tag = "guilds",
    params(("id" = Uuid, Path), ("cid" = Uuid, Path), ListMessagesQuery),
    responses((status = 200, body = Vec<MessageResponse>)),
)]
pub async fn list_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, channel_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<ListMessagesQuery>,
) -> Result<Json<Vec<MessageResponse>>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let channel = fetch_channel(&state, guild_id, channel_id).await?;
    require_channel_view_details(&state, guild_id, channel_id, channel.public, actor).await?;

    let limit = query
        .limit
        .unwrap_or(DEFAULT_MESSAGE_PAGE_SIZE)
        .clamp(1, MAX_MESSAGE_PAGE_SIZE);

    let rows = if let Some(before_id) = query.before {
        sqlx::query(
            "SELECT id, channel_id, author, body, sent_at FROM guild_messages \
             WHERE channel_id = $1 AND (sent_at, id) < (\
                 SELECT sent_at, id FROM guild_messages WHERE id = $2 AND channel_id = $1\
             ) ORDER BY sent_at DESC, id DESC LIMIT $3",
        )
        .bind(channel_id)
        .bind(before_id)
        .bind(limit)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query(
            "SELECT id, channel_id, author, body, sent_at FROM guild_messages \
             WHERE channel_id = $1 ORDER BY sent_at DESC, id DESC LIMIT $2",
        )
        .bind(channel_id)
        .bind(limit)
        .fetch_all(&state.pool)
        .await?
    };

    let mut messages = Vec::with_capacity(rows.len());
    for row in rows {
        messages.push(MessageResponse {
            id: row.try_get("id")?,
            channel_id: row.try_get("channel_id")?,
            author: row.try_get("author")?,
            body: row.try_get("body")?,
            sent_at: row.try_get("sent_at")?,
        });
    }
    Ok(Json(messages))
}

#[derive(Serialize, ToSchema)]
pub struct ArchivedMessageResponse {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub author: Uuid,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub sent_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub archived_at: OffsetDateTime,
}

/// `GET /guilds/{id}/channels/{cid}/messages/archive?before=&limit=` — same
/// newest-first, cursor-paginated shape as [`list_messages`], over
/// `guild_messages_archive` instead of the live table. Requires
/// *current* `view_details` on the channel (issue #458), same gate
/// [`list_messages`] uses — see the module doc comment's "Archive read
/// access" section for why this doesn't try to reconstruct membership as
/// of when each message was originally sent.
#[utoipa::path(
    get,
    path = "/guilds/{id}/channels/{cid}/messages/archive",
    tag = "guilds",
    params(("id" = Uuid, Path), ("cid" = Uuid, Path), ListMessagesQuery),
    responses((status = 200, body = Vec<ArchivedMessageResponse>)),
)]
pub async fn list_archive(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, channel_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<ListMessagesQuery>,
) -> Result<Json<Vec<ArchivedMessageResponse>>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let channel = fetch_channel(&state, guild_id, channel_id).await?;
    require_channel_view_details(&state, guild_id, channel_id, channel.public, actor).await?;

    let limit = query
        .limit
        .unwrap_or(DEFAULT_MESSAGE_PAGE_SIZE)
        .clamp(1, MAX_MESSAGE_PAGE_SIZE);

    let rows = if let Some(before_id) = query.before {
        sqlx::query(
            "SELECT id, channel_id, author, body, sent_at, archived_at FROM guild_messages_archive \
             WHERE channel_id = $1 AND (sent_at, id) < (\
                 SELECT sent_at, id FROM guild_messages_archive WHERE id = $2 AND channel_id = $1\
             ) ORDER BY sent_at DESC, id DESC LIMIT $3",
        )
        .bind(channel_id)
        .bind(before_id)
        .bind(limit)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query(
            "SELECT id, channel_id, author, body, sent_at, archived_at FROM guild_messages_archive \
             WHERE channel_id = $1 ORDER BY sent_at DESC, id DESC LIMIT $2",
        )
        .bind(channel_id)
        .bind(limit)
        .fetch_all(&state.pool)
        .await?
    };

    let mut messages = Vec::with_capacity(rows.len());
    for row in rows {
        messages.push(ArchivedMessageResponse {
            id: row.try_get("id")?,
            channel_id: row.try_get("channel_id")?,
            author: row.try_get("author")?,
            body: row.try_get("body")?,
            sent_at: row.try_get("sent_at")?,
            archived_at: row.try_get("archived_at")?,
        });
    }
    Ok(Json(messages))
}

#[derive(Deserialize, ToSchema)]
pub struct SendMessageRequest {
    pub body: String,
}

/// `POST /guilds/{id}/channels/{cid}/messages` — requires current guild
/// membership; rejected if the channel is archived. No transaction, no
/// outbox — see module doc comment.
#[utoipa::path(
    post,
    path = "/guilds/{id}/channels/{cid}/messages",
    tag = "guilds",
    params(("id" = Uuid, Path), ("cid" = Uuid, Path)),
    request_body = SendMessageRequest,
    responses((status = 200, body = MessageResponse)),
)]
pub async fn send_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, channel_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<SendMessageRequest>,
) -> Result<Json<MessageResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    validate_message_body(&body.body)?;
    require_member(&state, guild_id, actor).await?;

    let channel = fetch_channel(&state, guild_id, channel_id).await?;
    if channel.archived_at.is_some() {
        return Err(AppError::ChannelArchived);
    }
    if channel.announcement_only {
        let owner = guild_owner(&state, guild_id).await?;
        let allowed = has_resource_permission(
            &state,
            guild_id,
            owner,
            actor,
            GuildResourceKind::Channel,
            channel_id,
            GuildPermission::ChannelPost,
        )
        .await?;
        if !allowed {
            return Err(AppError::MissingGuildPermission);
        }
    }

    let message_id = Uuid::new_v4();
    let sent_at = OffsetDateTime::now_utc();
    sqlx::query(
        "INSERT INTO guild_messages (id, channel_id, author, body, sent_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(message_id)
    .bind(channel_id)
    .bind(actor)
    .bind(&body.body)
    .bind(sent_at)
    .execute(&state.pool)
    .await?;

    prune_channel(&state, channel_id).await?;

    let response = MessageResponse {
        id: message_id,
        channel_id,
        author: actor,
        body: body.body,
        sent_at,
    };
    // Issue #438: pushes the new message to every websocket connection
    // subscribed to this channel — see `crate::chat`. Best-effort (a lossy
    // broadcast, no receivers is not an error); a client falls back to the
    // paginated `GET` above if it misses this.
    state.chat.publish_channel_message(response.clone());
    // Issue #539: reach subscribers connected to a different node.
    tokio::spawn(crate::realtime_relay::relay_to_peers(
        state.clone(),
        crate::realtime_relay::RelayEvent::ChannelMessage(response.clone()),
    ));
    // Issue #540: at-rest durability on at least one additional node.
    tokio::spawn(crate::chat_replication::replicate_to_peers(
        state.clone(),
        crate::chat_replication::ReplicationEvent::ChannelMessage(response.clone()),
    ));
    Ok(Json(response))
}

/// Keeps at most `message_cap()` newest messages in `channel_id` (ordered
/// newest-first the same way `list_messages` orders them); anything past
/// that moves into `guild_messages_archive` instead of being deleted
/// outright (issue #253, implementing #193's decision). The
/// insert-then-delete pair runs inside one transaction so a row is never
/// visible in neither table (or, worse, in both) if this is interrupted
/// partway through. Both statements independently recompute "the newest N
/// ids to keep" with no row locking, so two concurrent calls for the same
/// channel (e.g. two near-simultaneous `send_message` requests once a
/// channel is at/near cap) can both try to archive the same row; the INSERT
/// is `ON CONFLICT (id) DO NOTHING` so that race is a harmless no-op rather
/// than a primary-key violation, and the DELETE that follows is naturally
/// idempotent. See [`tests::newest_n_survive_pruning`] and
/// [`tests::pruned_ids_land_in_archive_not_deleted`] for the pure-function
/// model of this "keep newest N, archive the rest" semantics, unit tested
/// since no live Postgres is reachable in this sandbox.
async fn prune_channel(state: &AppState, channel_id: Uuid) -> Result<(), AppError> {
    let mut tx = state.pool.begin().await?;

    sqlx::query(
        "INSERT INTO guild_messages_archive (id, channel_id, author, body, sent_at) \
         SELECT id, channel_id, author, body, sent_at FROM guild_messages \
         WHERE channel_id = $1 AND id NOT IN (\
             SELECT id FROM guild_messages WHERE channel_id = $1 \
             ORDER BY sent_at DESC, id DESC LIMIT $2\
         ) ON CONFLICT (id) DO NOTHING",
    )
    .bind(channel_id)
    .bind(message_cap())
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "DELETE FROM guild_messages WHERE channel_id = $1 AND id NOT IN (\
             SELECT id FROM guild_messages WHERE channel_id = $1 \
             ORDER BY sent_at DESC, id DESC LIMIT $2\
         )",
    )
    .bind(channel_id)
    .bind(message_cap())
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

/// Hard-deletes archive rows whose `archived_at` is older than
/// [`archive_retention_days`]'s window — the long-window expiry #193
/// decided the archive tier needs. This is a genuine, final hard delete:
/// nothing is recoverable past this point. Called on a timer by
/// [`run_archive_expiry_worker`]; exercised directly (no live Postgres) via
/// [`tests`] against the pure cutoff math.
async fn expire_archive(state: &AppState) -> Result<u64, AppError> {
    let cutoff = retention_cutoff(OffsetDateTime::now_utc(), archive_retention_days());
    let result = sqlx::query("DELETE FROM guild_messages_archive WHERE archived_at < $1")
        .bind(cutoff)
        .execute(&state.pool)
        .await?;
    Ok(result.rows_affected())
}

/// Pure cutoff math behind [`expire_archive`]: an archive row is expired
/// once its `archived_at` is older than `retention_days` before `now`.
/// Pulled out so the "within window survives, past it doesn't" boundary
/// can be unit tested without a live Postgres — see
/// [`tests::rows_within_window_survive_rows_past_it_expire`].
fn retention_cutoff(now: OffsetDateTime, retention_days: i64) -> OffsetDateTime {
    now - time::Duration::days(retention_days)
}

/// Background driver for archive expiry — same "call it on a timer, log
/// what happened" shape as `crate::retention::run_worker` /
/// `crate::outbox::run_worker`. Intended to be handed to `tokio::spawn`.
pub async fn run_archive_expiry_worker(state: AppState) {
    loop {
        match expire_archive(&state).await {
            Ok(count) if count > 0 => {
                tracing::info!(
                    count,
                    "guild message archive: hard-deleted rows past retention window"
                );
            }
            Ok(_) => {}
            Err(err) => tracing::error!("guild message archive expiry worker: {err}"),
        }
        tokio::time::sleep(ARCHIVE_EXPIRY_INTERVAL).await;
    }
}

/// `DELETE /guilds/{id}/channels/{cid}/messages/{mid}` — moderation, requires
/// `manage_channels`. A real hard delete, and one that reaches both tiers
/// on purpose: it first tries the live `guild_messages` row, and if that
/// finds nothing, falls back to `guild_messages_archive` — see the module
/// doc comment's "Moderation deletion and the archive" section for why a
/// moderator's takedown shouldn't be defeated just because cap-based
/// pruning already moved the row into the archive.
#[utoipa::path(
    delete,
    path = "/guilds/{id}/channels/{cid}/messages/{mid}",
    tag = "guilds",
    params(("id" = Uuid, Path), ("cid" = Uuid, Path), ("mid" = Uuid, Path)),
    responses((status = 200, description = "Message deleted")),
)]
pub async fn delete_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, channel_id, message_id)): Path<(Uuid, Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    fetch_channel(&state, guild_id, channel_id).await?;
    require_manage_channel_resource(&state, guild_id, channel_id, actor).await?;

    let deleted = sqlx::query("DELETE FROM guild_messages WHERE id = $1 AND channel_id = $2")
        .bind(message_id)
        .bind(channel_id)
        .execute(&state.pool)
        .await?;
    let deleted_from_live = deleted.rows_affected() > 0;

    let deleted_from_archive = if deleted_from_live {
        false
    } else {
        let archived =
            sqlx::query("DELETE FROM guild_messages_archive WHERE id = $1 AND channel_id = $2")
                .bind(message_id)
                .bind(channel_id)
                .execute(&state.pool)
                .await?;
        archived.rows_affected() > 0
    };

    if !deleted_from_live && !deleted_from_archive {
        return Err(AppError::MessageNotFound);
    }

    state
        .chat
        .publish_channel_message_deleted(channel_id, message_id);
    // Issue #539: reach subscribers connected to a different node.
    tokio::spawn(crate::realtime_relay::relay_to_peers(
        state.clone(),
        crate::realtime_relay::RelayEvent::ChannelMessageDeleted {
            channel_id,
            message_id,
        },
    ));
    // Issue #540: mark the replica's copy deleted too.
    tokio::spawn(crate::chat_replication::replicate_to_peers(
        state.clone(),
        crate::chat_replication::ReplicationEvent::ChannelMessageDeleted {
            channel_id,
            message_id,
        },
    ));
    Ok(Json(serde_json::json!({ "deleted": true })))
}

/// The per-channel cap [`list_my_guild_announcements`]'s `LATERAL` join
/// takes — keeps one chatty announcement channel from crowding out every
/// other guild's posts in the aggregate, same reasoning a page-size cap
/// exists anywhere else in this crate.
const ANNOUNCEMENT_ALERTS_PER_CHANNEL: i64 = 10;
/// The overall cap across every channel/guild combined.
const ANNOUNCEMENT_ALERTS_TOTAL: i64 = 50;

#[derive(Serialize, ToSchema)]
pub struct GuildAnnouncementAlert {
    pub message_id: Uuid,
    pub channel_id: Uuid,
    pub channel_name: String,
    pub guild_id: Uuid,
    pub author: Uuid,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub sent_at: OffsetDateTime,
}

/// `GET /me/guild-announcements` (issue #280) — the most recent posts to
/// any announcement-only channel in any guild the caller currently belongs
/// to, newest first. This is a plain read, not a notification/unread
/// tracker: read/unread state is the Hub's own client-local concern (see
/// `docs/architecture/guilds.md`'s "Guild announcement alerts" section),
/// matching #22/#74/#253's "chat is operational-tier, not protocol
/// history" posture — there is nothing here to promote to durable state,
/// so there is nothing here to track server-side either.
///
/// Scoped to *current* membership by construction: the `JOIN indexer_guild_members`
/// below means a guild the caller has left simply produces no rows for
/// that guild, the same "no historical-membership machinery for
/// non-durable data" posture `list_archive`'s own doc comment already
/// takes, applied here without needing a separate cleanup step — a member
/// who leaves a guild stops seeing its announcements on their very next
/// poll, automatically.
#[utoipa::path(
    get,
    path = "/me/guild-announcements",
    tag = "guilds",
    responses((status = 200, body = Vec<GuildAnnouncementAlert>)),
)]
pub async fn list_my_guild_announcements(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<GuildAnnouncementAlert>>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let rows = sqlx::query(
        "SELECT gm.id AS message_id, gc.id AS channel_id, gc.name AS channel_name, \
                gc.guild_id, gm.author, gm.body, gm.sent_at \
         FROM guild_channels gc \
         JOIN indexer_guild_members mem ON mem.guild_id = gc.guild_id AND mem.identity_id = $1 \
         JOIN LATERAL ( \
             SELECT id, author, body, sent_at FROM guild_messages \
             WHERE channel_id = gc.id ORDER BY sent_at DESC, id DESC LIMIT $2 \
         ) gm ON true \
         WHERE gc.announcement_only = true AND gc.archived_at IS NULL \
         ORDER BY gm.sent_at DESC, gm.id DESC \
         LIMIT $3",
    )
    .bind(identity_id)
    .bind(ANNOUNCEMENT_ALERTS_PER_CHANNEL)
    .bind(ANNOUNCEMENT_ALERTS_TOTAL)
    .fetch_all(&state.pool)
    .await?;

    let mut alerts = Vec::with_capacity(rows.len());
    for row in rows {
        alerts.push(GuildAnnouncementAlert {
            message_id: row.try_get("message_id")?,
            channel_id: row.try_get("channel_id")?,
            channel_name: row.try_get("channel_name")?,
            guild_id: row.try_get("guild_id")?,
            author: row.try_get("author")?,
            body: row.try_get("body")?,
            sent_at: row.try_get("sent_at")?,
        });
    }
    Ok(Json(alerts))
}

/// Pure model of [`prune_channel`]'s "keep newest `cap`, ordered by
/// `sent_at` desc with `id` desc as a tiebreak" semantics, given messages
/// in arbitrary order. Returns the ids that would be pruned. This mirrors
/// the SQL query but isn't itself called from the request path — it exists
/// purely so the pruning *behavior* can be exercised without a live
/// Postgres connection (see the tests below) — not called from the
/// request path itself, hence `#[cfg(test)]`.
#[cfg(test)]
fn ids_to_prune(mut messages: Vec<(Uuid, OffsetDateTime)>, cap: i64) -> Vec<Uuid> {
    let cap = usize::try_from(cap.max(0)).unwrap_or(usize::MAX);
    messages.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));
    if messages.len() <= cap {
        return Vec::new();
    }
    messages
        .split_off(cap)
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — pure-logic checks only. The
    //! endpoint-level flows (member vs non-member read/post, ordering,
    //! pagination) are covered by `crates/server/tests/guild_channels.rs`,
    //! gated `--ignored`.

    use super::*;

    #[test]
    fn validate_message_body_rejects_empty_and_overlong() {
        assert!(validate_message_body("hello").is_ok());
        assert!(validate_message_body("   ").is_err());
        assert!(validate_message_body(&"a".repeat(MESSAGE_BODY_MAX_CHARS + 1)).is_err());
        assert!(validate_message_body(&"a".repeat(MESSAGE_BODY_MAX_CHARS)).is_ok());
    }

    #[test]
    fn message_cap_falls_back_to_default_when_env_unset_or_invalid() {
        // Doesn't touch/assert the real env var (parallel test runs would
        // race on it) — just exercises the parsing/fallback logic directly.
        let parse_or_default = |raw: Option<&str>| -> i64 {
            raw.and_then(|s| s.parse::<i64>().ok())
                .filter(|&n| n > 0)
                .unwrap_or(DEFAULT_MESSAGE_CAP)
        };
        assert_eq!(parse_or_default(None), DEFAULT_MESSAGE_CAP);
        assert_eq!(parse_or_default(Some("not a number")), DEFAULT_MESSAGE_CAP);
        assert_eq!(parse_or_default(Some("0")), DEFAULT_MESSAGE_CAP);
        assert_eq!(parse_or_default(Some("-5")), DEFAULT_MESSAGE_CAP);
        assert_eq!(parse_or_default(Some("42")), 42);
    }

    #[test]
    fn newest_n_survive_pruning() {
        let base = OffsetDateTime::now_utc();
        let oldest = Uuid::new_v4();
        let middle = Uuid::new_v4();
        let newest = Uuid::new_v4();
        let messages = vec![
            (oldest, base),
            (middle, base + time::Duration::seconds(1)),
            (newest, base + time::Duration::seconds(2)),
        ];

        let pruned = ids_to_prune(messages.clone(), 2);
        assert_eq!(pruned, vec![oldest]);

        let pruned_none = ids_to_prune(messages.clone(), 3);
        assert!(pruned_none.is_empty());

        let mut pruned_all_but_newest = ids_to_prune(messages, 1);
        pruned_all_but_newest.sort();
        let mut expected = vec![oldest, middle];
        expected.sort();
        assert_eq!(pruned_all_but_newest, expected);
    }

    #[test]
    fn cap_at_or_above_message_count_prunes_nothing() {
        let base = OffsetDateTime::now_utc();
        let messages: Vec<_> = (0..5)
            .map(|i| (Uuid::new_v4(), base + time::Duration::seconds(i)))
            .collect();
        assert!(ids_to_prune(messages.clone(), 5).is_empty());
        assert!(ids_to_prune(messages, 100).is_empty());
    }

    // The "never touches the ledger/outbox" invariant is checked by a
    // source grep in `crates/server/tests/guild_messages_no_ledger.rs`
    // rather than here — a test in this file can't grep this file for the
    // absence of a string without that very check string then being
    // present in the file, defeating itself.

    /// Matches `ids_to_prune`'s pure "keep newest N" model directly against
    /// what `prune_channel` is supposed to do with the rest: nothing is
    /// silently lost — every pruned id plus every kept id must together
    /// account for the full input set, modeling "moved to the archive"
    /// rather than "deleted outright" (issue #253).
    #[test]
    fn pruned_ids_land_in_archive_not_deleted_outright() {
        let base = OffsetDateTime::now_utc();
        let messages: Vec<_> = (0..7)
            .map(|i| (Uuid::new_v4(), base + time::Duration::seconds(i)))
            .collect();
        let all_ids: std::collections::HashSet<Uuid> = messages.iter().map(|(id, _)| *id).collect();

        let cap = 3;
        let archived = ids_to_prune(messages.clone(), cap);
        let archived_set: std::collections::HashSet<Uuid> = archived.iter().copied().collect();

        // Every archived id really was one of the input messages (nothing
        // fabricated), archiving didn't duplicate an id, and the kept set
        // (input minus archived) is exactly `cap` in size — i.e. every
        // message the cap doesn't keep is accounted for as "archived",
        // never as "just gone."
        assert!(archived_set.is_subset(&all_ids));
        assert_eq!(
            archived_set.len(),
            archived.len(),
            "no duplicate archived ids"
        );
        let kept: std::collections::HashSet<Uuid> =
            all_ids.difference(&archived_set).copied().collect();
        assert_eq!(kept.len(), cap as usize);
        assert_eq!(kept.len() + archived_set.len(), all_ids.len());
    }

    #[test]
    fn archive_retention_days_falls_back_to_default_when_env_unset_or_invalid() {
        // Same parsing/fallback logic as `message_cap_falls_back_to_default...`
        // above, exercised directly rather than through the real env var
        // (parallel test runs would race on it).
        let parse_or_default = |raw: Option<&str>| -> i64 {
            raw.and_then(|s| s.parse::<i64>().ok())
                .filter(|&n| n > 0)
                .unwrap_or(DEFAULT_ARCHIVE_RETENTION_DAYS)
        };
        assert_eq!(parse_or_default(None), DEFAULT_ARCHIVE_RETENTION_DAYS);
        assert_eq!(
            parse_or_default(Some("not a number")),
            DEFAULT_ARCHIVE_RETENTION_DAYS
        );
        assert_eq!(parse_or_default(Some("0")), DEFAULT_ARCHIVE_RETENTION_DAYS);
        assert_eq!(parse_or_default(Some("-5")), DEFAULT_ARCHIVE_RETENTION_DAYS);
        assert_eq!(parse_or_default(Some("365")), 365);
    }

    #[test]
    fn rows_within_window_survive_rows_past_it_expire() {
        let now = OffsetDateTime::now_utc();
        let retention_days = 730;
        let cutoff = retention_cutoff(now, retention_days);

        let just_within = cutoff + time::Duration::seconds(1);
        let just_past = cutoff - time::Duration::seconds(1);
        let long_ago = now - time::Duration::days(retention_days + 365);
        let recent = now - time::Duration::days(1);

        // `expire_archive`'s query is `WHERE archived_at < cutoff` — model
        // that predicate directly against the same cutoff it computes.
        assert!(
            just_within >= cutoff,
            "row just inside the window must survive"
        );
        assert!(just_past < cutoff, "row just past the window must expire");
        assert!(long_ago < cutoff, "a row far past the window must expire");
        assert!(recent >= cutoff, "a recent row must survive");
    }
}
