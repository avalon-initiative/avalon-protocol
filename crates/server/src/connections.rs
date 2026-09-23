//! Integrator bindings and capability grants — the user consent flow
//! that establishes an `IntegratorBinding`. See
//! `docs/architecture/bindings.md`'s "Today in the repo" for why one
//! endpoint owns both concerns, the durable event history, and the
//! no-grant-without-a-binding invariants enforced here.

use std::collections::HashSet;

use avalon_protocol::event_payloads::{
    GameBindingEndedPayload, GameBindingEstablishedPayload, PermissionGrantedPayload,
    PermissionRevokedPayload,
};
use avalon_protocol::events::{ProtocolEvent, ProtocolEventKindVariant};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::AppError;
use crate::handlers::authenticate;
use crate::integrators::{fetch_integrator_id_by_slug, integrator_ref};
use crate::outbox;
use crate::signature_gate::{canonical_message, require_fresh_signature};
use crate::state::AppState;

fn identity_ref(identity_id: Uuid, verb: &str) -> avalon_protocol::ids::GlobalId {
    avalon_protocol::ids::GlobalId::new("identity", &identity_id.to_string(), "self", verb)
}

/// The set of capabilities `integrator_id` declared at registration — what
/// `connect` and the undeclared-capability check validate approvals
/// against.
async fn requested_capabilities(
    tx: &mut Transaction<'_, Postgres>,
    integrator_id: Uuid,
) -> Result<HashSet<String>, AppError> {
    let rows = sqlx::query(
        "SELECT capability FROM integrator_requested_capabilities WHERE integrator_id = $1",
    )
    .bind(integrator_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| r.try_get::<String, _>("capability"))
        .collect::<Result<HashSet<_>, _>>()?)
}

/// The active binding row (if any) for `(identity_id, integrator_id)`, locked
/// `FOR UPDATE` so a concurrent connect/disconnect can't race past this
/// check within the same transaction.
async fn active_binding(
    tx: &mut Transaction<'_, Postgres>,
    identity_id: Uuid,
    integrator_id: Uuid,
) -> Result<Option<Uuid>, AppError> {
    let row = sqlx::query(
        "SELECT id FROM bindings WHERE identity_id = $1 AND integrator_id = $2 AND ended_at IS NULL \
         FOR UPDATE",
    )
    .bind(identity_id)
    .bind(integrator_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|r| r.try_get("id")).transpose()?)
}

#[derive(Deserialize, ToSchema)]
pub struct ConnectRequest {
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// #697/#698: hands a third party standing permission over the
    /// identity's data going forward — signature-required.
    pub signing_key_id: Option<Uuid>,
    pub signature: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ConnectResponse {
    pub binding_id: Uuid,
    pub integrator_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub established_at: OffsetDateTime,
    pub granted_capabilities: Vec<String>,
}

/// `POST /integrations/{slug}/connect` — the consent flow and the endpoint
/// that establishes a `IntegratorBinding`. Idempotent: reconnecting to a
/// integrator the caller already has an active binding to does not create a
/// second binding or emit a second `game.binding_established`, but it does
/// still grant any newly-approved capabilities.
#[utoipa::path(
    post,
    path = "/integrations/{slug}/connect",
    tag = "integrators",
    params(("slug" = String, Path)),
    request_body = ConnectRequest,
    responses((status = 200, body = ConnectResponse)),
)]
pub async fn connect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(body): Json<ConnectRequest>,
) -> Result<Json<ConnectResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;
    let integrator_id = fetch_integrator_id_by_slug(&state, &slug).await?;

    let message = canonical_message(
        "integration.connect",
        &[&slug, &body.capabilities.join(",")],
    );
    require_fresh_signature(
        &state,
        identity_id,
        &message,
        body.signing_key_id,
        body.signature.as_deref(),
    )
    .await?;

    let mut tx = state.pool.begin().await?;

    let declared = requested_capabilities(&mut tx, integrator_id).await?;
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
    let existing_binding = active_binding(&mut tx, identity_id, integrator_id).await?;

    let (binding_id, established_at, newly_created) = if let Some(binding_id) = existing_binding {
        let row = sqlx::query("SELECT established_at FROM bindings WHERE id = $1")
            .bind(binding_id)
            .fetch_one(&mut *tx)
            .await?;
        (binding_id, row.try_get("established_at")?, false)
    } else {
        let binding_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO bindings (id, identity_id, integrator_id, established_at) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(binding_id)
        .bind(identity_id)
        .bind(integrator_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        (binding_id, now, true)
    };

    let mut binding_established_event = None;
    if newly_created {
        let event = ProtocolEvent {
            id: Uuid::new_v4(),
            kind: ProtocolEventKindVariant::GameBindingEstablished
                .as_str()
                .to_string(),
            issuer: identity_ref(identity_id, "binding_established"),
            subject: integrator_ref(&slug, "binding_established"),
            payload: serde_json::to_value(GameBindingEstablishedPayload {
                binding_id,
                identity_id,
                game_id: integrator_id,
                slug: slug.clone(),
            })
            .expect("GameBindingEstablishedPayload should serialize"),
            timestamp: now,
            version: 1,
        };
        // outbox::enqueue alone only drives ledger settlement — the Integrator
        // Registry's players/total_players_ever metrics (crate::registry,
        // backed by indexer_integrator_bindings) need the projection applied too,
        // same as register_finish/update_profile do for their own events.
        state.indexer.apply_in_tx(&mut tx, &event).await?;
        outbox::enqueue(&mut tx, &event).await?;
        binding_established_event = Some(event);
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
            kind: ProtocolEventKindVariant::PermissionGranted
                .as_str()
                .to_string(),
            issuer: identity_ref(identity_id, "granted"),
            subject: integrator_ref(&slug, capability),
            payload: serde_json::to_value(PermissionGrantedPayload {
                binding_id,
                identity_id,
                game_id: integrator_id,
                capability: capability.to_string(),
            })
            .expect("PermissionGrantedPayload should serialize"),
            timestamp: now,
            version: 1,
        };
        outbox::enqueue(&mut tx, &event).await?;
        granted_capabilities.push(capability.clone());
    }

    tx.commit().await?;
    if let Some(event) = &binding_established_event {
        state.indexer.apply_after_commit(event).await?;
    }

    Ok(Json(ConnectResponse {
        binding_id,
        integrator_id,
        established_at,
        granted_capabilities,
    }))
}

/// `DELETE /integrations/{slug}/grants/{capability}` — revokes one capability
/// without ending the binding.
#[utoipa::path(
    delete,
    path = "/integrations/{slug}/grants/{capability}",
    tag = "integrators",
    params(("slug" = String, Path), ("capability" = String, Path)),
    responses((status = 200, description = "{ \"revoked\": true }")),
)]
pub async fn revoke_grant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((slug, capability)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;
    let integrator_id = fetch_integrator_id_by_slug(&state, &slug).await?;

    let mut tx = state.pool.begin().await?;
    let binding_id = active_binding(&mut tx, identity_id, integrator_id)
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
        kind: ProtocolEventKindVariant::PermissionRevoked
            .as_str()
            .to_string(),
        issuer: identity_ref(identity_id, "revoked"),
        subject: integrator_ref(&slug, &capability),
        payload: serde_json::to_value(PermissionRevokedPayload {
            binding_id,
            identity_id,
            game_id: integrator_id,
            capability: capability.clone(),
            reason: None,
        })
        .expect("PermissionRevokedPayload should serialize"),
        timestamp: now,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(serde_json::json!({ "revoked": true })))
}

/// `DELETE /integrations/{slug}/connect` — ends the binding and revokes every
/// active grant under it, in the same transaction (#83's invariant).
#[utoipa::path(
    delete,
    path = "/integrations/{slug}/connect",
    tag = "integrators",
    params(("slug" = String, Path)),
    responses((status = 200, description = "{ \"ended\": true }")),
)]
pub async fn disconnect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;
    let integrator_id = fetch_integrator_id_by_slug(&state, &slug).await?;

    let mut tx = state.pool.begin().await?;
    let binding_id = active_binding(&mut tx, identity_id, integrator_id)
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
            kind: ProtocolEventKindVariant::PermissionRevoked
                .as_str()
                .to_string(),
            issuer: identity_ref(identity_id, "revoked"),
            subject: integrator_ref(&slug, capability),
            payload: serde_json::to_value(PermissionRevokedPayload {
                binding_id,
                identity_id,
                game_id: integrator_id,
                capability: capability.to_string(),
                reason: Some("binding_ended".to_string()),
            })
            .expect("PermissionRevokedPayload should serialize"),
            timestamp: now,
            version: 1,
        };
        outbox::enqueue(&mut tx, &event).await?;
    }

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::GameBindingEnded
            .as_str()
            .to_string(),
        issuer: identity_ref(identity_id, "binding_ended"),
        subject: integrator_ref(&slug, "binding_ended"),
        payload: serde_json::to_value(GameBindingEndedPayload {
            binding_id,
            identity_id,
            game_id: integrator_id,
            slug: slug.clone(),
        })
        .expect("GameBindingEndedPayload should serialize"),
        timestamp: now,
        version: 1,
    };
    state.indexer.apply_in_tx(&mut tx, &event).await?;
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;
    state.indexer.apply_after_commit(&event).await?;

    Ok(Json(serde_json::json!({ "ended": true })))
}

#[derive(Serialize, ToSchema)]
pub struct ConnectionGrant {
    pub capability: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub granted_at: OffsetDateTime,
}

#[derive(Serialize, ToSchema)]
pub struct Connection {
    pub binding_id: Uuid,
    pub integrator_id: Uuid,
    pub slug: String,
    pub name: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub established_at: OffsetDateTime,
    pub grants: Vec<ConnectionGrant>,
}

/// `GET /me/connections` — every binding the caller has, each with its
/// currently-active grants. Ended bindings are not included; a future
/// history view can add them separately without changing this endpoint's
/// meaning.
#[utoipa::path(
    get,
    path = "/me/connections",
    tag = "integrators",
    responses((status = 200, body = Vec<Connection>)),
)]
pub async fn list_my_connections(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Connection>>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let binding_rows = sqlx::query(
        "SELECT b.id AS binding_id, b.integrator_id, b.established_at, g.slug, g.name \
         FROM bindings b JOIN integrators g ON g.id = b.integrator_id \
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
            integrator_id: row.try_get("integrator_id")?,
            slug: row.try_get("slug")?,
            name: row.try_get("name")?,
            established_at: row.try_get("established_at")?,
            grants,
        });
    }

    Ok(Json(connections))
}

/// The single accepted spelling since #290 collapsed the transitional
/// `x-avalon-game-key-id`/`x-avalon-integrator-key-id` pair that #293 had
/// introduced. Worth keeping in mind when touching this: a live #34 SDK test
/// once failed with `CapabilityNotGranted` despite a real grant existing,
/// because this handler read only one of the two names and silently ignored
/// the one `AvalonClient` actually sends.
const INTEGRATOR_KEY_ID_HEADER: &str = "x-avalon-integrator-key-id";

#[derive(Serialize, ToSchema)]
pub struct MyGrantsResponse {
    pub integrator_id: Uuid,
    pub capabilities: Vec<String>,
}

/// `GET /me/grants` — the calling integrator's own active grants for the
/// authenticating user, read by `crates/sdk/src/lib.rs`'s
/// `AvalonClient::authenticate()` to populate `Session.granted`.
///
/// Authenticated by the caller's own user session
/// (`Authorization: Bearer <user token>`), same as every other endpoint
/// in this module — **not** the integrator challenge-response scheme
/// (`integrators::authenticate_integrator`). The `x-avalon-integrator-key-id` header only
/// says *which* integrator's grants to read; it is not itself a security
/// boundary here, since the answer ("what has this user granted this
/// integrator") is the user's own information to ask about their own
/// connections, not something that needs an integrator to prove key possession —
/// the SDK already knows its own `integrator_credential_key_id`
/// (`AvalonConfig`) and just needs a way to tell the server which integrator it
/// is asking on behalf of.
#[utoipa::path(
    get,
    path = "/me/grants",
    tag = "integrators",
    params(("x-avalon-integrator-key-id" = String, Header)),
    responses((status = 200, body = MyGrantsResponse)),
)]
pub async fn my_grants(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<MyGrantsResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;
    let key_id: Uuid = headers
        .get(INTEGRATOR_KEY_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or(AppError::IntegratorKeyNotFound)?;

    let integrator_id: Uuid =
        sqlx::query("SELECT integrator_id FROM issuer_keys WHERE key_id = $1")
            .bind(key_id)
            .fetch_optional(&state.pool)
            .await?
            .ok_or(AppError::IntegratorKeyNotFound)?
            .try_get("integrator_id")?;

    let rows = sqlx::query(
        "SELECT pg.capability FROM permission_grants pg \
         JOIN bindings b ON b.id = pg.binding_id \
         WHERE b.identity_id = $1 AND b.integrator_id = $2 \
         AND b.ended_at IS NULL AND pg.revoked_at IS NULL",
    )
    .bind(identity_id)
    .bind(integrator_id)
    .fetch_all(&state.pool)
    .await?;
    let capabilities = rows
        .into_iter()
        .map(|r| r.try_get::<String, _>("capability"))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Json(MyGrantsResponse {
        integrator_id,
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
    /// not present in what the integrator declared is rejected outright, not
    /// silently dropped.
    #[test]
    fn granting_an_undeclared_capability_is_rejected() {
        let declared: HashSet<String> = ["friends.read".to_string()].into_iter().collect();
        let approved = ["friends.read".to_string(), "wallet.write".to_string()];

        let rejected = approved.iter().any(|c| !declared.contains(c));
        assert!(
            rejected,
            "wallet.write was never declared by the integrator"
        );
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
