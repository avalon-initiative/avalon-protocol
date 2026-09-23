//! Friend requests, friendships, and their durable events.
//!
//! Every mutation here requires the caller's own user session — there is
//! no integrator-credential auth path in this repo yet (see `crates/server/src/auth.rs`),
//! so "an integrator cannot act on a user's behalf" is enforced simply by these
//! routes only ever accepting a session bearer token in the first place, not
//! by an explicit per-request check against a credential kind.
//!
//! A friendship is promised-durable per `docs/projects/backend-server/architecture/social-graph.md`:
//! `friend.requested` and `friend.accepted` (or `friend.removed`) are
//! written into the outbox in the same transaction as the `friendships`/
//! `friend_requests` projection change, same pattern `handlers::register_finish`
//! established. A declined or withdrawn *request* is not durable
//! history — resolving it is a plain projection update with no event.
//!
//! Events here are session-authenticated but not yet individually signed —
//! no general per-event signing ceremony exists in this repo, only
//! `identity.created`'s one-off Ed25519 signature. This matches
//! `docs/projects/backend-server/architecture/protocol-events.md`'s "network as signer" milestone-1
//! stand-in, attributed to the acting identity via `issuer` rather than to
//! the node, since the request already proves the actor's session.

use std::collections::HashSet;

use avalon_indexer::projections::friendships as friendship_reads;
use avalon_protocol::event_payloads::{
    FriendAcceptedPayload, FriendRemovedPayload, FriendRequestedPayload,
};
use avalon_protocol::events::{ProtocolEvent, ProtocolEventKindVariant};
use avalon_protocol::ids::GlobalId;
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
use crate::outbox;
use crate::state::AppState;

fn identity_ref(identity_id: Uuid, verb: &str) -> GlobalId {
    GlobalId::new("identity", &identity_id.to_string(), "self", verb)
}

/// Every identity `caller` is currently friends with — one batched query,
/// same shape as `crate::blocks::block_partners`. Used by
/// `presence::get_presence`/`presence::handle_presence_socket` to apply
/// presence's default friends-only visibility scope, pending
/// the full per-resource scope granularity.
pub(crate) async fn friend_partners(
    state: &AppState,
    caller: Uuid,
) -> Result<HashSet<Uuid>, AppError> {
    Ok(friendship_reads::partners_of(&state.pool, caller).await?)
}

fn ordered_pair(x: Uuid, y: Uuid) -> (Uuid, Uuid) {
    if x < y {
        (x, y)
    } else {
        (y, x)
    }
}

#[derive(Serialize, ToSchema)]
pub struct ResolveHandleResponse {
    pub identity_id: Uuid,
}

/// Resolves a `display_name` handle (a globally-unique, case-insensitive
/// `display_name` — no discriminator) to
/// an identity id for the "add friend" flow — exact match only, never
/// partial/fuzzy. Fuzzy name search is a separate, bigger question
/// with its own privacy tradeoffs, deliberately not folded in here.
/// Session-authenticated like every other route in this module, both so an
/// anonymous caller can't use it to enumerate handles and so it matches
/// this module's existing "no integrator-credential auth path" convention.
#[utoipa::path(
    get,
    path = "/friends/handle/{handle}",
    tag = "friends",
    params(("handle" = String, Path)),
    responses((status = 200, body = ResolveHandleResponse)),
)]
pub async fn resolve_handle(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(handle): Path<String>,
) -> Result<Json<ResolveHandleResponse>, AppError> {
    authenticate(&state, &headers).await?;

    let row = sqlx::query("SELECT identity_id FROM profiles WHERE lower(display_name) = lower($1)")
        .bind(&handle)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::HandleNotFound)?;

    Ok(Json(ResolveHandleResponse {
        identity_id: row.try_get("identity_id")?,
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct CreateFriendRequestRequest {
    pub to: Uuid,
}

#[derive(Serialize, ToSchema)]
pub struct FriendRequestResponse {
    pub id: Uuid,
    pub from: Uuid,
    pub to: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub requested_at: OffsetDateTime,
}

#[utoipa::path(
    post,
    path = "/friends/requests",
    tag = "friends",
    request_body = CreateFriendRequestRequest,
    responses((status = 200, body = FriendRequestResponse)),
)]
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

    if friendship_reads::are_friends(&state.pool, from, to).await? {
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
        kind: ProtocolEventKindVariant::FriendRequested
            .as_str()
            .to_string(),
        issuer: identity_ref(from, "friend_requested"),
        subject: identity_ref(to, "friend_requested"),
        payload: serde_json::to_value(FriendRequestedPayload {
            from,
            to,
            actor: from,
        })
        .expect("FriendRequestedPayload should serialize"),
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

#[derive(Serialize, ToSchema)]
pub struct FriendshipResponse {
    pub a: Uuid,
    pub b: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub since: OffsetDateTime,
}

#[utoipa::path(
    post,
    path = "/friends/requests/{id}/accept",
    tag = "friends",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = FriendshipResponse)),
)]
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

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::FriendAccepted
            .as_str()
            .to_string(),
        issuer: identity_ref(actor, "friend_accepted"),
        subject: identity_ref(request.from, "friend_accepted"),
        payload: serde_json::to_value(FriendAcceptedPayload {
            from: request.from,
            to: request.to,
            actor,
        })
        .expect("FriendAcceptedPayload should serialize"),
        timestamp: since,
        version: 1,
    };
    // Issue #506: `friendships` is a projection now — the row is written
    // by the indexer applying `event`, not a bespoke `INSERT` here, same
    // pattern `handlers::register_finish` established for `profiles`.
    state.indexer.apply_in_tx(&mut tx, &event).await?;
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;
    state.indexer.apply_after_commit(&event).await?;

    Ok(Json(FriendshipResponse { a, b, since }))
}

#[utoipa::path(
    delete,
    path = "/friends/requests/{id}",
    tag = "friends",
    params(("id" = Uuid, Path)),
    responses((status = 200, description = "Friend request declined or withdrawn")),
)]
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

#[utoipa::path(
    delete,
    path = "/friends/{identity_id}",
    tag = "friends",
    params(("identity_id" = Uuid, Path)),
    responses((status = 200, description = "Friendship removed")),
)]
pub async fn remove_friend(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(other_identity_id): Path<Uuid>,
) -> Result<(), AppError> {
    let actor = authenticate(&state, &headers).await?;
    let (a, b) = ordered_pair(actor, other_identity_id);

    // A real correctness check, not just advisory: `indexer_friendships`'
    // DELETE is idempotent (`friendships::apply`), so it can't itself tell
    // us whether a row existed — this is what stops an emitted
    // `friend.removed` event from lying about a relationship that was
    // never there.
    if !friendship_reads::are_friends(&state.pool, actor, other_identity_id).await? {
        return Err(AppError::NotFriends);
    }

    let mut tx = state.pool.begin().await?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::FriendRemoved.as_str().to_string(),
        issuer: identity_ref(actor, "friend_removed"),
        subject: identity_ref(other_identity_id, "friend_removed"),
        payload: serde_json::to_value(FriendRemovedPayload { a, b, actor })
            .expect("FriendRemovedPayload should serialize"),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    };
    state.indexer.apply_in_tx(&mut tx, &event).await?;
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;
    state.indexer.apply_after_commit(&event).await?;

    Ok(())
}

#[utoipa::path(
    get,
    path = "/friends",
    tag = "friends",
    responses((status = 200, body = Vec<FriendshipResponse>)),
)]
pub async fn list_friends(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<FriendshipResponse>>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let rows = friendship_reads::list_for(&state.pool, identity_id).await?;
    let friendships = rows
        .into_iter()
        .map(|row| FriendshipResponse {
            a: row.a,
            b: row.b,
            since: row.since,
        })
        .collect();
    Ok(Json(friendships))
}

#[utoipa::path(
    get,
    path = "/friends/requests",
    tag = "friends",
    responses((status = 200, body = Vec<FriendRequestResponse>)),
)]
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
