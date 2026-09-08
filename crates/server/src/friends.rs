//! Friend requests, friendships, and their durable events (issue #15).
//!
//! Every mutation here requires the caller's own player session — there is
//! no game-credential auth path in this repo yet (see `crates/server/src/auth.rs`),
//! so "a game cannot act on a player's behalf" is enforced simply by these
//! routes only ever accepting a session bearer token in the first place, not
//! by an explicit per-request check against a credential kind.
//!
//! A friendship is promised-durable per `docs/architecture/social-graph.md`:
//! `friend.requested` and `friend.accepted` (or `friend.removed`) are
//! written into the outbox in the same transaction as the `friendships`/
//! `friend_requests` projection change, same pattern `handlers::register_finish`
//! established for #71. A declined or withdrawn *request* is not durable
//! history — resolving it is a plain projection update with no event.
//!
//! Events here are session-authenticated but not yet individually signed —
//! no general per-event signing ceremony exists in this repo, only
//! `identity.created`'s one-off Ed25519 signature (#73). This matches
//! `docs/architecture/protocol-events.md`'s "network as signer" milestone-1
//! stand-in, attributed to the acting identity via `issuer` rather than to
//! the node, since the request already proves the actor's session.

use avalon_protocol::events::ProtocolEvent;
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

fn identity_ref(identity_id: Uuid, verb: &str) -> GlobalId {
    GlobalId::new("identity", &identity_id.to_string(), "self", verb)
}

fn ordered_pair(x: Uuid, y: Uuid) -> (Uuid, Uuid) {
    if x < y {
        (x, y)
    } else {
        (y, x)
    }
}

#[derive(Serialize)]
pub struct ResolveHandleResponse {
    pub identity_id: Uuid,
}

/// Resolves a `display_name#1234` handle (issue #128) to an identity id for
/// the "add friend" flow — exact match only, never partial/fuzzy. Fuzzy
/// name search is a separate, bigger question (issue #129) with its own
/// privacy tradeoffs, deliberately not folded in here. Session-authenticated
/// like every other route in this module, both so an anonymous caller can't
/// use it to enumerate handles and so it matches this module's existing
/// "no game-credential auth path" convention.
pub async fn resolve_handle(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(handle): Path<String>,
) -> Result<Json<ResolveHandleResponse>, AppError> {
    authenticate(&state, &headers).await?;

    let (display_name, discriminator) = handle.rsplit_once('#').ok_or(AppError::HandleNotFound)?;

    let row = sqlx::query(
        "SELECT identity_id FROM profiles WHERE display_name = $1 AND discriminator = $2",
    )
    .bind(display_name)
    .bind(discriminator)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::HandleNotFound)?;

    Ok(Json(ResolveHandleResponse {
        identity_id: row.try_get("identity_id")?,
    }))
}

#[derive(Deserialize)]
pub struct CreateFriendRequestRequest {
    pub to: Uuid,
}

#[derive(Serialize)]
pub struct FriendRequestResponse {
    pub id: Uuid,
    pub from: Uuid,
    pub to: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub requested_at: OffsetDateTime,
}

pub async fn create_friend_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateFriendRequestRequest>,
) -> Result<Json<FriendRequestResponse>, AppError> {
    let from = authenticate(&state, &headers).await?;
    let to = body.to;

    if from == to {
        return Err(AppError::SelfFriendRequest);
    }

    let target_exists = sqlx::query("SELECT 1 FROM identities WHERE id = $1")
        .bind(to)
        .fetch_optional(&state.pool)
        .await?;
    if target_exists.is_none() {
        return Err(AppError::IdentityNotFound);
    }

    // Same error as a nonexistent identity, deliberately — issue #97's
    // "the blocked party is never told, not even indirectly" invariant
    // means a block and a missing identity must be indistinguishable from
    // this endpoint's response alone.
    if crate::blocks::has_block_between(&state, from, to).await? {
        return Err(AppError::IdentityNotFound);
    }

    let (a, b) = ordered_pair(from, to);
    let already_friends = sqlx::query("SELECT 1 FROM friendships WHERE a = $1 AND b = $2")
        .bind(a)
        .bind(b)
        .fetch_optional(&state.pool)
        .await?;
    if already_friends.is_some() {
        return Err(AppError::AlreadyFriends);
    }

    let mut tx = state.pool.begin().await?;

    let request_id = Uuid::new_v4();
    let requested_at = OffsetDateTime::now_utc();
    let inserted = sqlx::query(
        r#"
        INSERT INTO friend_requests (id, "from", "to", requested_at)
        VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(request_id)
    .bind(from)
    .bind(to)
    .bind(requested_at)
    .execute(&mut *tx)
    .await;
    if let Err(sqlx::Error::Database(db_err)) = &inserted {
        if db_err.is_unique_violation() {
            return Err(AppError::FriendRequestExists);
        }
    }
    inserted?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "friend.requested".to_string(),
        issuer: identity_ref(from, "friend_requested"),
        subject: identity_ref(to, "friend_requested"),
        payload: serde_json::json!({ "from": from, "to": to, "actor": from }),
        timestamp: requested_at,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(FriendRequestResponse {
        id: request_id,
        from,
        to,
        requested_at,
    }))
}

struct PendingRequest {
    from: Uuid,
    to: Uuid,
}

async fn fetch_pending_request(
    state: &AppState,
    request_id: Uuid,
) -> Result<PendingRequest, AppError> {
    let row = sqlx::query(
        r#"SELECT "from", "to" FROM friend_requests WHERE id = $1 AND resolved_at IS NULL"#,
    )
    .bind(request_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::FriendRequestNotFound)?;
    Ok(PendingRequest {
        from: row.try_get("from")?,
        to: row.try_get("to")?,
    })
}

#[derive(Serialize)]
pub struct FriendshipResponse {
    pub a: Uuid,
    pub b: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
}

pub async fn accept_friend_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(request_id): Path<Uuid>,
) -> Result<Json<FriendshipResponse>, AppError> {
    let actor = authenticate(&state, &headers).await?;
    let request = fetch_pending_request(&state, request_id).await?;
    // Only the recipient can accept — the requester consented by asking;
    // consent from the other side is exactly what "accept" means.
    if actor != request.to {
        return Err(AppError::FriendRequestNotFound);
    }

    let mut tx = state.pool.begin().await?;

    let resolved = sqlx::query(
        "UPDATE friend_requests SET resolved_at = now(), outcome = 'accepted' WHERE id = $1",
    )
    .bind(request_id)
    .execute(&mut *tx)
    .await?;
    if resolved.rows_affected() == 0 {
        // Resolved by a concurrent request between the fetch above and here.
        return Err(AppError::FriendRequestNotFound);
    }

    let (a, b) = ordered_pair(request.from, request.to);
    let since = OffsetDateTime::now_utc();
    sqlx::query("INSERT INTO friendships (a, b, since) VALUES ($1, $2, $3)")
        .bind(a)
        .bind(b)
        .bind(since)
        .execute(&mut *tx)
        .await?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "friend.accepted".to_string(),
        issuer: identity_ref(actor, "friend_accepted"),
        subject: identity_ref(request.from, "friend_accepted"),
        payload: serde_json::json!({ "from": request.from, "to": request.to, "actor": actor }),
        timestamp: since,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(FriendshipResponse { a, b, since }))
}

pub async fn decline_or_withdraw_friend_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(request_id): Path<Uuid>,
) -> Result<(), AppError> {
    let actor = authenticate(&state, &headers).await?;
    let request = fetch_pending_request(&state, request_id).await?;
    if actor != request.from && actor != request.to {
        return Err(AppError::FriendRequestNotFound);
    }

    // Not durable history — see module docs. A single projection update,
    // no outbox entry, no transaction needed.
    let outcome = if actor == request.from {
        "withdrawn"
    } else {
        "declined"
    };
    sqlx::query("UPDATE friend_requests SET resolved_at = now(), outcome = $2 WHERE id = $1")
        .bind(request_id)
        .bind(outcome)
        .execute(&state.pool)
        .await?;

    Ok(())
}

pub async fn remove_friend(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(other_identity_id): Path<Uuid>,
) -> Result<(), AppError> {
    let actor = authenticate(&state, &headers).await?;
    let (a, b) = ordered_pair(actor, other_identity_id);

    let mut tx = state.pool.begin().await?;

    let removed = sqlx::query("DELETE FROM friendships WHERE a = $1 AND b = $2")
        .bind(a)
        .bind(b)
        .execute(&mut *tx)
        .await?;
    if removed.rows_affected() == 0 {
        return Err(AppError::NotFriends);
    }

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "friend.removed".to_string(),
        issuer: identity_ref(actor, "friend_removed"),
        subject: identity_ref(other_identity_id, "friend_removed"),
        payload: serde_json::json!({ "a": a, "b": b, "actor": actor }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(())
}

pub async fn list_friends(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<FriendshipResponse>>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let rows = sqlx::query("SELECT a, b, since FROM friendships WHERE a = $1 OR b = $1")
        .bind(identity_id)
        .fetch_all(&state.pool)
        .await?;

    let mut friendships = Vec::with_capacity(rows.len());
    for row in rows {
        friendships.push(FriendshipResponse {
            a: row.try_get("a")?,
            b: row.try_get("b")?,
            since: row.try_get("since")?,
        });
    }
    Ok(Json(friendships))
}

pub async fn list_friend_requests(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<FriendRequestResponse>>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let rows = sqlx::query(
        r#"
        SELECT id, "from", "to", requested_at FROM friend_requests
        WHERE ("from" = $1 OR "to" = $1) AND resolved_at IS NULL
        ORDER BY requested_at
        "#,
    )
    .bind(identity_id)
    .fetch_all(&state.pool)
    .await?;

    let mut requests = Vec::with_capacity(rows.len());
    for row in rows {
        requests.push(FriendRequestResponse {
            id: row.try_get("id")?,
            from: row.try_get("from")?,
            to: row.try_get("to")?,
            requested_at: row.try_get("requested_at")?,
        });
    }
    Ok(Json(requests))
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — these exercise the pure logic only.
    //! The request→accept/decline/remove flow itself is covered by
    //! `crates/server/tests/friends.rs`, gated `--ignored`.

    use super::*;

    #[test]
    fn ordered_pair_is_stable_regardless_of_argument_order() {
        let x = Uuid::new_v4();
        let y = Uuid::new_v4();
        assert_eq!(ordered_pair(x, y), ordered_pair(y, x));
        let (a, b) = ordered_pair(x, y);
        assert!(a < b);
    }

    #[test]
    fn identity_ref_namespaces_by_identity_and_verb() {
        let id = Uuid::new_v4();
        let global_id = identity_ref(id, "friend_requested");
        assert_eq!(
            global_id.as_str(),
            format!("identity:{id}:self:friend_requested")
        );
    }
}
