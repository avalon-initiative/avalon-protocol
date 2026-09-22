//! Presence tracking: publish + read (issue #16). Ephemeral realtime
//! state per ADR #78 — never a `ProtocolEvent`, never durable, lost on
//! restart. See `docs/architecture/presence.md`'s "Rules" and "Today in
//! the repo" sections for the publish/read auth model, sticky manual
//! overrides, and the durable `hide_active_in` opt-out.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use avalon_protocol::social::PresenceStatus;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::authz::{authenticate_caller, require_capability, Caller};
use crate::error::AppError;
use crate::handlers::{authenticate, authenticate_token};
use crate::state::AppState;
use avalon_protocol::permissions::{Capability, Visibility};
use utoipa::ToSchema;

/// A published presence entry that hasn't been refreshed within this window
/// reads as `Offline`. Overridable via `AVALON_PRESENCE_TTL_SECS` (see
/// `PresenceStore::from_env`) so integration tests don't need to sleep two
/// real minutes to exercise expiry.
pub const DEFAULT_PRESENCE_TTL: Duration = Duration::from_secs(120);

#[derive(Clone)]
struct PresenceEntry {
    status: PresenceStatus,
    active_in: Option<Uuid>,
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
        active_in: Option<Uuid>,
    ) -> OffsetDateTime {
        let now = OffsetDateTime::now_utc();
        self.apply(identity_id, status, active_in, now);
        now
    }

    /// Issue #539: applies a presence update this node received via
    /// cross-node relay (`crate::realtime_relay`), rather than one
    /// published directly by a caller of this node. Never itself
    /// triggers another relay — `realtime_relay::relay_handler` calls
    /// this instead of `set` precisely so a relayed update updates this
    /// node's own local store/broadcast without bouncing back out again.
    /// `updated_at` is the *origin* node's timestamp, preserved as-is
    /// rather than re-stamped with this node's own clock — `seen_at`
    /// (this node's own TTL clock) is still `Instant::now()`, since TTL
    /// expiry is inherently a per-node concept.
    pub fn apply_relayed(
        &self,
        identity_id: Uuid,
        status: PresenceStatus,
        active_in: Option<Uuid>,
        updated_at: OffsetDateTime,
    ) {
        self.apply(identity_id, status, active_in, updated_at);
    }

    fn apply(
        &self,
        identity_id: Uuid,
        status: PresenceStatus,
        active_in: Option<Uuid>,
        updated_at: OffsetDateTime,
    ) {
        {
            let mut entries = self.entries.write().expect("presence lock poisoned");
            entries.insert(
                identity_id,
                PresenceEntry {
                    status,
                    active_in,
                    updated_at,
                    seen_at: Instant::now(),
                },
            );
        }
        // No receivers is not an error — most publishes happen with nobody
        // subscribed to that particular identity yet.
        let _ = self.updates.send(PresenceResponse {
            identity_id,
            status,
            active_in,
            updated_at,
        });
    }

    /// A missing entry reads as `Offline` with no invented history — never
    /// a guess at when the identity was last actually seen.
    ///
    /// A present entry is either live or a sticky manual override, decided
    /// by its own last explicitly-set `status` — no separate override
    /// flag/column: `Online` is the one status this store ever computes
    /// automatically, so "last explicit status was `Online`" is exactly
    /// "let TTL govern this entry" and anything else is exactly "an
    /// override is active". Concretely: `Online` past `self.ttl` since its
    /// last publish expires to `Offline`, same as always. `Away`,
    /// `DoNotDisturb`, and `Offline`, once explicitly set via
    /// `PresenceStore::set`, are reported as-is regardless of `seen_at` —
    /// they stay stuck until a caller explicitly `set`s `Online` again,
    /// which immediately resumes live TTL tracking. This is the Discord-
    /// style "sticky manual override" behavior; see
    /// `avalon_protocol::social::PresenceStatus`'s doc comment.
    fn get(&self, identity_id: Uuid) -> PresenceView {
        let entries = self.entries.read().expect("presence lock poisoned");
        match entries.get(&identity_id) {
            Some(entry) => {
                let expired = entry.seen_at.elapsed() >= self.ttl;
                if expired && entry.status == PresenceStatus::Online {
                    PresenceView {
                        identity_id,
                        status: PresenceStatus::Offline,
                        active_in: None,
                        updated_at: OffsetDateTime::now_utc(),
                    }
                } else {
                    PresenceView {
                        identity_id,
                        status: entry.status,
                        active_in: entry.active_in,
                        updated_at: entry.updated_at,
                    }
                }
            }
            None => PresenceView {
                identity_id,
                status: PresenceStatus::Offline,
                active_in: None,
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
    active_in: Option<Uuid>,
    updated_at: OffsetDateTime,
}

#[derive(Serialize, Deserialize, Clone, ToSchema)]
pub struct PresenceResponse {
    pub identity_id: Uuid,
    pub status: PresenceStatus,
    pub active_in: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub updated_at: OffsetDateTime,
}

impl From<PresenceView> for PresenceResponse {
    fn from(view: PresenceView) -> Self {
        Self {
            identity_id: view.identity_id,
            status: view.status,
            active_in: view.active_in,
            updated_at: view.updated_at,
        }
    }
}

/// Reads `presence_preferences.hide_active_in` for every id in `ids` in one
/// batched query, returning the set of ids that currently have it set.
/// Absence (no row at all) means "not hidden" — see module doc comment —
/// so the caller only ever needs the positive set, never a full map with
/// defaults filled in.
async fn hide_active_in_for(state: &AppState, ids: &[Uuid]) -> Result<HashSet<Uuid>, AppError> {
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    let rows = sqlx::query(
        "SELECT identity_id FROM presence_preferences WHERE identity_id = ANY($1) \
         AND hide_active_in = true",
    )
    .bind(ids)
    .fetch_all(&state.pool)
    .await?;
    let mut set = HashSet::with_capacity(rows.len());
    for row in rows {
        set.insert(row.try_get("identity_id")?);
    }
    Ok(set)
}

/// Reads `profiles.presence_visibility` for every id in `ids` in one
/// batched query (issue #87). An id with no `profiles` row (shouldn't
/// happen for a real identity, but never assumed) falls back to
/// `Visibility::Friends` — this endpoint's own original hardcoded
/// default, preserved rather than silently becoming more permissive for a
/// row this lookup can't find.
async fn presence_visibility_for(
    state: &AppState,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, Visibility>, AppError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query(
        "SELECT identity_id, presence_visibility FROM profiles WHERE identity_id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(&state.pool)
    .await?;
    let mut map = HashMap::with_capacity(rows.len());
    for row in rows {
        let identity_id: Uuid = row.try_get("identity_id")?;
        let raw: String = row.try_get("presence_visibility")?;
        map.insert(identity_id, crate::visibility::parse_visibility(&raw));
    }
    Ok(map)
}

/// Upserts `identity_id`'s own `hide_active_in` preference — the durable
/// half of `PUT /me/presence`, see module doc comment for why this isn't
/// part of the ephemeral `PresenceStore`.
async fn set_hide_active_in(
    state: &AppState,
    identity_id: Uuid,
    hide: bool,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO presence_preferences (identity_id, hide_active_in) VALUES ($1, $2) \
         ON CONFLICT (identity_id) DO UPDATE SET hide_active_in = EXCLUDED.hide_active_in",
    )
    .bind(identity_id)
    .bind(hide)
    .execute(&state.pool)
    .await?;
    Ok(())
}

#[derive(Deserialize, ToSchema)]
pub struct UpdatePresenceRequest {
    pub status: PresenceStatus,
    /// Opt out of (or back into) `active_in` ever being shown, independent
    /// of any integrator's `presence.publish` grant. `None` leaves the existing
    /// preference untouched — this endpoint publishes a status on every
    /// call, but the caller doesn't have to re-state its opt-out choice
    /// every heartbeat.
    #[serde(default)]
    pub hide_active_in: Option<bool>,
}

/// `PUT /me/presence` — a user publishing their own status. Deliberately
/// cannot set `active_in`: that's reserved for an integrator's own credential
/// (`update_integrator_presence` below).
#[utoipa::path(
    put,
    path = "/me/presence",
    tag = "presence",
    request_body = UpdatePresenceRequest,
    responses((status = 200, body = PresenceResponse)),
)]
pub async fn update_my_presence(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UpdatePresenceRequest>,
) -> Result<Json<PresenceResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;
    if let Some(hide) = body.hide_active_in {
        set_hide_active_in(&state, identity_id, hide).await?;
    }
    let updated_at = state.presence.set(identity_id, body.status, None);
    let response = PresenceResponse {
        identity_id,
        status: body.status,
        active_in: None,
        updated_at,
    };
    // Issue #539: reach subscribers connected to a different node, not
    // just this one — spawned, never awaited inline, so an unreachable
    // peer never delays this response.
    tokio::spawn(crate::realtime_relay::relay_to_peers(
        state.clone(),
        crate::realtime_relay::RelayEvent::Presence(response.clone()),
    ));
    Ok(Json(response))
}

/// The one rule an integrator's presence claim must satisfy: `active_in`, if set at
/// all, must name the calling integrator's own id. Pure and DB-free so it's
/// directly unit-testable — see this module's tests below — separate from
/// `require_capability`'s binding/grant check, which does need the
/// database.
fn validate_integrator_playing(
    caller_integrator_id: Uuid,
    active_in: Option<Uuid>,
) -> Result<(), AppError> {
    match active_in {
        Some(integrator_id) if integrator_id != caller_integrator_id => {
            Err(AppError::PresenceActiveInMismatch)
        }
        _ => Ok(()),
    }
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateIntegratorPresenceRequest {
    pub status: PresenceStatus,
    /// Must be the calling integrator's own id, or omitted/`null` — see
    /// [`validate_integrator_playing`].
    #[serde(default)]
    pub active_in: Option<Uuid>,
}

/// `PUT /presence/:identity_id` — an integrator publishing presence on behalf of
/// a user it's bound to. Authenticated via `crate::authz`'s
/// `Caller`/`require_capability` (issue #28): the caller must be
/// `Caller::Integrator` (a user session hitting this route is rejected — that
/// endpoint is `PUT /me/presence` above), the path `identity_id` must
/// match the identity the integrator claims to act for
/// (`x-avalon-identity-id`, resolved by `authenticate_caller` — see
/// `authz`'s own doc comment for why this heads off an integrator naming one
/// identity in the path and another in the header), and the integrator must
/// hold an active `presence.publish` grant under an active binding to
/// that identity — `require_capability` alone is what rejects an unbound
/// identity or a revoked/missing grant, no separate hand-rolled check
/// here (see `authz`'s own module doc comment on why that's the one
/// authorization decision, not two).
#[utoipa::path(
    put,
    path = "/presence/{identity_id}",
    tag = "presence",
    params(("identity_id" = Uuid, Path)),
    request_body = UpdateIntegratorPresenceRequest,
    responses((status = 200, body = PresenceResponse)),
)]
pub async fn update_integrator_presence(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(identity_id): Path<Uuid>,
    Json(body): Json<UpdateIntegratorPresenceRequest>,
) -> Result<Json<PresenceResponse>, AppError> {
    let caller = authenticate_caller(&state, &headers).await?;
    let Caller::Integrator {
        integrator_id,
        identity_id: caller_identity_id,
    } = caller
    else {
        return Err(AppError::Forbidden);
    };
    if identity_id != caller_identity_id {
        return Err(AppError::Forbidden);
    }

    validate_integrator_playing(integrator_id, body.active_in)?;
    require_capability(&caller, Capability::PresencePublish, &state).await?;

    let updated_at = state.presence.set(identity_id, body.status, body.active_in);
    let response = PresenceResponse {
        identity_id,
        status: body.status,
        active_in: body.active_in,
        updated_at,
    };
    tokio::spawn(crate::realtime_relay::relay_to_peers(
        state.clone(),
        crate::realtime_relay::RelayEvent::Presence(response.clone()),
    ));
    Ok(Json(response))
}

#[derive(Deserialize, utoipa::IntoParams)]
pub struct PresenceQuery {
    /// Comma-separated identity ids, e.g. `?ids=<uuid>,<uuid>`.
    pub ids: String,
}

/// True if `caller` may see `subject`'s real presence: always for their
/// own entry; otherwise a block between them always hides it (checked
/// first, issue #97 — a block wins even under a `Public` setting); beyond
/// that, `subject`'s own stored `Visibility` decides (issue #87) —
/// `Public`/`AuthenticatedOnly` always visible here (`GET /presence`
/// already requires a valid session, so "authenticated" is trivially
/// true), `Friends` only if `friend_ids` contains `subject`,
/// `GuildMembers`/`Private` never (presence has no guild context, and
/// `Private` means nobody but the subject).
fn presence_visible(
    caller: Uuid,
    subject: Uuid,
    visibility: Visibility,
    friend_ids: &HashSet<Uuid>,
    blocked_partners: &HashSet<Uuid>,
) -> bool {
    if subject == caller {
        return true;
    }
    if blocked_partners.contains(&subject) {
        return false;
    }
    match visibility {
        Visibility::Public | Visibility::AuthenticatedOnly => true,
        Visibility::Friends => friend_ids.contains(&subject),
        Visibility::GuildMembers | Visibility::Private => false,
    }
}

fn hidden_playing_view(view: PresenceResponse, hidden: &HashSet<Uuid>) -> PresenceResponse {
    if hidden.contains(&view.identity_id) {
        PresenceResponse {
            active_in: None,
            ..view
        }
    } else {
        view
    }
}

/// `GET /presence?ids=…` — session-authenticated. Reads default to
/// friends-only visibility (see [`presence_visible`] and module doc
/// comment); a caller-hidden `active_in` preference
/// (`presence_preferences.hide_active_in`, see [`hide_active_in_for`]) is
/// applied independently on top, so an identity visible to the caller can
/// still have `active_in` come back `null` if they've opted out of it.
#[utoipa::path(
    get,
    path = "/presence",
    tag = "presence",
    params(PresenceQuery),
    responses((status = 200, body = Vec<PresenceResponse>)),
)]
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
    // "never a guess" precedent for a genuinely missing entry. A
    // non-friend reads identically, for the same reason.
    let friend_ids = crate::friends::friend_partners(&state, caller).await?;
    let blocked_partners = crate::blocks::block_partners(&state, caller).await?;
    let hidden_playing = hide_active_in_for(&state, &ids).await?;
    let visibility_by_id = presence_visibility_for(&state, &ids).await?;
    let views = ids
        .into_iter()
        .map(|id| {
            let visibility = visibility_by_id
                .get(&id)
                .copied()
                .unwrap_or(Visibility::Friends);
            if presence_visible(caller, id, visibility, &friend_ids, &blocked_partners) {
                hidden_playing_view(state.presence.get(id).into(), &hidden_playing)
            } else {
                PresenceResponse {
                    identity_id: id,
                    status: PresenceStatus::Offline,
                    active_in: None,
                    updated_at: OffsetDateTime::now_utc(),
                }
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
/// the connection's lifetime. Same friends-only default visibility as
/// `GET /presence` (see module doc comment and [`presence_visible`]).
///
/// Issue #663: when this node's own `AVALON_NODE_ROLES` excludes
/// `realtime` (`state.realtime_remote_url` is `Some`), the connection is
/// proxied through to the configured remote Realtime node instead of
/// being handled by `handle_presence_socket` locally — see
/// `crate::realtime_proxy`'s own module doc comment for the full design.
/// Authentication happens here either way, before either path upgrades
/// the socket.
pub async fn presence_ws(
    State(state): State<AppState>,
    Query(query): Query<PresenceWsQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, AppError> {
    let caller = authenticate_token(&state, &query.token).await?;
    if let Some(remote_url) = state.realtime_remote_url.clone() {
        let token = query.token.clone();
        return Ok(ws.on_upgrade(move |socket| {
            crate::realtime_proxy::proxy_websocket(socket, remote_url, "/ws/presence", token)
        }));
    }
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
/// identity (or, now, a non-friend under the friends-only default) must
/// read exactly like a missing/stale entry, never a distinguishable
/// "hidden" state.
fn offline_view(id: Uuid) -> PresenceResponse {
    PresenceResponse {
        identity_id: id,
        status: PresenceStatus::Offline,
        active_in: None,
        updated_at: OffsetDateTime::now_utc(),
    }
}

async fn handle_presence_socket(mut socket: WebSocket, state: AppState, caller: Uuid) {
    // Loaded once per connection, not per message — see
    // `blocks::block_partners`'s own doc comment for the staleness
    // tradeoff this accepts; `friend_partners` and each subscribed id's
    // `hide_active_in` preference accept the same tradeoff for the same
    // reason.
    let friend_ids = match crate::friends::friend_partners(&state, caller).await {
        Ok(set) => set,
        Err(_) => return,
    };
    let blocked_partners = match crate::blocks::block_partners(&state, caller).await {
        Ok(set) => set,
        Err(_) => return,
    };
    let mut subscribed: HashSet<Uuid> = HashSet::new();
    let mut hidden_playing: HashSet<Uuid> = HashSet::new();
    let mut visibility_by_id: HashMap<Uuid, Visibility> = HashMap::new();
    let mut updates = state.presence.subscribe();

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(ClientMessage::Subscribe { ids }) = serde_json::from_str(&text) else {
                            continue;
                        };
                        let new_ids: Vec<Uuid> = ids.into_iter().filter(|id| !subscribed.contains(id)).collect();
                        if new_ids.is_empty() {
                            continue;
                        }
                        if let Ok(hidden) = hide_active_in_for(&state, &new_ids).await {
                            hidden_playing.extend(hidden);
                        }
                        if let Ok(visibility) = presence_visibility_for(&state, &new_ids).await {
                            visibility_by_id.extend(visibility);
                        }
                        // Send a catch-up snapshot for each newly-subscribed
                        // id immediately, rather than making the client wait
                        // for that identity's next publish to learn its
                        // current status.
                        for id in new_ids {
                            subscribed.insert(id);
                            let visibility = visibility_by_id
                                .get(&id)
                                .copied()
                                .unwrap_or(Visibility::Friends);
                            let view: PresenceResponse = if presence_visible(caller, id, visibility, &friend_ids, &blocked_partners) {
                                hidden_playing_view(state.presence.get(id).into(), &hidden_playing)
                            } else {
                                offline_view(id)
                            };
                            let payload =
                                serde_json::to_string(&view).expect("PresenceResponse always serializes");
                            if socket.send(Message::Text(payload.into())).await.is_err() {
                                return;
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
                        let visibility = visibility_by_id
                            .get(&update.identity_id)
                            .copied()
                            .unwrap_or(Visibility::Friends);
                        let view = if presence_visible(caller, update.identity_id, visibility, &friend_ids, &blocked_partners) {
                            hidden_playing_view(update, &hidden_playing)
                        } else {
                            offline_view(update.identity_id)
                        };
                        let payload =
                            serde_json::to_string(&view).expect("PresenceResponse always serializes");
                        if socket.send(Message::Text(payload.into())).await.is_err() {
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
    fn sticky_away_survives_ttl_expiry() {
        let store = PresenceStore::with_ttl(Duration::from_millis(10));
        let id = Uuid::new_v4();
        store.set(id, PresenceStatus::Away, None);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(store.get(id).status, PresenceStatus::Away);
    }

    #[test]
    fn sticky_do_not_disturb_survives_ttl_expiry() {
        let store = PresenceStore::with_ttl(Duration::from_millis(10));
        let id = Uuid::new_v4();
        store.set(id, PresenceStatus::DoNotDisturb, None);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(store.get(id).status, PresenceStatus::DoNotDisturb);
    }

    #[test]
    fn sticky_offline_survives_ttl_expiry_and_is_distinguishable_from_a_missing_entry() {
        // Both a missing entry and an expired sticky `Offline` entry read as
        // `Offline` — the test is really about the entry not panicking/
        // erroring and the status staying `Offline`, not a new observable
        // difference (there isn't one on the wire), but this locks in that
        // an explicit sticky `Offline` takes the same code path as `Away`/
        // `DoNotDisturb` rather than accidentally hitting the TTL-expiry
        // branch (which would still yield `Offline` here, coincidentally —
        // see the `updated_at`-based test below for the real distinction).
        let store = PresenceStore::with_ttl(Duration::from_millis(10));
        let id = Uuid::new_v4();
        store.set(id, PresenceStatus::Offline, None);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(store.get(id).status, PresenceStatus::Offline);
    }

    #[test]
    fn sticky_override_preserves_its_own_updated_at_instead_of_now() {
        let store = PresenceStore::with_ttl(Duration::from_millis(10));
        let id = Uuid::new_v4();
        let set_at = store.set(id, PresenceStatus::Away, None);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(store.get(id).updated_at, set_at);
    }

    #[test]
    fn explicit_online_after_a_sticky_override_clears_it_and_resumes_ttl_tracking() {
        let store = PresenceStore::with_ttl(Duration::from_millis(10));
        let id = Uuid::new_v4();
        store.set(id, PresenceStatus::DoNotDisturb, None);
        store.set(id, PresenceStatus::Online, None);
        assert_eq!(store.get(id).status, PresenceStatus::Online);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(store.get(id).status, PresenceStatus::Offline);
    }

    #[test]
    fn missing_entry_reads_as_offline() {
        let store = PresenceStore::with_ttl(Duration::from_secs(60));
        let view = store.get(Uuid::new_v4());
        assert_eq!(view.status, PresenceStatus::Offline);
    }

    #[test]
    fn a_integrator_publishing_playing_for_another_integrators_id_is_rejected() {
        let own_integrator_id = Uuid::new_v4();
        let other_integrator_id = Uuid::new_v4();
        assert!(matches!(
            validate_integrator_playing(own_integrator_id, Some(other_integrator_id)),
            Err(AppError::PresenceActiveInMismatch)
        ));
    }

    #[test]
    fn a_integrator_publishing_playing_for_its_own_id_is_accepted() {
        let integrator_id = Uuid::new_v4();
        assert!(validate_integrator_playing(integrator_id, Some(integrator_id)).is_ok());
    }

    #[test]
    fn a_integrator_publishing_with_no_playing_claim_is_always_accepted() {
        assert!(validate_integrator_playing(Uuid::new_v4(), None).is_ok());
    }

    // The DB-backed "an integrator publishing for an unbound identity is
    // rejected" case is `require_capability`'s job, not
    // `validate_integrator_playing`'s — see `crate::authz`'s own exhaustive
    // pure-logic test matrix (`no_binding_at_all_is_rejected` et al.) plus
    // its `live_tests` submodule for the real-Postgres version, and
    // `crates/server/tests/presence.rs`'s
    // `a_integrator_cannot_publish_presence_for_an_unbound_identity` for the
    // full `PUT /presence/:identity_id` endpoint exercising it end to end.

    #[test]
    fn presence_visible_to_self_regardless_of_visibility_setting() {
        let caller = Uuid::new_v4();
        let empty = HashSet::new();
        assert!(presence_visible(
            caller,
            caller,
            Visibility::Private,
            &empty,
            &empty
        ));
    }

    #[test]
    fn presence_visible_to_a_friend_under_the_friends_default() {
        let caller = Uuid::new_v4();
        let friend = Uuid::new_v4();
        let friends: HashSet<Uuid> = [friend].into_iter().collect();
        assert!(presence_visible(
            caller,
            friend,
            Visibility::Friends,
            &friends,
            &HashSet::new()
        ));
    }

    #[test]
    fn presence_hidden_from_a_non_friend_under_the_friends_default() {
        let caller = Uuid::new_v4();
        let stranger = Uuid::new_v4();
        assert!(!presence_visible(
            caller,
            stranger,
            Visibility::Friends,
            &HashSet::new(),
            &HashSet::new()
        ));
    }

    #[test]
    fn presence_visible_to_a_stranger_under_public() {
        let caller = Uuid::new_v4();
        let stranger = Uuid::new_v4();
        assert!(presence_visible(
            caller,
            stranger,
            Visibility::Public,
            &HashSet::new(),
            &HashSet::new()
        ));
    }

    #[test]
    fn presence_hidden_from_everyone_but_self_under_private() {
        let caller = Uuid::new_v4();
        let friend = Uuid::new_v4();
        let friends: HashSet<Uuid> = [friend].into_iter().collect();
        assert!(!presence_visible(
            caller,
            friend,
            Visibility::Private,
            &friends,
            &HashSet::new()
        ));
    }

    #[test]
    fn presence_hidden_from_a_blocked_friend_even_under_public() {
        // A block hides presence regardless of the subject's own
        // visibility setting — the block check runs before the setting is
        // ever consulted.
        let caller = Uuid::new_v4();
        let friend_and_blocked = Uuid::new_v4();
        let friends: HashSet<Uuid> = [friend_and_blocked].into_iter().collect();
        let blocked: HashSet<Uuid> = [friend_and_blocked].into_iter().collect();
        assert!(!presence_visible(
            caller,
            friend_and_blocked,
            Visibility::Public,
            &friends,
            &blocked
        ));
    }

    #[test]
    fn hidden_playing_view_nulls_out_playing_only_for_hidden_ids() {
        let shown = Uuid::new_v4();
        let hidden_id = Uuid::new_v4();
        let hidden: HashSet<Uuid> = [hidden_id].into_iter().collect();

        let view = PresenceResponse {
            identity_id: shown,
            status: PresenceStatus::Online,
            active_in: Some(Uuid::new_v4()),
            updated_at: OffsetDateTime::now_utc(),
        };
        assert!(hidden_playing_view(view.clone(), &hidden)
            .active_in
            .is_some());

        let hidden_view = PresenceResponse {
            identity_id: hidden_id,
            ..view
        };
        assert_eq!(hidden_playing_view(hidden_view, &hidden).active_in, None);
    }

    /// The ticket's own explicit ask (issue #16's Tests section): a
    /// grep-level check that no handler in this file calls
    /// `state.chain` `.commit` — belt-and-suspenders on top of the fact
    /// that this module imports neither `avalon_chain` nor `crate::outbox`
    /// at all (ADR #78). Reads its own source via `include_str!` rather
    /// than walking the filesystem, so it runs the same in any working
    /// directory `cargo test` is invoked from.
    ///
    /// The needle is built from two joined halves, deliberately never
    /// written as one contiguous string literal anywhere in this file —
    /// otherwise `include_str!` would pull in this very assertion's own
    /// text and the check would trivially fail against itself.
    #[test]
    fn presence_handlers_never_call_chain_commit() {
        let source = include_str!("presence.rs");
        let needle = format!("{}{}", "chain.", "commit");
        assert!(
            !source.contains(&needle),
            "presence is ephemeral (ADR #78) and must never touch SettlementProvider::commit"
        );
    }
}
