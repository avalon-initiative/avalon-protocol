//! Blocking — private, unilateral, server-side application state.
//!
//! Never a protocol/settlement event and never a `crates/protocol` type,
//! unlike friendship — see `crates/server/db/migrations/0006_blocks/up.sql`
//! and `docs/architecture/social-graph.md` for the full "why" (privacy: the
//! settlement log is a public transparency log anyone can mirror, and
//! nobody ever needs to verify a block the way an integrator verifies an
//! attestation). Losing this table means users re-block people — the
//! same acceptable failure mode presence already has, not a fact
//! anyone needs to prove later.
//!
//! **The load-bearing invariant: the blocked party is never told, through
//! any endpoint, not even indirectly.** Every check elsewhere in this crate
//! that consults a block (`friends::create_friend_request`,
//! `presence::get_presence`/`presence_ws`) treats "blocked" identically to
//! an ordinary, unrelated rejection reason or an ordinary offline/missing
//! presence entry — never a distinguishable error code or presence state.
//! `has_block_between`/`block_partners` below are the only two places that
//! query, so every caller gets the exact same answer shape.

use std::collections::HashSet;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::AppError;
use crate::handlers::authenticate;
use crate::state::AppState;

/// True if `a` has blocked `b` OR `b` has blocked `a` — direction never
/// matters to a caller asking "should these two interact." Used by
/// `friends::create_friend_request`, a one-off, request-scoped check.
pub(crate) async fn has_block_between(
    state: &AppState,
    a: Uuid,
    b: Uuid,
) -> Result<bool, AppError> {
    let row = sqlx::query(
        "SELECT 1 FROM blocks WHERE (blocker = $1 AND blocked = $2) OR (blocker = $2 AND blocked = $1)",
    )
    .bind(a)
    .bind(b)
    .fetch_optional(&state.pool)
    .await?;
    Ok(row.is_some())
}

/// Every identity `caller` currently has a block relationship with, in
/// either direction — one batched query instead of one `has_block_between`
/// call per id, used by `presence::get_presence` (one request, many ids)
/// and `presence::handle_presence_socket` (loaded once per connection
/// rather than once per message/tick). For the websocket case this means a
/// block created or removed mid-connection isn't reflected until the
/// client reconnects/resubscribes — an acceptable staleness window for a
/// presence connection, the same "eventually consistent, not
/// instantaneous" tradeoff issue #136's own lossy broadcast channel
/// already accepts.
pub(crate) async fn block_partners(
    state: &AppState,
    caller: Uuid,
) -> Result<HashSet<Uuid>, AppError> {
    let rows = sqlx::query(
        r#"
        SELECT blocked AS other FROM blocks WHERE blocker = $1
        UNION
        SELECT blocker AS other FROM blocks WHERE blocked = $1
        "#,
    )
    .bind(caller)
    .fetch_all(&state.pool)
    .await?;
    let mut set = HashSet::with_capacity(rows.len());
    for row in rows {
        set.insert(row.try_get("other")?);
    }
    Ok(set)
}

/// True if any block row (either direction) exists entirely *within*
/// `participants` — i.e. some pair `(a, b)` both present in the slice has a
/// block between them. Used by `crate::conversations::send_message`
/// to enforce blocking across a group conversation: unlike
/// [`has_block_between`]'s fixed pair, a conversation can have more than
/// two participants, so the check has to be "does a blocked pair exist
/// anywhere in this set," not just "are the two request-scoped identities
/// blocked." One query rather than an O(n^2) loop of [`has_block_between`]
/// calls — Postgres does the pairwise check via `blocker = ANY($1) AND
/// blocked = ANY($1)`.
pub(crate) async fn has_block_among(
    state: &AppState,
    participants: &[Uuid],
) -> Result<bool, AppError> {
    let row =
        sqlx::query("SELECT 1 FROM blocks WHERE blocker = ANY($1) AND blocked = ANY($1) LIMIT 1")
            .bind(participants)
            .fetch_optional(&state.pool)
            .await?;
    Ok(row.is_some())
}

#[derive(Deserialize, ToSchema)]
pub struct CreateBlockRequest {
    pub identity_id: Uuid,
}

#[derive(Serialize, ToSchema)]
pub struct BlockResponse {
    pub blocked: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub created_at: OffsetDateTime,
}

/// `POST /blocks` — session-authenticated, blocker-only. Also resolves any
/// pending friend request between the two, in either direction, as part of
/// the same transaction — the ticket's own acceptance criteria requires a
/// pending request be auto-rejected the moment a block is created. Reuses
/// `friend_requests`' existing `'withdrawn'` outcome rather than adding a
/// new one: it's a projection update, not durable history, the same
/// treatment `friends::decline_or_withdraw_friend_request` already gives a
/// resolved request, and a fourth precise outcome label isn't worth a
/// schema change for what's an internal bookkeeping value never surfaced
/// to either party as "resolved because of a block."
#[utoipa::path(
    post,
    path = "/blocks",
    tag = "blocks",
    request_body = CreateBlockRequest,
    responses((status = 200, body = BlockResponse)),
)]
pub async fn create_block(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateBlockRequest>,
) -> Result<Json<BlockResponse>, AppError> {
    let blocker = authenticate(&state, &headers).await?;
    let blocked = body.identity_id;

    if blocker == blocked {
        return Err(AppError::SelfBlock);
    }

    let mut tx = state.pool.begin().await?;

    let created_at = OffsetDateTime::now_utc();
    let inserted =
        sqlx::query("INSERT INTO blocks (blocker, blocked, created_at) VALUES ($1, $2, $3)")
            .bind(blocker)
            .bind(blocked)
            .bind(created_at)
            .execute(&mut *tx)
            .await;
    if let Err(sqlx::Error::Database(db_err)) = &inserted {
        if db_err.is_unique_violation() {
            return Err(AppError::AlreadyBlocked);
        }
    }
    inserted?;

    sqlx::query(
        r#"
        UPDATE friend_requests
        SET resolved_at = now(), outcome = 'withdrawn'
        WHERE resolved_at IS NULL
          AND (("from" = $1 AND "to" = $2) OR ("from" = $2 AND "to" = $1))
        "#,
    )
    .bind(blocker)
    .bind(blocked)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Json(BlockResponse {
        blocked,
        created_at,
    }))
}

/// `DELETE /blocks/:identity_id` — session-authenticated, blocker-only.
/// Simply removes the row: no history, no event, mirrors presence's
/// ephemerality rather than friendship's durability (module docs).
#[utoipa::path(
    delete,
    path = "/blocks/{identity_id}",
    tag = "blocks",
    params(("identity_id" = Uuid, Path)),
    responses((status = 200, description = "Block removed")),
)]
pub async fn remove_block(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(blocked): Path<Uuid>,
) -> Result<(), AppError> {
    let blocker = authenticate(&state, &headers).await?;
    let removed = sqlx::query("DELETE FROM blocks WHERE blocker = $1 AND blocked = $2")
        .bind(blocker)
        .bind(blocked)
        .execute(&state.pool)
        .await?;
    if removed.rows_affected() == 0 {
        return Err(AppError::BlockNotFound);
    }
    Ok(())
}

#[derive(Serialize, ToSchema)]
pub struct BlockListEntry {
    pub blocked: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub created_at: OffsetDateTime,
}

/// `GET /blocks` — the caller's own block list, and *only* the caller's own
/// — this endpoint (and every endpoint in this crate) never returns "who
/// has blocked me" for any identity, per this module's own invariant.
#[utoipa::path(
    get,
    path = "/blocks",
    tag = "blocks",
    responses((status = 200, body = Vec<BlockListEntry>)),
)]
pub async fn list_blocks(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<BlockListEntry>>, AppError> {
    let blocker = authenticate(&state, &headers).await?;
    let rows = sqlx::query("SELECT blocked, created_at FROM blocks WHERE blocker = $1")
        .bind(blocker)
        .fetch_all(&state.pool)
        .await?;

    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        entries.push(BlockListEntry {
            blocked: row.try_get("blocked")?,
            created_at: row.try_get("created_at")?,
        });
    }
    Ok(Json(entries))
}

/// Pure mirror of [`has_block_among`]'s "does a blocked pair exist entirely
/// within this set" query, so the group-conversation semantics can be unit
/// tested without a live Postgres — see [`tests`] below. Not called from
/// the request path; `#[cfg(test)]` only.
#[cfg(test)]
fn any_pair_blocked(blocks: &[(Uuid, Uuid)], participants: &[Uuid]) -> bool {
    blocks
        .iter()
        .any(|(a, b)| participants.contains(a) && participants.contains(b))
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here. The full block/enforcement flow
    //! (presence hidden in both directions, friend requests rejected
    //! indistinguishably from a nonexistent identity, unblock restoring
    //! access) is covered by `crates/server/tests/blocks.rs`, gated
    //! `--ignored`. `has_block_among` gets a pure-logic model
    //! below since its "any pair within an arbitrary-size set" semantics
    //! are worth exercising directly; every other function here is a thin,
    //! directly-verified SQL wrapper, unlike e.g. `friends::ordered_pair`.

    use super::*;

    #[test]
    fn any_pair_blocked_finds_a_block_between_two_non_adjacent_participants() {
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let carol = Uuid::new_v4();
        let dave = Uuid::new_v4();

        // alice blocked dave; neither is directly "adjacent" in the
        // group's natural ordering — the check still has to find it.
        let blocks = vec![(alice, dave)];
        let participants = vec![alice, bob, carol, dave];
        assert!(any_pair_blocked(&blocks, &participants));
    }

    #[test]
    fn any_pair_blocked_ignores_a_block_involving_someone_outside_the_set() {
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let outsider = Uuid::new_v4();

        let blocks = vec![(alice, outsider)];
        let participants = vec![alice, bob];
        assert!(!any_pair_blocked(&blocks, &participants));
    }

    #[test]
    fn any_pair_blocked_is_false_with_no_blocks_at_all() {
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        assert!(!any_pair_blocked(&[], &[alice, bob]));
    }
}
