//! Game bindings and capability grants — the player consent flow (issue
//! #27) that establishes a `GameBinding` (issue #83).
//!
//! `POST /games/{slug}/connect` is the one endpoint both tickets describe
//! from two angles: #83 says "a binding is established by the player
//! through the consent flow (#27)"; #27 says its own connect endpoint is
//! "also where a GameBinding (#83) is established." Building them as
//! separate endpoints would mean two code paths fighting over the same
//! row, so this module owns both.
//!
//! **Every mutating endpoint here requires the caller's own player
//! session** (`crate::handlers::authenticate`), never a game credential —
//! same reasoning `crates/server/src/guilds.rs`'s module doc comment lays
//! out for guild mutations: a grant is a player action, and no endpoint
//! lets a game grant itself anything. `GET /games/{slug}` (in `games.rs`)
//! is the one public, unauthenticated read this module depends on, to
//! validate an approved capability against what the game actually declared
//! at registration (`game_requested_capabilities`).
//!
//! `bindings` and `permission_grants` are projections, same posture as
//! every other table in this repo: `game.binding_established`,
//! `game.binding_ended`, `permission.granted`, and `permission.revoked` are
//! the durable history, written into the outbox in the same transaction as
//! the row change they accompany. All four are network-attributed for now,
//! not player-signed, for the same reason `games.rs`'s `game.registered`
//! is network-attributed rather than game-signed: no general per-event
//! Ed25519 signing ceremony exists yet beyond `identity.created`
//! (`crates/server/src/handlers.rs`'s "network as signer" milestone-1
//! stand-in). `issuer`/`subject` for the binding events are the acting
//! identity and the game, mirroring the event-kind catalogue
//! (`docs/architecture/protocol-events.md`); for the grant events, the
//! acting identity and the game+capability pair.
//!
//! **Invariants enforced here:**
//! - No grant exists without a binding — every `permission_grants` row
//!   references a `bindings` row, and a grant is only ever inserted inside
//!   the same transaction that guarantees an active binding exists.
//! - Ending a binding ends every grant under it, in the same transaction
//!   (`end_connection`).
//! - A player can only grant a capability the game declared at registration
//!   (`game_requested_capabilities`) — anything else is a 400
//!   (`AppError::CapabilityNotRequested`).
//! - Reconnecting to an already-bound game does not duplicate the binding
//!   or emit a second `game.binding_established` — `connect` only creates a
//!   binding row (and only emits the event) when no active binding already
//!   exists.

use std::collections::HashSet;

use avalon_protocol::events::ProtocolEvent;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::AppError;
use crate::games::{fetch_game_id_by_slug, game_ref};
use crate::handlers::authenticate;
use crate::outbox;
use crate::state::AppState;

fn identity_ref(identity_id: Uuid, verb: &str) -> avalon_protocol::ids::GlobalId {
    avalon_protocol::ids::GlobalId::new("identity", &identity_id.to_string(), "self", verb)
}

/// The set of capabilities `game_id` declared at registration — what
/// `connect` and the undeclared-capability check validate approvals
/// against.
async fn requested_capabilities(
    tx: &mut Transaction<'_, Postgres>,
    game_id: Uuid,
) -> Result<HashSet<String>, AppError> {
    let rows = sqlx::query("SELECT capability FROM game_requested_capabilities WHERE game_id = $1")
        .bind(game_id)
        .fetch_all(&mut **tx)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| r.try_get::<String, _>("capability"))
        .collect::<Result<HashSet<_>, _>>()?)
}

/// The active binding row (if any) for `(identity_id, game_id)`, locked
/// `FOR UPDATE` so a concurrent connect/disconnect can't race past this
/// check within the same transaction.
async fn active_binding(
    tx: &mut Transaction<'_, Postgres>,
    identity_id: Uuid,
    game_id: Uuid,
) -> Result<Option<Uuid>, AppError> {
    let row = sqlx::query(
        "SELECT id FROM bindings WHERE identity_id = $1 AND game_id = $2 AND ended_at IS NULL \
         FOR UPDATE",
    )
    .bind(identity_id)
    .bind(game_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|r| r.try_get("id")).transpose()?)
}

#[derive(Deserialize)]
pub struct ConnectRequest {
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Serialize)]
pub struct ConnectResponse {
    pub binding_id: Uuid,
    pub game_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub established_at: OffsetDateTime,
    pub granted_capabilities: Vec<String>,
}

/// `POST /games/{slug}/connect` — the consent flow (#27) and the endpoint
/// that establishes a `GameBinding` (#83). Idempotent: reconnecting to a
/// game the caller already has an active binding to does not create a
/// second binding or emit a second `game.binding_established`, but it does
/// still grant any newly-approved capabilities.
pub async fn connect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(body): Json<ConnectRequest>,
) -> Result<Json<ConnectResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;
    let game_id = fetch_game_id_by_slug(&state, &slug).await?;

    let mut tx = state.pool.begin().await?;

    let declared = requested_capabilities(&mut tx, game_id).await?;
    for capability in &body.capabilities {
        if !declared.contains(capability) {
            return Err(AppError::CapabilityNotRequested);
        }
    }

    // Truncated to microseconds up front: Postgres timestamptz only stores
    // that much precision, so the freshly-inserted response below would
    // otherwise claim more precision than what a later read of this same
    // row (the `existing_binding` branch) actually returns.
    let now = OffsetDateTime::now_utc();
    let now = now
        .replace_nanosecond((now.nanosecond() / 1_000) * 1_000)
        .expect("truncating toward zero always stays in the valid nanosecond range");
    let existing_binding = active_binding(&mut tx, identity_id, game_id).await?;

    let (binding_id, established_at, newly_created) = if let Some(binding_id) = existing_binding {
        let row = sqlx::query("SELECT established_at FROM bindings WHERE id = $1")
            .bind(binding_id)
            .fetch_one(&mut *tx)
            .await?;
        (binding_id, row.try_get("established_at")?, false)
    } else {
        let binding_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO bindings (id, identity_id, game_id, established_at) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(binding_id)
        .bind(identity_id)
        .bind(game_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        (binding_id, now, true)
    };

    if newly_created {
        let event = ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "game.binding_established".to_string(),
            issuer: identity_ref(identity_id, "binding_established"),
            subject: game_ref(&slug, "binding_established"),
            payload: serde_json::json!({
                "binding_id": binding_id,
                "identity_id": identity_id,
                "game_id": game_id,
                "slug": slug,
            }),
            timestamp: now,
            version: 1,
        };
        outbox::enqueue(&mut tx, &event).await?;
    }

    let mut granted_capabilities = Vec::with_capacity(body.capabilities.len());
    for capability in &body.capabilities {
        // Re-granting an already-active capability is a no-op at the row
        // level (the partial unique index on `(binding_id, capability)
        // WHERE revoked_at IS NULL` would reject a duplicate insert), and
        // re-granting a previously revoked one clears `revoked_at` rather
        // than inserting a second history row for the same capability —
        // "grant" always ends in exactly one active row per
        // (binding, capability).
        let already_active = sqlx::query(
            "SELECT 1 FROM permission_grants WHERE binding_id = $1 AND capability = $2 \
             AND revoked_at IS NULL",
        )
        .bind(binding_id)
        .bind(capability)
        .fetch_optional(&mut *tx)
        .await?
        .is_some();
        if already_active {
            granted_capabilities.push(capability.clone());
            continue;
        }

        let updated = sqlx::query(
            "UPDATE permission_grants SET granted_at = $1, revoked_at = NULL \
             WHERE binding_id = $2 AND capability = $3",
        )
        .bind(now)
        .bind(binding_id)
        .bind(capability)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            sqlx::query(
                "INSERT INTO permission_grants (id, binding_id, capability, granted_at) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(Uuid::new_v4())
            .bind(binding_id)
            .bind(capability)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }

        let event = ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "permission.granted".to_string(),
            issuer: identity_ref(identity_id, "granted"),
            subject: game_ref(&slug, capability),
            payload: serde_json::json!({
                "binding_id": binding_id,
                "identity_id": identity_id,
                "game_id": game_id,
                "capability": capability,
            }),
            timestamp: now,
            version: 1,
        };
        outbox::enqueue(&mut tx, &event).await?;
        granted_capabilities.push(capability.clone());
    }

    tx.commit().await?;

    Ok(Json(ConnectResponse {
        binding_id,
        game_id,
        established_at,
        granted_capabilities,
    }))
}

/// `DELETE /games/{slug}/grants/{capability}` — revokes one capability
/// without ending the binding.
pub async fn revoke_grant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((slug, capability)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;
    let game_id = fetch_game_id_by_slug(&state, &slug).await?;

    let mut tx = state.pool.begin().await?;
    let binding_id = active_binding(&mut tx, identity_id, game_id)
        .await?
        .ok_or(AppError::BindingNotFound)?;

    let now = OffsetDateTime::now_utc();
    let updated = sqlx::query(
        "UPDATE permission_grants SET revoked_at = $1 \
         WHERE binding_id = $2 AND capability = $3 AND revoked_at IS NULL",
    )
    .bind(now)
    .bind(binding_id)
    .bind(&capability)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(AppError::GrantNotFound);
    }

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "permission.revoked".to_string(),
        issuer: identity_ref(identity_id, "revoked"),
        subject: game_ref(&slug, &capability),
        payload: serde_json::json!({
            "binding_id": binding_id,
            "identity_id": identity_id,
            "game_id": game_id,
            "capability": capability,
        }),
        timestamp: now,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(serde_json::json!({ "revoked": true })))
}

/// `DELETE /games/{slug}/connect` — ends the binding and revokes every
/// active grant under it, in the same transaction (#83's invariant).
pub async fn disconnect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;
    let game_id = fetch_game_id_by_slug(&state, &slug).await?;

    let mut tx = state.pool.begin().await?;
    let binding_id = active_binding(&mut tx, identity_id, game_id)
        .await?
        .ok_or(AppError::BindingNotFound)?;

    let now = OffsetDateTime::now_utc();

    let revoked_capabilities: Vec<String> = sqlx::query(
        "UPDATE permission_grants SET revoked_at = $1 \
         WHERE binding_id = $2 AND revoked_at IS NULL \
         RETURNING capability",
    )
    .bind(now)
    .bind(binding_id)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|r| r.try_get::<String, _>("capability"))
    .collect::<Result<_, _>>()?;

    sqlx::query("UPDATE bindings SET ended_at = $1 WHERE id = $2")
        .bind(now)
        .bind(binding_id)
        .execute(&mut *tx)
        .await?;

    for capability in &revoked_capabilities {
        let event = ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "permission.revoked".to_string(),
            issuer: identity_ref(identity_id, "revoked"),
            subject: game_ref(&slug, capability),
            payload: serde_json::json!({
                "binding_id": binding_id,
                "identity_id": identity_id,
                "game_id": game_id,
                "capability": capability,
                "reason": "binding_ended",
            }),
            timestamp: now,
            version: 1,
        };
        outbox::enqueue(&mut tx, &event).await?;
    }

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "game.binding_ended".to_string(),
        issuer: identity_ref(identity_id, "binding_ended"),
        subject: game_ref(&slug, "binding_ended"),
        payload: serde_json::json!({
            "binding_id": binding_id,
            "identity_id": identity_id,
            "game_id": game_id,
            "slug": slug,
        }),
        timestamp: now,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(serde_json::json!({ "ended": true })))
}

#[derive(Serialize)]
pub struct ConnectionGrant {
    pub capability: String,
    #[serde(with = "time::serde::rfc3339")]
    pub granted_at: OffsetDateTime,
}

#[derive(Serialize)]
pub struct Connection {
    pub binding_id: Uuid,
    pub game_id: Uuid,
    pub slug: String,
    pub name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub established_at: OffsetDateTime,
    pub grants: Vec<ConnectionGrant>,
}

/// `GET /me/connections` — every binding the caller has, each with its
/// currently-active grants. Ended bindings are not included; a future
/// history view can add them separately without changing this endpoint's
/// meaning.
pub async fn list_my_connections(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Connection>>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let binding_rows = sqlx::query(
        "SELECT b.id AS binding_id, b.game_id, b.established_at, g.slug, g.name \
         FROM bindings b JOIN games g ON g.id = b.game_id \
         WHERE b.identity_id = $1 AND b.ended_at IS NULL \
         ORDER BY b.established_at",
    )
    .bind(identity_id)
    .fetch_all(&state.pool)
    .await?;

    let mut connections = Vec::with_capacity(binding_rows.len());
    for row in binding_rows {
        let binding_id: Uuid = row.try_get("binding_id")?;
        let grant_rows = sqlx::query(
            "SELECT capability, granted_at FROM permission_grants \
             WHERE binding_id = $1 AND revoked_at IS NULL ORDER BY granted_at",
        )
        .bind(binding_id)
        .fetch_all(&state.pool)
        .await?;
        let grants = grant_rows
            .into_iter()
            .map(|r| {
                Ok(ConnectionGrant {
                    capability: r.try_get("capability")?,
                    granted_at: r.try_get("granted_at")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()?;

        connections.push(Connection {
            binding_id,
            game_id: row.try_get("game_id")?,
            slug: row.try_get("slug")?,
            name: row.try_get("name")?,
            established_at: row.try_get("established_at")?,
            grants,
        });
    }

    Ok(Json(connections))
}

const GAME_KEY_ID_HEADER: &str = "x-avalon-game-key-id";

#[derive(Serialize)]
pub struct MyGrantsResponse {
    pub game_id: Uuid,
    pub capabilities: Vec<String>,
}

/// `GET /me/grants` — the calling game's own active grants for the
/// authenticating player, read by `crates/sdk/src/lib.rs`'s
/// `AvalonClient::authenticate()` to populate `Session.granted`.
///
/// Authenticated by the caller's own player session
/// (`Authorization: Bearer <player token>`), same as every other endpoint
/// in this module — **not** the game challenge-response scheme
/// (`games::authenticate_game`). The `x-avalon-game-key-id` header only
/// says *which* game's grants to read; it is not itself a security
/// boundary here, since the answer ("what has this player granted this
/// game") is the player's own information to ask about their own
/// connections, not something that needs a game to prove key possession —
/// the SDK already knows its own `game_credential_key_id`
/// (`AvalonConfig`) and just needs a way to tell the server which game it
/// is asking on behalf of.
pub async fn my_grants(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<MyGrantsResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;
    let key_id: Uuid = headers
        .get(GAME_KEY_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or(AppError::GameKeyNotFound)?;

    let game_id: Uuid = sqlx::query("SELECT game_id FROM issuer_keys WHERE key_id = $1")
        .bind(key_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::GameKeyNotFound)?
        .try_get("game_id")?;

    let rows = sqlx::query(
        "SELECT pg.capability FROM permission_grants pg \
         JOIN bindings b ON b.id = pg.binding_id \
         WHERE b.identity_id = $1 AND b.game_id = $2 \
         AND b.ended_at IS NULL AND pg.revoked_at IS NULL",
    )
    .bind(identity_id)
    .bind(game_id)
    .fetch_all(&state.pool)
    .await?;
    let capabilities = rows
        .into_iter()
        .map(|r| r.try_get::<String, _>("capability"))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Json(MyGrantsResponse {
        game_id,
        capabilities,
    }))
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — pure-logic checks only. The
    //! endpoint-level flows (connect, revoke, disconnect, reconnect
    //! idempotency) are covered by `crates/server/tests/connections.rs`,
    //! gated `--ignored`.

    use std::collections::HashSet;

    /// Mirrors the rejection `connect` performs: any approved capability
    /// not present in what the game declared is rejected outright, not
    /// silently dropped.
    #[test]
    fn granting_an_undeclared_capability_is_rejected() {
        let declared: HashSet<String> = ["friends.read".to_string()].into_iter().collect();
        let approved = ["friends.read".to_string(), "wallet.write".to_string()];

        let rejected = approved.iter().any(|c| !declared.contains(c));
        assert!(rejected, "wallet.write was never declared by the game");
    }

    #[test]
    fn granting_only_declared_capabilities_is_accepted() {
        let declared: HashSet<String> = ["friends.read".to_string(), "presence.read".to_string()]
            .into_iter()
            .collect();
        let approved = ["friends.read".to_string()];

        let rejected = approved.iter().any(|c| !declared.contains(c));
        assert!(!rejected);
    }

    /// Mirrors `connect`'s `newly_created` branch: a second connect against
    /// an already-active binding must not re-create the binding or fire a
    /// second `game.binding_established` — `existing_binding.is_some()`
    /// short-circuits both.
    #[test]
    fn reconnecting_to_an_active_binding_does_not_re_establish_it() {
        let existing_binding: Option<uuid::Uuid> = Some(uuid::Uuid::new_v4());
        let newly_created = existing_binding.is_none();
        assert!(
            !newly_created,
            "an existing active binding must be reused, not recreated"
        );
    }

    /// Mirrors `disconnect`'s revoke-then-end sequence: every grant with
    /// `revoked_at IS NULL` under the binding is captured for a
    /// `permission.revoked` event before the binding itself is ended, in
    /// the same transaction — none are left active.
    #[test]
    fn ending_a_binding_revokes_every_active_grant_under_it() {
        struct Grant {
            capability: &'static str,
            revoked: bool,
        }
        let mut grants = [
            Grant {
                capability: "friends.read",
                revoked: false,
            },
            Grant {
                capability: "presence.read",
                revoked: false,
            },
            Grant {
                capability: "wallet.read",
                revoked: true,
            },
        ];

        let newly_revoked: Vec<&'static str> = grants
            .iter_mut()
            .filter(|g| !g.revoked)
            .map(|g| {
                g.revoked = true;
                g.capability
            })
            .collect();

        assert_eq!(newly_revoked, vec!["friends.read", "presence.read"]);
        assert!(grants.iter().all(|g| g.revoked));
    }
}
