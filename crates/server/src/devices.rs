//! Device-registration / linked-device grant model for signing-key custody —
//! the primary path for moving an identity
//! to a new device, complementing a mnemonic-phrase fallback.
//!
//! No new "device" concept lives anywhere but `identity_signing_keys`
//! itself: a device *is* one row there (it already supports multiple rows
//! per identity, already carries a `label`). A grant only ever
//! *authorizes a new public key* into that table — it never transfers a
//! private key across the network. The requesting device generates its own
//! fresh Ed25519 keypair client-side and keeps the secret in memory until
//! its grant is approved; the server only ever sees the resulting public
//! key, exactly the same "server never sees private key material"
//! invariant #134 already established, now per-device instead of
//! per-identity.
//!
//! Grant round-trip: a device with a session but no local signing key
//! `POST`s a grant request (its freshly generated public key); any other
//! currently-trusted device (one whose own `identity_signing_keys` row
//! isn't revoked) polls for pending grants and approves one by signing
//! `device_grant_approval_signing_bytes_v2(...)` with its own key — proving
//! the approval itself came from a device that once passed a real WebAuthn
//! ceremony, not a bare unauthenticated request. Revocation is
//! unilateral: any authenticated session for the identity can revoke any
//! signing-key row (including its own) without the revoked device's
//! cooperation.
//!
//! `identity.signing_key_added` is signed by the *approving* device's key
//! (the same detached-signature verification `handlers::register_finish`
//! already established for `identity.created`); `identity.signing_key_revoked`
//! carries the signature of an active key of the same identity.

use avalon_protocol::ids::IdentityId;
use avalon_protocol::event_payloads::{
    IdentitySigningKeyAddedPayload, IdentitySigningKeyRevokedPayload, SIGNING_KEY_KIND_DEVICE_GRANT,
};
use avalon_protocol::identity_id::{
    device_grant_approval_signing_bytes_v2, signing_key_revoked_signing_bytes_v2,
};
use avalon_protocol::events::{ProtocolEvent, ProtocolEventKindVariant};
use avalon_protocol::ids::GlobalId;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::verify_event_signature;
use crate::error::AppError;
use crate::handlers::authenticate;
use crate::outbox;
use crate::state::AppState;
use utoipa::ToSchema;

const GRANT_TTL_MINUTES: i64 = 15;

fn identity_ref(identity_id: IdentityId, verb: &str) -> GlobalId {
    GlobalId::new("identity", &identity_id.to_string(), "self", verb)
}

#[derive(Deserialize, ToSchema)]
pub struct RequestDeviceGrantRequest {
    /// Base64-encoded raw Ed25519 public key — freshly generated
    /// client-side for this device, never persisted locally until this
    /// grant is approved.
    pub requested_signing_public_key: String,
    pub device_label: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct DeviceGrantResponse {
    pub id: Uuid,
    pub status: String,
    pub device_label: Option<String>,
    /// Base64-encoded — the approving device needs this exact value to
    /// reconstruct `device_grant_approval_signing_bytes_v2` and sign it; the
    /// server never trusts a client-supplied copy of its own request back,
    /// but the *approver* is a different device that only ever learns this
    /// key by reading it back off this response.
    pub requested_signing_public_key: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub requested_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub expires_at: OffsetDateTime,
}

/// `POST /me/devices/grants` — a device with a session but no local signing
/// key asks to be granted one. Requires only the caller's existing session
/// (already proven by a real WebAuthn ceremony, per this repo's usual
/// "every route just accepts a session bearer token" pattern) — approval,
/// not this request, is where the stronger signature check lives.
#[utoipa::path(
    post,
    path = "/me/devices/grants",
    tag = "devices",
    request_body = RequestDeviceGrantRequest,
    responses((status = 200, description = "The resulting device grant", body = DeviceGrantResponse)),
)]
pub async fn request_device_grant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RequestDeviceGrantRequest>,
) -> Result<Json<DeviceGrantResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;
    let requested_signing_public_key = BASE64
        .decode(&body.requested_signing_public_key)
        .ok()
        .filter(|raw| {
            <[u8; 32]>::try_from(raw.as_slice())
                .ok()
                .and_then(|key| avalon_protocol::ed25519_key::parse_ed25519_public_key(&key))
                .is_some()
        })
        .ok_or(AppError::InvalidGrantSignature)?;

    let grant_id = Uuid::new_v4();
    let requested_at = OffsetDateTime::now_utc();
    let expires_at = requested_at + time::Duration::minutes(GRANT_TTL_MINUTES);

    sqlx::query(
        r#"
        INSERT INTO device_grants
            (id, identity_id, requested_signing_public_key, device_label, requested_at, expires_at)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(grant_id)
    .bind(identity_id)
    .bind(&requested_signing_public_key)
    .bind(&body.device_label)
    .bind(requested_at)
    .bind(expires_at)
    .execute(&state.pool)
    .await?;

    Ok(Json(DeviceGrantResponse {
        id: grant_id,
        status: "pending".to_string(),
        device_label: body.device_label,
        requested_signing_public_key: BASE64.encode(&requested_signing_public_key),
        requested_at,
        expires_at,
    }))
}

fn row_to_grant_response(row: &sqlx::postgres::PgRow) -> Result<DeviceGrantResponse, AppError> {
    let key: Vec<u8> = row.try_get("requested_signing_public_key")?;
    Ok(DeviceGrantResponse {
        id: row.try_get("id")?,
        status: row.try_get("status")?,
        device_label: row.try_get("device_label")?,
        requested_signing_public_key: BASE64.encode(&key),
        requested_at: row.try_get("requested_at")?,
        expires_at: row.try_get("expires_at")?,
    })
}

#[derive(Deserialize, utoipa::IntoParams)]
pub struct ListDeviceGrantsQuery {
    /// Filters to exactly this status when present (e.g. `?status=pending`
    /// for the approval UI); returns every grant for the caller's identity
    /// when omitted.
    pub status: Option<String>,
}

/// `GET /me/devices/grants?status=…` — every grant the caller's identity
/// has requested, from any device (used both by a trusted device polling
/// for pending requests to approve, and by the requesting device polling
/// its own request's status).
#[utoipa::path(
    get,
    path = "/me/devices/grants",
    tag = "devices",
    params(ListDeviceGrantsQuery),
    responses((status = 200, description = "List of device grant entries", body = Vec<DeviceGrantResponse>)),
)]
pub async fn list_device_grants(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListDeviceGrantsQuery>,
) -> Result<Json<Vec<DeviceGrantResponse>>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let rows = sqlx::query(
        r#"
        SELECT id, status, device_label, requested_signing_public_key, requested_at, expires_at
        FROM device_grants
        WHERE identity_id = $1 AND ($2::text IS NULL OR status = $2)
        ORDER BY requested_at DESC
        "#,
    )
    .bind(identity_id)
    .bind(&query.status)
    .fetch_all(&state.pool)
    .await?;

    let grants = rows
        .iter()
        .map(row_to_grant_response)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(grants))
}

/// `GET /me/devices/grants/:id` — the requesting device polls this for its
/// own grant's status until it flips to `approved`. Scoped to the caller's
/// own identity like every other read here, so one identity can never poll
/// another's pending grant.
#[utoipa::path(
    get,
    path = "/me/devices/grants/{id}",
    tag = "devices",
    params(("id" = Uuid, Path)),
    responses((status = 200, description = "The device grant", body = DeviceGrantResponse)),
)]
pub async fn get_device_grant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(grant_id): Path<Uuid>,
) -> Result<Json<DeviceGrantResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let row = sqlx::query(
        r#"
        SELECT id, status, device_label, requested_signing_public_key, requested_at, expires_at
        FROM device_grants
        WHERE id = $1 AND identity_id = $2
        "#,
    )
    .bind(grant_id)
    .bind(identity_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::DeviceGrantNotFound)?;

    Ok(Json(row_to_grant_response(&row)?))
}

struct PendingGrant {
    requested_signing_public_key: Vec<u8>,
    device_label: Option<String>,
}

async fn fetch_pending_grant(
    state: &AppState,
    identity_id: IdentityId,
    grant_id: Uuid,
) -> Result<PendingGrant, AppError> {
    let row = sqlx::query(
        r#"
        SELECT requested_signing_public_key, device_label, expires_at
        FROM device_grants
        WHERE id = $1 AND identity_id = $2 AND status = 'pending'
        "#,
    )
    .bind(grant_id)
    .bind(identity_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::DeviceGrantNotFound)?;

    let expires_at: OffsetDateTime = row.try_get("expires_at")?;
    if expires_at < OffsetDateTime::now_utc() {
        return Err(AppError::DeviceGrantExpired);
    }

    Ok(PendingGrant {
        requested_signing_public_key: row.try_get("requested_signing_public_key")?,
        device_label: row.try_get("device_label")?,
    })
}

#[derive(Deserialize, ToSchema)]
pub struct ApproveDeviceGrantRequest {
    /// Which of the caller's own `identity_signing_keys` rows is approving
    /// this grant — must belong to the caller's identity and not be
    /// revoked.
    pub approver_signing_key_id: Uuid,
    /// Base64-encoded Ed25519 signature over
    /// `device_grant_approval_signing_bytes_v2(grant_id, identity_id, requested_signing_public_key)`,
    /// produced by `approver_signing_key_id`'s key.
    pub signature: String,
}

#[derive(Serialize, ToSchema)]
pub struct DeviceResponse {
    pub id: Uuid,
    pub label: Option<String>,
    /// Base64-encoded — lets a device that only knows its own local secret
    /// key (never sent anywhere) find which server-side row is *itself* by
    /// deriving and comparing its public key client-side, e.g. to learn
    /// its own `approver_signing_key_id` before approving someone else's
    /// grant. Public key material only; no risk in exposing it the same
    /// way `identity_created`'s payload already does.
    pub public_key: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub added_at: OffsetDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = "date-time")]
    pub revoked_at: Option<OffsetDateTime>,
}

/// `POST /me/devices/grants/:id/approve` — the security-bearing step this
/// whole module exists for. Verifies the approving key is one of the
/// caller's own, still active (not revoked), and that its signature really
/// covers this exact grant and requested key before ever inserting
/// anything — satisfies the ticket's invariant that a grant only ever
/// comes from a device that itself already passed a real WebAuthn ceremony
/// (every row in `identity_signing_keys` only exists because
/// `register_finish` or a prior approval put it there).
#[utoipa::path(
    post,
    path = "/me/devices/grants/{id}/approve",
    tag = "devices",
    params(("id" = Uuid, Path)),
    request_body = ApproveDeviceGrantRequest,
    responses((status = 200, description = "The resulting device", body = DeviceResponse)),
)]
pub async fn approve_device_grant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(grant_id): Path<Uuid>,
    Json(body): Json<ApproveDeviceGrantRequest>,
) -> Result<Json<DeviceResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;
    let grant = fetch_pending_grant(&state, identity_id, grant_id).await?;

    let approver_row = sqlx::query(
        "SELECT public_key FROM identity_signing_keys WHERE id = $1 AND identity_id = $2 AND revoked_at IS NULL",
    )
    .bind(body.approver_signing_key_id)
    .bind(identity_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::ApproverKeyInvalid)?;
    let approver_public_key: Vec<u8> = approver_row.try_get("public_key")?;

    let signature_bytes = BASE64
        .decode(&body.signature)
        .map_err(|_| AppError::InvalidGrantSignature)?;
    let requested_key: [u8; 32] = grant
        .requested_signing_public_key
        .as_slice()
        .try_into()
        .map_err(|_| AppError::InvalidGrantSignature)?;
    let signing_bytes =
        device_grant_approval_signing_bytes_v2(grant_id, &identity_id, &requested_key);
    if !verify_event_signature(&approver_public_key, &signing_bytes, &signature_bytes) {
        return Err(AppError::InvalidGrantSignature);
    }

    let mut tx = state.pool.begin().await?;
    crate::identity_chain::ensure_not_forked(&mut *tx, identity_id).await?;

    let new_key_row = sqlx::query(
        "INSERT INTO identity_signing_keys (identity_id, public_key, label) VALUES ($1, $2, $3) RETURNING id, added_at",
    )
    .bind(identity_id)
    .bind(&grant.requested_signing_public_key)
    .bind(&grant.device_label)
    .fetch_one(&mut *tx)
    .await?;
    let new_key_id: Uuid = new_key_row.try_get("id")?;
    let added_at: OffsetDateTime = new_key_row.try_get("added_at")?;

    let resolved = sqlx::query(
        r#"
        UPDATE device_grants
        SET status = 'approved', approved_by_signing_key_id = $2, approved_at = now()
        WHERE id = $1 AND status = 'pending'
        "#,
    )
    .bind(grant_id)
    .bind(body.approver_signing_key_id)
    .execute(&mut *tx)
    .await?;
    if resolved.rows_affected() == 0 {
        // Resolved by a concurrent approval between the fetch above and here.
        return Err(AppError::DeviceGrantNotFound);
    }

    let mut event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::IdentitySigningKeyAdded
            .as_str()
            .to_string(),
        issuer: identity_ref(identity_id, "signing_key_added"),
        subject: identity_ref(identity_id, "signing_key_added"),
        payload: serde_json::to_value(IdentitySigningKeyAddedPayload {
            signing_key_id: new_key_id,
            public_key: BASE64.encode(&grant.requested_signing_public_key),
            device_label: grant.device_label.clone(),
            approved_by_signing_key_id: body.approver_signing_key_id,
            identity_id,
            kind: SIGNING_KEY_KIND_DEVICE_GRANT.to_string(),
            grant_id: Some(grant_id),
            approval_signature: Some(body.signature.clone()),
        })
        .expect("IdentitySigningKeyAddedPayload should serialize"),
        timestamp: added_at,
        version: 2,
        identity_chain: None,
    };
    crate::identity_chain::assign(&mut tx, &mut event).await?;
    outbox::enqueue(&mut tx, &event).await?;
    state.indexer.apply_in_tx(&mut tx, &event).await?;

    tx.commit().await?;
    state.indexer.apply_after_commit(&event).await?;

    Ok(Json(DeviceResponse {
        id: new_key_id,
        label: grant.device_label,
        public_key: BASE64.encode(&grant.requested_signing_public_key),
        added_at,
        revoked_at: None,
    }))
}

/// `GET /me/devices` — every signing key (active or revoked) registered to
/// the caller's identity, for the Hub's device-list/revoke UI.
#[utoipa::path(
    get,
    path = "/me/devices",
    tag = "devices",
    responses((status = 200, description = "List of device entries", body = Vec<DeviceResponse>)),
)]
pub async fn list_devices(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<DeviceResponse>>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let rows = sqlx::query(
        "SELECT id, label, public_key, added_at, revoked_at FROM identity_signing_keys WHERE identity_id = $1 ORDER BY added_at",
    )
    .bind(identity_id)
    .fetch_all(&state.pool)
    .await?;

    let mut devices = Vec::with_capacity(rows.len());
    for row in rows {
        let public_key: Vec<u8> = row.try_get("public_key")?;
        devices.push(DeviceResponse {
            id: row.try_get("id")?,
            label: row.try_get("label")?,
            public_key: BASE64.encode(&public_key),
            added_at: row.try_get("added_at")?,
            revoked_at: row.try_get("revoked_at")?,
        });
    }
    Ok(Json(devices))
}

#[derive(Deserialize, ToSchema)]
pub struct RenameDeviceRequest {
    pub label: String,
}

/// `PATCH /me/devices/:id` — issue #145: the first device (registered by
/// `handlers::register_finish`) previously had no way to be labeled after
/// the fact, and no device could be renamed at all. Same ownership check as
/// `revoke_device` — any authenticated session for the identity may rename
/// any of its own signing-key rows, active or revoked, unilaterally.
#[utoipa::path(
    patch,
    path = "/me/devices/{id}",
    tag = "devices",
    params(("id" = Uuid, Path)),
    request_body = RenameDeviceRequest,
    responses((status = 200, description = "The resulting device", body = DeviceResponse)),
)]
pub async fn rename_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(signing_key_id): Path<Uuid>,
    Json(body): Json<RenameDeviceRequest>,
) -> Result<Json<DeviceResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let row = sqlx::query(
        r#"
        UPDATE identity_signing_keys
        SET label = $3
        WHERE id = $1 AND identity_id = $2
        RETURNING id, label, public_key, added_at, revoked_at
        "#,
    )
    .bind(signing_key_id)
    .bind(identity_id)
    .bind(&body.label)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::SigningKeyNotFound)?;

    let public_key: Vec<u8> = row.try_get("public_key")?;
    Ok(Json(DeviceResponse {
        id: row.try_get("id")?,
        label: row.try_get("label")?,
        public_key: BASE64.encode(&public_key),
        added_at: row.try_get("added_at")?,
        revoked_at: row.try_get("revoked_at")?,
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct RevokeDeviceRequest {
    /// The caller's own active signing key that signs the revocation (may be the key being revoked).
    pub revoked_by_signing_key_id: Uuid,
    /// Base64 Ed25519 signature by `revoked_by_signing_key_id` over
    /// `avalon_protocol::identity_id::signing_key_revoked_signing_bytes_v2`.
    pub signature: String,
}

/// `POST /me/devices/:id/revoke` — any authenticated session for the identity can
/// revoke any of its signing keys (including the signer's own, for self-rotation), provided
/// the request carries a valid signature from an active key of the same identity.
#[utoipa::path(
    post,
    path = "/me/devices/{id}/revoke",
    tag = "devices",
    params(("id" = Uuid, Path)),
    request_body = RevokeDeviceRequest,
    responses((status = 200, description = "Signing key revoked")),
)]
pub async fn revoke_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(signing_key_id): Path<Uuid>,
    Json(body): Json<RevokeDeviceRequest>,
) -> Result<(), AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let revoker_row = sqlx::query(
        "SELECT public_key FROM identity_signing_keys WHERE id = $1 AND identity_id = $2 AND revoked_at IS NULL",
    )
    .bind(body.revoked_by_signing_key_id)
    .bind(identity_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::ApproverKeyInvalid)?;
    let revoker_public_key: Vec<u8> = revoker_row.try_get("public_key")?;
    let signature_bytes = BASE64
        .decode(&body.signature)
        .map_err(|_| AppError::InvalidEventSignature)?;
    let signing_bytes = signing_key_revoked_signing_bytes_v2(
        &identity_id,
        signing_key_id,
        body.revoked_by_signing_key_id,
    );
    if !verify_event_signature(&revoker_public_key, &signing_bytes, &signature_bytes) {
        return Err(AppError::InvalidEventSignature);
    }

    let mut tx = state.pool.begin().await?;
    crate::identity_chain::ensure_not_forked(&mut *tx, identity_id).await?;

    let revoked = sqlx::query(
        "UPDATE identity_signing_keys SET revoked_at = now() WHERE id = $1 AND identity_id = $2 AND revoked_at IS NULL",
    )
    .bind(signing_key_id)
    .bind(identity_id)
    .execute(&mut *tx)
    .await?;
    if revoked.rows_affected() == 0 {
        return Err(AppError::SigningKeyNotFound);
    }

    let mut event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::IdentitySigningKeyRevoked
            .as_str()
            .to_string(),
        issuer: identity_ref(identity_id, "signing_key_revoked"),
        subject: identity_ref(identity_id, "signing_key_revoked"),
        payload: serde_json::to_value(IdentitySigningKeyRevokedPayload {
            identity_id,
            signing_key_id,
            revoked_by_signing_key_id: body.revoked_by_signing_key_id,
            signature: body.signature.clone(),
        })
        .expect("IdentitySigningKeyRevokedPayload should serialize"),
        timestamp: OffsetDateTime::now_utc(),
        version: 2,
        identity_chain: None,
    };
    crate::identity_chain::assign(&mut tx, &mut event).await?;
    outbox::enqueue(&mut tx, &event).await?;
    state.indexer.apply_in_tx(&mut tx, &event).await?;

    tx.commit().await?;
    state.indexer.apply_after_commit(&event).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — these exercise the pure logic
    //! only. The full request→approve→revoke flow is covered by
    //! `crates/server/tests/device_grants.rs`, gated `--ignored`.

    use super::*;

    #[test]
    fn identity_ref_namespaces_by_identity_and_verb() {
        let id = IdentityId::random_for_tests();
        let global_id = identity_ref(id, "signing_key_added");
        assert_eq!(
            global_id.as_str(),
            format!("identity:{id}:self:signing_key_added")
        );
    }
}
