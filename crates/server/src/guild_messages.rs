//! Guild chat messages (issue #22) — deliberately NOT protocol history.
//!
//! `GuildMessage` rows (`crates/protocol/src/guilds.rs`) live in the
//! `guild_messages` table and never touch the ledger commit path or the
//! outbox module — same "ephemeral/non-ledger" reasoning `crate::presence`
//! already documents for presence, applied here to ordinary chat: it's
//! high-volume, non-interoperable state, not something a game or another
//! identity ever needs to *prove* was said. No protocol event kind for an
//! individual chat message exists anywhere, on purpose. This is enforced
//! two ways: by construction (this module never imports the outbox or the
//! chain crate, checked by a source grep in
//! `crates/server/tests/guild_messages_no_ledger.rs`), and by design —
//! channel *structure* (`crate::channels`) is durable history, individual
//! *messages* are not.
//!
//! **Retention.** Messages are kept indefinitely up to a configurable cap
//! per channel (`GUILD_CHANNEL_MESSAGE_CAP` env var, default 10,000 — see
//! [`message_cap`]); the oldest are pruned once a channel exceeds it (see
//! [`prune_channel`]). No client should assume guild chat history is
//! permanent.
//!
//! **Membership.** Reading or posting requires current guild membership,
//! via `crate::channels::require_member` — see that module's doc comment
//! for the `guild_members` table this depends on and issue #21's status.
//! Moderation (hard-deleting a message) instead requires `manage_channels`
//! (`crate::channels`'s permission helper), same as channel management —
//! messages aren't history, so there's nothing to preserve when one is
//! deleted.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::channels::{actor_permissions, fetch_channel, guild_owner, require_member};
use crate::error::AppError;
use crate::guilds::has_guild_permission;
use crate::handlers::authenticate;
use crate::state::AppState;
use avalon_protocol::guilds::GuildPermission;

const MESSAGE_BODY_MAX_CHARS: usize = 4000;
const DEFAULT_MESSAGE_PAGE_SIZE: i64 = 50;
const MAX_MESSAGE_PAGE_SIZE: i64 = 200;
const DEFAULT_MESSAGE_CAP: i64 = 10_000;

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
/// newest-first the same way `list_messages` orders them), deleting the
/// rest in one statement. See [`tests::newest_n_survive_pruning`] for the
/// pure-function model of this exact "keep newest N" semantics, unit
/// tested since no live Postgres is reachable in this sandbox.
async fn prune_channel(state: &AppState, channel_id: Uuid) -> Result<(), AppError> {
    sqlx::query(
        "DELETE FROM guild_messages WHERE channel_id = $1 AND id NOT IN (\
             SELECT id FROM guild_messages WHERE channel_id = $1 \
             ORDER BY sent_at DESC, id DESC LIMIT $2\
         )",
    )
    .bind(channel_id)
    .bind(message_cap())
    .execute(&state.pool)
    .await?;
    Ok(())
}

/// `DELETE /guilds/{id}/channels/{cid}/messages/{mid}` — moderation, requires
/// `manage_channels`. A real hard delete: messages aren't history, there's
/// nothing to preserve.
pub async fn delete_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, channel_id, message_id)): Path<(Uuid, Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    fetch_channel(&state, guild_id, channel_id).await?;

    let owner = guild_owner(&state, guild_id).await?;
    let permissions = actor_permissions(&state, guild_id, actor).await?;
    if !has_guild_permission(owner, actor, &permissions, GuildPermission::ManageChannels) {
        return Err(AppError::MissingGuildPermission);
    }

    let deleted = sqlx::query("DELETE FROM guild_messages WHERE id = $1 AND channel_id = $2")
        .bind(message_id)
        .bind(channel_id)
        .execute(&state.pool)
        .await?;
    if deleted.rows_affected() == 0 {
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
}
