//! Direct/small-group conversations — the identity-to-identity
//! sibling of `crate::guild_messages`: never touches the outbox or ledger,
//! participant-only, blocking-aware. See `docs/projects/backend-server/architecture/communication.md`
//! for idempotent creation, the relationship gate,
//! and how blocking is enforced identically on read and write.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::blocks;
use crate::discovery;
use crate::error::AppError;
use crate::friends;
use crate::handlers::authenticate;
use crate::state::AppState;

const MESSAGE_BODY_MAX_CHARS: usize = 4000;
const DEFAULT_MESSAGE_PAGE_SIZE: i64 = 50;
const MAX_MESSAGE_PAGE_SIZE: i64 = 200;
const DEFAULT_MESSAGE_CAP: i64 = 10_000;

/// Reads `CONVERSATION_MESSAGE_CAP`, otherwise `DEFAULT_MESSAGE_CAP`. Same
/// non-positive/unparseable-falls-back-to-default rule as
/// `guild_messages::message_cap`, so a bad env value never silently
/// disables pruning.
fn message_cap() -> i64 {
    std::env::var("CONVERSATION_MESSAGE_CAP")
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

/// Deterministic key for a participant set — sorted, deduplicated,
/// comma-joined ids — so the same set of identities always produces the
/// same key regardless of request order. See the module doc comment's
/// "Idempotent creation" section.
pub(crate) fn participants_key(participants: &[Uuid]) -> String {
    let mut sorted = participants.to_vec();
    sorted.sort();
    sorted.dedup();
    sorted
        .iter()
        .map(Uuid::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Every identity currently participating in `conversation_id`, in no
/// particular order. Empty (not an error) if the conversation doesn't
/// exist — callers that care about existence go through
/// [`require_unblocked_participant`], which treats "doesn't exist" and "exists but
/// you're not in it" identically on purpose.
async fn conversation_participant_ids(
    state: &AppState,
    conversation_id: Uuid,
) -> Result<Vec<Uuid>, AppError> {
    let rows =
        sqlx::query("SELECT identity_id FROM conversation_participants WHERE conversation_id = $1")
            .bind(conversation_id)
            .fetch_all(&state.pool)
            .await?;
    let mut ids = Vec::with_capacity(rows.len());
    for row in rows {
        ids.push(row.try_get("identity_id")?);
    }
    Ok(ids)
}

/// Returns the conversation's current participant ids if `actor` is
/// currently a participant *and* no block exists between any two
/// participants in the conversation; `Err(NotConversationParticipant)`
/// otherwise. Used by [`list_messages`] and [`send_message`] to gate both
/// reads and posts identically — see the module doc comment's "Blocking,
/// enforced on both read and write" section.
///
/// Deliberately runs both the participant lookup and
/// [`blocks::has_block_among`] unconditionally, rather than short-circuiting
/// on the first failure: short-circuiting would mean a blocked participant
/// (2 queries: membership passes, then the block check fails) takes
/// measurably longer than a genuine non-participant (1 query: membership
/// already fails) — a timing side channel on top of the functional leak
/// this function exists to close. Both checks always run; the cost is the
/// same two queries regardless of which one (or both) actually fail.
///
/// "Conversation id doesn't exist at all" resolves to an empty participant
/// list, which fails the membership check the same way a real conversation
/// the actor isn't in does — [`has_block_among`](blocks::has_block_among)
/// on an empty slice is trivially `false` and doesn't change the outcome.
pub(crate) async fn require_unblocked_participant(
    state: &AppState,
    conversation_id: Uuid,
    actor: Uuid,
) -> Result<Vec<Uuid>, AppError> {
    let participants = conversation_participant_ids(state, conversation_id).await?;
    let blocked = blocks::has_block_among(state, &participants).await?;
    if participants.contains(&actor) && !blocked {
        Ok(participants)
    } else {
        Err(AppError::NotConversationParticipant)
    }
}

/// True if every id in `others` is a friend or guild-mate of `caller` — the
/// relationship gate [`create_conversation`] applies to every named
/// participant. Pure, so it's unit-testable without a database.
fn all_related_to_caller(
    others: &[Uuid],
    friends: &std::collections::HashSet<Uuid>,
    guild_mates: &std::collections::HashSet<Uuid>,
) -> bool {
    others
        .iter()
        .all(|id| friends.contains(id) || guild_mates.contains(id))
}

#[derive(Serialize, ToSchema)]
pub struct ConversationResponse {
    pub id: Uuid,
    pub participants: Vec<Uuid>,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateConversationRequest {
    pub participants: Vec<Uuid>,
}

/// `POST /conversations` — session-authenticated. The caller is always
/// added to the participant set, then deduplicated; rejects fewer than two
/// distinct identities or any participant the caller isn't related to (see
/// the module doc comment's "Relationship gate" section). Idempotent on the
/// final participant set — see "Idempotent creation" above.
#[utoipa::path(
    post,
    path = "/conversations",
    tag = "chat",
    request_body = CreateConversationRequest,
    responses((status = 200, body = ConversationResponse)),
)]
pub async fn create_conversation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateConversationRequest>,
) -> Result<Json<ConversationResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;

    let mut participants = body.participants.clone();
    participants.push(actor);
    participants.sort();
    participants.dedup();

    if participants.len() < 2 {
        return Err(AppError::InvalidConversationParticipants);
    }

    // Relationship gate — also closes the existence oracle, since a
    // nonexistent id can never be a friend or guild-mate.
    let others: Vec<Uuid> = participants
        .iter()
        .copied()
        .filter(|&id| id != actor)
        .collect();
    let friend_ids = friends::friend_partners(&state, actor).await?;
    let guild_mates = discovery::mutual_guild_members(&state, actor).await?;
    if !all_related_to_caller(&others, &friend_ids, &guild_mates) {
        return Err(AppError::InvalidConversationParticipants);
    }

    let key = participants_key(&participants);
    let mut tx = state.pool.begin().await?;

    let candidate_id = Uuid::new_v4();
    let created_at = OffsetDateTime::now_utc();
    let inserted = sqlx::query(
        "INSERT INTO conversations (id, participants_key, created_at) VALUES ($1, $2, $3) \
         ON CONFLICT (participants_key) DO NOTHING",
    )
    .bind(candidate_id)
    .bind(&key)
    .bind(created_at)
    .execute(&mut *tx)
    .await?;

    let conversation_id = if inserted.rows_affected() == 1 {
        for participant in &participants {
            sqlx::query(
                "INSERT INTO conversation_participants (conversation_id, identity_id) \
                 VALUES ($1, $2)",
            )
            .bind(candidate_id)
            .bind(participant)
            .execute(&mut *tx)
            .await?;
        }
        candidate_id
    } else {
        // Someone else already holds this exact participant set — the
        // INSERT above was a harmless no-op against the unique constraint.
        // Look the existing row up by the same key rather than creating a
        // second one.
        sqlx::query("SELECT id FROM conversations WHERE participants_key = $1")
            .bind(&key)
            .fetch_one(&mut *tx)
            .await?
            .try_get("id")?
    };

    tx.commit().await?;

    Ok(Json(ConversationResponse {
        id: conversation_id,
        participants,
    }))
}

/// `GET /conversations` — the caller's own conversation list. A
/// conversation with a block anywhere in its participant set is left out —
/// see the module doc comment's note on why, and
/// [`require_unblocked_participant`] for the same rule applied to a single
/// conversation.
#[utoipa::path(
    get,
    path = "/conversations",
    tag = "chat",
    responses((status = 200, body = Vec<ConversationResponse>)),
)]
pub async fn list_my_conversations(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<ConversationResponse>>, AppError> {
    let actor = authenticate(&state, &headers).await?;

    let rows = sqlx::query(
        "SELECT c.id AS id, array_agg(cp2.identity_id) AS participants \
         FROM conversations c \
         JOIN conversation_participants cp ON cp.conversation_id = c.id AND cp.identity_id = $1 \
         JOIN conversation_participants cp2 ON cp2.conversation_id = c.id \
         GROUP BY c.id",
    )
    .bind(actor)
    .fetch_all(&state.pool)
    .await?;

    let mut conversations = Vec::with_capacity(rows.len());
    for row in rows {
        let id = row.try_get("id")?;
        let participants: Vec<Uuid> = row.try_get("participants")?;
        if blocks::has_block_among(&state, &participants).await? {
            continue;
        }
        conversations.push(ConversationResponse { id, participants });
    }
    Ok(Json(conversations))
}

// Renamed in the published schema: a bare
// `ToSchema` name collides with `guild_messages::MessageResponse` — both
// register as `MessageResponse` in `openapi.rs`'s `components(schemas(...))`
// list, and utoipa's aggregation silently lets the second-registered one
// win, so `docs/generated/openapi.json`'s `MessageResponse` component
// actually described `guild_messages::MessageResponse`'s shape
// (`channel_id`) even for this endpoint's real `conversation_id` field —
// every SDK generated from that schema for `/conversations/{id}/messages`
// deserialized the wrong field name.
#[derive(Serialize, Deserialize, Clone, ToSchema)]
#[schema(as = ConversationMessageResponse)]
pub struct MessageResponse {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub author: Uuid,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub sent_at: OffsetDateTime,
}

#[derive(Deserialize, IntoParams)]
pub struct ListMessagesQuery {
    /// Cursor: a message id already seen by the caller. Results are the
    /// next page strictly older than it (by `sent_at`, `id` as tiebreak) —
    /// same cursor style as `guild_messages::ListMessagesQuery`.
    pub before: Option<Uuid>,
    pub limit: Option<i64>,
}

/// `GET /conversations/{id}/messages?before=&limit=` — newest first,
/// cursor-paginated. Requires current participation and no block among the
/// conversation's participants — same gate [`send_message`] uses, so a
/// blocked participant's read fails exactly as their write does. See the
/// module doc comment's "Blocking, enforced on both read and write"
/// section.
#[utoipa::path(
    get,
    path = "/conversations/{id}/messages",
    tag = "chat",
    params(("id" = Uuid, Path), ListMessagesQuery),
    responses((status = 200, body = Vec<MessageResponse>)),
)]
pub async fn list_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(conversation_id): Path<Uuid>,
    Query(query): Query<ListMessagesQuery>,
) -> Result<Json<Vec<MessageResponse>>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    require_unblocked_participant(&state, conversation_id, actor).await?;

    let limit = query
        .limit
        .unwrap_or(DEFAULT_MESSAGE_PAGE_SIZE)
        .clamp(1, MAX_MESSAGE_PAGE_SIZE);

    let rows = if let Some(before_id) = query.before {
        sqlx::query(
            "SELECT id, conversation_id, author, body, sent_at FROM conversation_messages \
             WHERE conversation_id = $1 AND (sent_at, id) < (\
                 SELECT sent_at, id FROM conversation_messages \
                 WHERE id = $2 AND conversation_id = $1\
             ) ORDER BY sent_at DESC, id DESC LIMIT $3",
        )
        .bind(conversation_id)
        .bind(before_id)
        .bind(limit)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query(
            "SELECT id, conversation_id, author, body, sent_at FROM conversation_messages \
             WHERE conversation_id = $1 ORDER BY sent_at DESC, id DESC LIMIT $2",
        )
        .bind(conversation_id)
        .bind(limit)
        .fetch_all(&state.pool)
        .await?
    };

    let mut messages = Vec::with_capacity(rows.len());
    for row in rows {
        messages.push(MessageResponse {
            id: row.try_get("id")?,
            conversation_id: row.try_get("conversation_id")?,
            author: row.try_get("author")?,
            body: row.try_get("body")?,
            sent_at: row.try_get("sent_at")?,
        });
    }
    Ok(Json(messages))
}

// Renamed in the published schema (same class of bug found and fixed for
// `MessageResponse`/`ConversationMessageResponse`): a
// bare `ToSchema` name collides with `guild_messages::SendMessageRequest`
// (both register as `SendMessageRequest` in `openapi.rs`'s aggregator;
// utoipa lets the second-registered one win silently) — without this,
// the published schema for this endpoint was missing `client_entry_id`.
#[derive(Deserialize, ToSchema)]
#[schema(as = ConversationSendMessageRequest)]
pub struct SendMessageRequest {
    pub body: String,
    /// The submitting client's journal `EntryId`, when
    /// this request came from the SDK's deferred submission engine rather
    /// than a direct online send. Optional — a message sent directly online
    /// never sets this and never needs to dedupe against anything (see
    /// migration `0037_conversation_message_idempotency`).
    ///
    /// A retried request after a dropped response carries the *same*
    /// `client_entry_id` as the original attempt — that's the whole
    /// mechanism: [`send_message`] treats a conflict on
    /// `(conversation_id, client_entry_id)` as "already applied" and
    /// returns the existing row instead of erroring or inserting a
    /// duplicate.
    pub client_entry_id: Option<Uuid>,
}

/// `POST /conversations/{id}/messages` — requires current participation,
/// and — unlike `guild_messages::send_message` — an additional group-wide
/// block check. See the module doc comment's "Blocking, enforced on both
/// read and write" section for why the rejection is indistinguishable from
/// a non-participant's, on both this endpoint and [`list_messages`].
///
/// **Idempotent when `client_entry_id` is set**: inserts with
/// `ON CONFLICT (conversation_id, client_entry_id) DO NOTHING` against the
/// partial unique index from migration `0037_conversation_message_idempotency`
/// and, if that hit an existing row instead of inserting a new one, looks
/// the existing row up and returns it — the exact same "the unique
/// constraint is what actually prevents duplicates, the query just
/// discovers which case it's in" shape [`create_conversation`] already uses
/// for `participants_key`. This is what lets the SDK's deferred submission
/// engine retry a submission whose response was dropped without ever
/// double-applying it.
#[utoipa::path(
    post,
    path = "/conversations/{id}/messages",
    tag = "chat",
    params(("id" = Uuid, Path)),
    request_body = SendMessageRequest,
    responses((status = 200, body = MessageResponse)),
)]
pub async fn send_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(conversation_id): Path<Uuid>,
    Json(body): Json<SendMessageRequest>,
) -> Result<Json<MessageResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    validate_message_body(&body.body)?;
    require_unblocked_participant(&state, conversation_id, actor).await?;

    let message_id = Uuid::new_v4();
    let sent_at = OffsetDateTime::now_utc();
    let inserted = sqlx::query(
        "INSERT INTO conversation_messages (id, conversation_id, author, body, sent_at, client_entry_id) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (conversation_id, client_entry_id) WHERE client_entry_id IS NOT NULL DO NOTHING",
    )
    .bind(message_id)
    .bind(conversation_id)
    .bind(actor)
    .bind(&body.body)
    .bind(sent_at)
    .bind(body.client_entry_id)
    .execute(&state.pool)
    .await?;

    if inserted.rows_affected() == 0 {
        // Only reachable when client_entry_id is Some — the conflict target
        // above is a partial index that never matches a NULL client_entry_id,
        // so a plain online send (no client_entry_id) always inserts.
        // Someone (this same retry, or a concurrent one) already landed this
        // exact (conversation_id, client_entry_id) pair — look it up and
        // return it rather than erroring or creating a duplicate.
        let client_entry_id = body
            .client_entry_id
            .expect("ON CONFLICT only fires when client_entry_id is Some");
        let existing = find_message_by_client_entry_id(&state, conversation_id, client_entry_id)
            .await?
            .ok_or(AppError::MessageNotFound)?;
        return Ok(Json(existing));
    }

    prune_conversation(&state, conversation_id).await?;

    let response = MessageResponse {
        id: message_id,
        conversation_id,
        author: actor,
        body: body.body,
        sent_at,
    };
    // Issue #438: pushes the new message to every websocket connection
    // subscribed to this conversation — see `crate::chat`. Only the
    // fresh-insert path publishes; the idempotent-retry return above
    // already published once, on the original send.
    state.chat.publish_conversation_message(response.clone());
    // Issue #539: reach subscribers connected to a different node.
    tokio::spawn(crate::realtime_relay::relay_to_peers(
        state.clone(),
        crate::realtime_relay::RelayEvent::ConversationMessage(response.clone()),
    ));
    // Issue #540: at-rest durability on at least one additional node.
    tokio::spawn(crate::chat_replication::replicate_to_peers(
        state.clone(),
        crate::chat_replication::ReplicationEvent::ConversationMessage(response.clone()),
    ));
    Ok(Json(response))
}

/// Looks up a message already recorded for `(conversation_id,
/// client_entry_id)` — the read half of [`send_message`]'s idempotency
/// check. `None` only when no submission for this entry id has landed yet;
/// [`send_message`] only calls this after losing the `ON CONFLICT` race, so
/// in that call site it should always find a row.
async fn find_message_by_client_entry_id(
    state: &AppState,
    conversation_id: Uuid,
    client_entry_id: Uuid,
) -> Result<Option<MessageResponse>, AppError> {
    let row = sqlx::query(
        "SELECT id, conversation_id, author, body, sent_at FROM conversation_messages \
         WHERE conversation_id = $1 AND client_entry_id = $2",
    )
    .bind(conversation_id)
    .bind(client_entry_id)
    .fetch_optional(&state.pool)
    .await?;

    row.map(|row| {
        Ok(MessageResponse {
            id: row.try_get("id")?,
            conversation_id: row.try_get("conversation_id")?,
            author: row.try_get("author")?,
            body: row.try_get("body")?,
            sent_at: row.try_get("sent_at")?,
        })
    })
    .transpose()
}

/// Keeps at most `message_cap()` newest messages in `conversation_id`
/// (ordered the same way [`list_messages`] orders them), hard-deleting the
/// rest — plain cap-based pruning, no archive tier (see module doc
/// comment). Mirrors `guild_messages::prune_channel`'s query shape minus
/// the archive-insert half.
async fn prune_conversation(state: &AppState, conversation_id: Uuid) -> Result<(), AppError> {
    sqlx::query(
        "DELETE FROM conversation_messages WHERE conversation_id = $1 AND id NOT IN (\
             SELECT id FROM conversation_messages WHERE conversation_id = $1 \
             ORDER BY sent_at DESC, id DESC LIMIT $2\
         )",
    )
    .bind(conversation_id)
    .bind(message_cap())
    .execute(&state.pool)
    .await?;
    Ok(())
}

/// Pure model of [`prune_conversation`]'s "keep newest `cap`, ordered by
/// `sent_at` desc with `id` desc as a tiebreak" semantics — same shape as
/// `guild_messages::ids_to_prune`, minus the archive distinction (pruned
/// here really does mean deleted, not moved). Returns the ids that would
/// be pruned. `#[cfg(test)]` only — not called from the request path.
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

/// Pure decision behind [`send_message`]'s rejection: denied if the actor
/// isn't a participant *or* a block exists among participants, both
/// producing the identical outcome — modeling why the two reasons are
/// indistinguishable to the caller. `#[cfg(test)]` only.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
enum SendDecision {
    Allowed,
    Denied,
}

#[cfg(test)]
fn decide_send(is_participant: bool, blocked_pair_present: bool) -> SendDecision {
    if !is_participant || blocked_pair_present {
        SendDecision::Denied
    } else {
        SendDecision::Allowed
    }
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — pure-logic checks only. The
    //! endpoint-level flows (participant vs non-participant read/post,
    //! idempotent creation, cross-session exchange, mid-conversation
    //! blocking) belong in `crates/server/tests/conversations.rs`, gated
    //! `--ignored`, matching `crates/server/tests/guild_channels.rs`'s
    //! precedent.

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
        // race on it) — exercises the parsing/fallback logic directly, same
        // approach `guild_messages`'s equivalent test uses.
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
    fn participants_key_is_order_independent_and_deduplicates() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();

        assert_eq!(participants_key(&[a, b, c]), participants_key(&[c, a, b]));
        assert_eq!(participants_key(&[a, b]), participants_key(&[b, a, a, b]));
        assert_ne!(participants_key(&[a, b]), participants_key(&[a, c]));
    }

    // --- Relationship gate ------------------------------

    #[test]
    fn unrelated_participant_is_denied() {
        // Stands in for both an unrelated real identity and a nonexistent
        // one — neither is ever in either relationship set.
        let stranger = Uuid::new_v4();
        let friends = std::collections::HashSet::new();
        let guild_mates = std::collections::HashSet::new();

        assert!(!all_related_to_caller(&[stranger], &friends, &guild_mates));
    }

    #[test]
    fn friend_participant_is_allowed() {
        let friend = Uuid::new_v4();
        let mut friends = std::collections::HashSet::new();
        friends.insert(friend);
        let guild_mates = std::collections::HashSet::new();

        assert!(all_related_to_caller(&[friend], &friends, &guild_mates));
    }

    #[test]
    fn mutual_guild_participant_is_allowed() {
        let guild_mate = Uuid::new_v4();
        let friends = std::collections::HashSet::new();
        let mut guild_mates = std::collections::HashSet::new();
        guild_mates.insert(guild_mate);

        assert!(all_related_to_caller(&[guild_mate], &friends, &guild_mates));
    }

    #[test]
    fn one_unrelated_participant_denies_the_whole_group() {
        let friend = Uuid::new_v4();
        let stranger = Uuid::new_v4();
        let mut friends = std::collections::HashSet::new();
        friends.insert(friend);
        let guild_mates = std::collections::HashSet::new();

        // Every named participant must clear the gate, not just some.
        assert!(!all_related_to_caller(
            &[friend, stranger],
            &friends,
            &guild_mates
        ));
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

    #[test]
    fn non_participant_and_blocked_participant_are_denied_identically() {
        // A non-participant is denied regardless of blocking.
        assert_eq!(decide_send(false, false), SendDecision::Denied);
        assert_eq!(decide_send(false, true), SendDecision::Denied);
        // A participant with a blocked pair present is *also* denied — the
        // same `SendDecision::Denied` outcome, which the handler maps to
        // the exact same `AppError::NotConversationParticipant`. Nothing
        // downstream of this decision distinguishes the two reasons.
        assert_eq!(decide_send(true, true), SendDecision::Denied);
        // Only a participant with no blocked pair present is allowed.
        assert_eq!(decide_send(true, false), SendDecision::Allowed);
    }

    // The "never touches the ledger/outbox" invariant is checked by a
    // source grep in `crates/server/tests/conversations_no_ledger.rs`
    // rather than here — a test in this file can't grep this file for the
    // absence of a string without that very check string then being
    // present in the file, defeating itself (same note
    // `guild_messages::tests` leaves for its own equivalent).
}
