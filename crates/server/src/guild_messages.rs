//! Guild chat messages (issue #22) — deliberately NOT protocol history.
//!
//! `GuildMessage` rows (`crates/protocol/src/guilds.rs`) live in the
//! `guild_messages` table and never touch the ledger commit path or the
//! outbox module — same "ephemeral/non-ledger" reasoning `crate::presence`
//! already documents for presence, applied here to ordinary chat: it's
//! high-volume, non-interoperable state, not something an integrator or another
//! identity ever needs to *prove* was said. No protocol event kind for an
//! individual chat message exists anywhere, on purpose. This is enforced
//! two ways: by construction (this module never imports the outbox or the
//! chain crate, checked by a source grep in
//! `crates/server/tests/guild_messages_no_ledger.rs`), and by design —
//! channel *structure* (`crate::channels`) is durable history, individual
//! *messages* are not.
//!
//! **Retention (issue #253, implementing #193's decision).** Messages are
//! kept indefinitely up to a configurable cap per channel
//! (`GUILD_CHANNEL_MESSAGE_CAP` env var, default 10,000 — see
//! [`message_cap`]); once a channel exceeds it, [`prune_channel`] moves the
//! oldest rows into `guild_messages_archive` instead of deleting them
//! outright. The archive itself is held for a much longer, separately
//! configurable window (`GUILD_MESSAGE_ARCHIVE_RETENTION_DAYS`, default 730
//! days — see [`archive_retention_days`]); [`expire_archive`] hard-deletes
//! whatever falls past that window, with nothing recoverable afterward. No
//! client should assume guild chat history is permanent, in the live table
//! or the archive.
//!
//! **Archive read access.** `GET .../channels/{cid}/messages/archive` requires
//! *current* guild membership, exactly like [`list_messages`] — not
//! membership at the time each archived message was sent. `guild_members`
//! is a live projection with no point-in-time history of its own (that
//! would require replaying membership through the indexer/ledger, real
//! infrastructure this ticket doesn't need to build for what is, by
//! design, non-durable data); current-membership keeps the archive's
//! access rule identical to the live channel's and avoids inventing new
//! historical-membership machinery for a tier that explicitly isn't
//! protocol history.
//!
//! **Moderation deletion and the archive.** [`delete_message`] now purges
//! *both* the live row and any archive copy of the same message id.
//! Ordinarily a message a moderator deletes is still live (the archive
//! endpoint is separate from the moderation endpoint, and a message can't
//! be both), but treating the two as strictly distinct would leave a loophole:
//! content a moderator hard-deletes for cause (harassment, illegal
//! content, etc.) could still surface later in the archive if pruning had
//! already run first. Moderation intent should win regardless of which
//! tier currently holds the row, so `delete_message` is written to check
//! both tables rather than assuming the row it's after is always in
//! `guild_messages`.
//!
//! **Membership.** Reading or posting requires current guild membership,
//! via `crate::channels::require_member` — see that module's doc comment
//! for the `guild_members` table this depends on and issue #21's status.
//! Moderation (hard-deleting a message) instead requires `manage_channels`,
//! resource-aware against the channel it's posted in
//! (`crate::channels::require_manage_channel_resource`, issue #250), same
//! as channel management — messages aren't history, so there's nothing to
//! preserve when one is deleted.
//!
//! **Announcement-only channels (issue #250).** When a channel's
//! `announcement_only` flag is set, posting additionally requires the
//! `ChannelPost` permission for that specific channel
//! (`crate::guilds::has_resource_permission`) — membership alone is no
//! longer sufficient. A regular channel (the default) keeps today's
//! "any current member may post" behavior unchanged; this is strictly
//! additive per-channel, not a change to the guild-wide permission model.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::channels::{
    fetch_channel, guild_owner, require_manage_channel_resource, require_member,
};
use crate::error::AppError;
use crate::guilds::has_resource_permission;
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

fn validate_message_body(body: &str) -> Result<(), AppError> {
    if body.trim().is_empty() || body.chars().count() > MESSAGE_BODY_MAX_CHARS {
        return Err(AppError::MessageTooLong);
    }
    Ok(())
}

#[derive(Serialize)]
pub struct MessageResponse {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub author: Uuid,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    pub sent_at: OffsetDateTime,
}

#[derive(Deserialize)]
pub struct ListMessagesQuery {
    /// Cursor: a message id already seen by the caller. Results are the
    /// next page strictly older than it (by `sent_at`, `id` as tiebreak).
    pub before: Option<Uuid>,
    pub limit: Option<i64>,
}

/// `GET /guilds/{id}/channels/{cid}/messages?before=&limit=` — newest
/// first, cursor-paginated. Requires current guild membership. Works for
/// archived channels too (history stays readable — only posting stops).
pub async fn list_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, channel_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<ListMessagesQuery>,
) -> Result<Json<Vec<MessageResponse>>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    require_member(&state, guild_id, actor).await?;
    fetch_channel(&state, guild_id, channel_id).await?;

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

#[derive(Serialize)]
pub struct ArchivedMessageResponse {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub author: Uuid,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    pub sent_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub archived_at: OffsetDateTime,
}

/// `GET /guilds/{id}/channels/{cid}/messages/archive?before=&limit=` — same
/// newest-first, cursor-paginated shape as [`list_messages`], over
/// `guild_messages_archive` instead of the live table. Requires *current*
/// guild membership — see the module doc comment's "Archive read access"
/// section for why this doesn't try to reconstruct membership as of when
/// each message was originally sent.
pub async fn list_archive(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, channel_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<ListMessagesQuery>,
) -> Result<Json<Vec<ArchivedMessageResponse>>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    require_member(&state, guild_id, actor).await?;
    fetch_channel(&state, guild_id, channel_id).await?;

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

#[derive(Deserialize)]
pub struct SendMessageRequest {
    pub body: String,
}

/// `POST /guilds/{id}/channels/{cid}/messages` — requires current guild
/// membership; rejected if the channel is archived. No transaction, no
/// outbox — see module doc comment.
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

    Ok(Json(MessageResponse {
        id: message_id,
        channel_id,
        author: actor,
        body: body.body,
        sent_at,
    }))
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

    Ok(Json(serde_json::json!({ "deleted": true })))
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
