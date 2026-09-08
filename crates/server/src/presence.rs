//! In-memory presence tracking (issue #16).
//!
//! Ephemeral realtime state per ADR #78
//! (docs/architecture/presence.md): presence is never a `ProtocolEvent`,
//! never touches `crate::outbox` or `avalon_chain`, and never lives in a
//! migrated Postgres table. It is intentionally lost on server restart —
//! everyone reads as `Offline` until their next heartbeat, which is the
//! correct failure mode for state nobody needs to prove later.
//!
//! Two scope cuts from the issue, both deliberate and documented rather
//! than silently missing:
//!
//! - **No game-side publish endpoint.** The issue describes
//!   `PUT /presence/:identity_id` under a `GameCredential`, gated on an
//!   active `GameBinding` and a `presence.publish` capability grant. None
//!   of that exists in this repo yet — there is no game-credential auth
//!   concept, no `GameBinding`, no capability-grant system (#26/#28/#83
//!   are all unbuilt). Only a player's own session can publish their own
//!   presence here, and it can never claim to be `playing` a game.
//! - **No visibility filtering.** `GET /presence` returns exactly the
//!   requested ids' presence to any valid session, with no friends/guild/
//!   private scoping — the same "reads are session-gated only, no
//!   capability/visibility model yet" cut `crates/server/src/friends.rs`
//!   (#15) already established, deferred to #87.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use avalon_protocol::social::PresenceStatus;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::AppError;
use crate::handlers::{authenticate, authenticate_token};
use crate::state::AppState;

/// A published presence entry that hasn't been refreshed within this window
/// reads as `Offline`. Overridable via `AVALON_PRESENCE_TTL_SECS` (see
/// `PresenceStore::from_env`) so integration tests don't need to sleep two
/// real minutes to exercise expiry.
pub const DEFAULT_PRESENCE_TTL: Duration = Duration::from_secs(120);

#[derive(Clone)]
struct PresenceEntry {
    status: PresenceStatus,
    playing: Option<Uuid>,
    updated_at: OffsetDateTime,
    seen_at: Instant,
}

/// Bounded so a burst of publishes can't grow this unboundedly if a
/// subscriber is slow to drain — a lagging receiver drops the oldest
/// messages instead (see `handle_presence_socket`'s `Lagged` handling)
/// rather than the channel growing without limit.
const UPDATE_CHANNEL_CAPACITY: usize = 256;

/// `Arc<RwLock<_>>` around a plain map — deliberately not a migrated table
/// or `ledger_entries`; see module docs. Cheap to clone into `AppState`.
#[derive(Clone)]
pub struct PresenceStore {
    entries: Arc<RwLock<HashMap<Uuid, PresenceEntry>>>,
    ttl: Duration,
    /// Fan-out for `presence::presence_ws` (issue #136) — every `set()`
    /// call also broadcasts the new status, so a subscribed websocket
    /// client learns about a friend coming online without polling
    /// `GET /presence`. A lossy broadcast, not a queue: a slow/absent
    /// subscriber simply misses ticks (or drops the oldest under
    /// `UPDATE_CHANNEL_CAPACITY` pressure) rather than backing up the
    /// publisher — acceptable for ephemeral presence, unlike the outbox's
    /// durable delivery guarantee for real protocol events.
    updates: tokio::sync::broadcast::Sender<PresenceResponse>,
}

impl PresenceStore {
    pub fn new() -> Self {
        Self::with_ttl(DEFAULT_PRESENCE_TTL)
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        let (updates, _) = tokio::sync::broadcast::channel(UPDATE_CHANNEL_CAPACITY);
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            ttl,
            updates,
        }
    }

    /// A new receiver for every presence update published from now on —
    /// each websocket connection gets its own, so one slow connection
    /// lagging never affects another's.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<PresenceResponse> {
        self.updates.subscribe()
    }

    /// Reads `AVALON_PRESENCE_TTL_SECS` if set, otherwise `DEFAULT_PRESENCE_TTL`.
    pub fn from_env() -> Self {
        let ttl = std::env::var("AVALON_PRESENCE_TTL_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_PRESENCE_TTL);
        Self::with_ttl(ttl)
    }

    fn set(
        &self,
        identity_id: Uuid,
        status: PresenceStatus,
        playing: Option<Uuid>,
    ) -> OffsetDateTime {
        let now = OffsetDateTime::now_utc();
        {
            let mut entries = self.entries.write().expect("presence lock poisoned");
            entries.insert(
                identity_id,
                PresenceEntry {
                    status,
                    playing,
                    updated_at: now,
                    seen_at: Instant::now(),
                },
            );
        }
        // No receivers is not an error — most publishes happen with nobody
        // subscribed to that particular identity yet.
        let _ = self.updates.send(PresenceResponse {
            identity_id,
            status,
            playing,
            updated_at: now,
        });
        now
    }

    /// A missing or stale entry reads as `Offline` with no invented history
    /// — never a guess at when the identity was last actually seen.
    fn get(&self, identity_id: Uuid) -> PresenceView {
        let entries = self.entries.read().expect("presence lock poisoned");
        let fresh = entries
            .get(&identity_id)
            .filter(|entry| entry.seen_at.elapsed() < self.ttl);
        match fresh {
            Some(entry) => PresenceView {
                identity_id,
                status: entry.status,
                playing: entry.playing,
                updated_at: entry.updated_at,
            },
            None => PresenceView {
                identity_id,
                status: PresenceStatus::Offline,
                playing: None,
                updated_at: OffsetDateTime::now_utc(),
            },
        }
    }
}

impl Default for PresenceStore {
    fn default() -> Self {
        Self::new()
    }
}

struct PresenceView {
    identity_id: Uuid,
    status: PresenceStatus,
    playing: Option<Uuid>,
    updated_at: OffsetDateTime,
}

#[derive(Serialize, Clone)]
pub struct PresenceResponse {
    pub identity_id: Uuid,
    pub status: PresenceStatus,
    pub playing: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl From<PresenceView> for PresenceResponse {
    fn from(view: PresenceView) -> Self {
        Self {
            identity_id: view.identity_id,
            status: view.status,
            playing: view.playing,
            updated_at: view.updated_at,
        }
    }
}

#[derive(Deserialize)]
pub struct UpdatePresenceRequest {
    pub status: PresenceStatus,
}

/// `PUT /me/presence` — a player publishing their own status. Deliberately
/// cannot set `playing`: that's reserved for a game's own credential once
/// game-side publishing exists (see module docs).
pub async fn update_my_presence(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UpdatePresenceRequest>,
) -> Result<Json<PresenceResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;
    let updated_at = state.presence.set(identity_id, body.status, None);
    Ok(Json(PresenceResponse {
        identity_id,
        status: body.status,
        playing: None,
        updated_at,
    }))
}

#[derive(Deserialize)]
pub struct PresenceQuery {
    /// Comma-separated identity ids, e.g. `?ids=<uuid>,<uuid>`.
    pub ids: String,
}

/// `GET /presence?ids=…` — session-authenticated, no visibility filtering
/// yet (see module docs, deferred to #87). Any valid session may look up
/// presence for any ids it names.
pub async fn get_presence(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PresenceQuery>,
) -> Result<Json<Vec<PresenceResponse>>, AppError> {
    let caller = authenticate(&state, &headers).await?;

    let ids: Vec<Uuid> = query
        .ids
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<Uuid>()
                .map_err(|_| AppError::InvalidPresenceQuery)
        })
        .collect::<Result<_, _>>()?;

    // Issue #97: a blocked identity's presence reads exactly like a
    // missing/stale entry (Offline, `updated_at: now`) — never a
    // distinguishable "hidden" state, matching this store's own existing
    // "never a guess" precedent for a genuinely missing entry.
    let blocked_partners = crate::blocks::block_partners(&state, caller).await?;
    let views = ids
        .into_iter()
        .map(|id| {
            if blocked_partners.contains(&id) {
                PresenceResponse {
                    identity_id: id,
                    status: PresenceStatus::Offline,
                    playing: None,
                    updated_at: OffsetDateTime::now_utc(),
                }
            } else {
                state.presence.get(id).into()
            }
        })
        .collect();
    Ok(Json(views))
}

/// `token` is a query parameter, not a header — the issue #136 tradeoff a
/// websocket upgrade forces: the browser `WebSocket` constructor has no way
/// to set an `Authorization` header on the handshake request the way every
/// other route in this crate expects. See `handlers::authenticate_token`'s
/// own doc comment.
#[derive(Deserialize)]
pub struct PresenceWsQuery {
    pub token: String,
}

/// `GET /ws/presence?token=…` — issue #136's live push transport, additive
/// to `GET /presence` above, not a replacement for it. Authenticates before
/// upgrading (a bad/missing token gets a real 401, not a socket that opens
/// and then silently closes) and hands off to `handle_presence_socket` for
/// the connection's lifetime. Same "no visibility filtering yet" cut as
/// `GET /presence` (deferred to #87, see module docs) — any valid session
/// may subscribe to any ids it names.
pub async fn presence_ws(
    State(state): State<AppState>,
    Query(query): Query<PresenceWsQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, AppError> {
    let caller = authenticate_token(&state, &query.token).await?;
    Ok(ws.on_upgrade(move |socket| handle_presence_socket(socket, state, caller)))
}

/// What a subscribed client can send. `Subscribe` is additive — sending it
/// again with more ids grows the connection's subscription set rather than
/// replacing it, so a client doesn't need to remember and resend its whole
/// friends list every time it adds one.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Subscribe { ids: Vec<Uuid> },
}

/// Forces a presence view for `id` to `Offline` — issue #97: a blocked
/// identity must read exactly like a missing/stale entry, never a
/// distinguishable "hidden" state.
fn offline_view(id: Uuid) -> PresenceResponse {
    PresenceResponse {
        identity_id: id,
        status: PresenceStatus::Offline,
        playing: None,
        updated_at: OffsetDateTime::now_utc(),
    }
}

async fn handle_presence_socket(mut socket: WebSocket, state: AppState, caller: Uuid) {
    // Loaded once per connection, not per message — see
    // `blocks::block_partners`'s own doc comment for the staleness
    // tradeoff this accepts.
    let blocked_partners = match crate::blocks::block_partners(&state, caller).await {
        Ok(set) => set,
        Err(_) => return,
    };
    let mut subscribed: HashSet<Uuid> = HashSet::new();
    let mut updates = state.presence.subscribe();

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(ClientMessage::Subscribe { ids }) = serde_json::from_str(&text) else {
                            continue;
                        };
                        // Send a catch-up snapshot for each newly-subscribed
                        // id immediately, rather than making the client wait
                        // for that identity's next publish to learn its
                        // current status.
                        for id in ids {
                            if subscribed.insert(id) {
                                let view: PresenceResponse = if blocked_partners.contains(&id) {
                                    offline_view(id)
                                } else {
                                    state.presence.get(id).into()
                                };
                                let payload =
                                    serde_json::to_string(&view).expect("PresenceResponse always serializes");
                                if socket.send(Message::Text(payload)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                    Some(Ok(_)) => {}
                }
            }
            update = updates.recv() => {
                match update {
                    Ok(update) if subscribed.contains(&update.identity_id) => {
                        let view = if blocked_partners.contains(&update.identity_id) {
                            offline_view(update.identity_id)
                        } else {
                            update
                        };
                        let payload =
                            serde_json::to_string(&view).expect("PresenceResponse always serializes");
                        if socket.send(Message::Text(payload)).await.is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    // A slow consumer missed some ticks — its next
                    // subscribe (or a plain `GET /presence` poll) catches
                    // it back up; dropping interim ticks is the documented
                    // tradeoff of a lossy broadcast channel, not a bug.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — these exercise the in-memory
    //! store's own logic. The publish/read HTTP flow is covered by
    //! `crates/server/tests/presence.rs`, gated `--ignored`.

    use super::*;

    #[test]
    fn fresh_entry_reads_as_its_own_status() {
        let store = PresenceStore::with_ttl(Duration::from_secs(60));
        let id = Uuid::new_v4();
        store.set(id, PresenceStatus::Online, None);
        let view = store.get(id);
        assert_eq!(view.status, PresenceStatus::Online);
    }

    #[test]
    fn stale_entry_reads_as_offline() {
        let store = PresenceStore::with_ttl(Duration::from_millis(10));
        let id = Uuid::new_v4();
        store.set(id, PresenceStatus::Online, None);
        std::thread::sleep(Duration::from_millis(30));
        let view = store.get(id);
        assert_eq!(view.status, PresenceStatus::Offline);
    }

    #[test]
    fn missing_entry_reads_as_offline() {
        let store = PresenceStore::with_ttl(Duration::from_secs(60));
        let view = store.get(Uuid::new_v4());
        assert_eq!(view.status, PresenceStatus::Offline);
    }

    // ADR #78's hard rule — presence never touches the outbox or the chain
    // crate — is enforced by this module's imports alone (neither is
    // imported above) rather than by a self-referential source grep here.
}
