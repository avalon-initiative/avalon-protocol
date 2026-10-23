//! Social recovery via an M-of-N set of trusted guardians —
//! the answer decided on for losing every registered device at once.
//! See `avalon-docs/protocol/identity.md`'s "Social recovery via M-of-N
//! guardians" section for the full
//! configure/request/approve/finalize state machine and its abuse-
//! resistance measures.

use avalon_protocol::ids::IdentityId;
use std::collections::HashSet;

use avalon_protocol::event_payloads::{
    IdentityRecoveredPayload, IdentityRecoveryApprovedPayload, IdentityRecoveryCancelledPayload,
    IdentityRecoveryConfiguredPayload, IdentityRecoveryRequestedPayload, RecoveryApprovalProof,
    RecoveryWithdrawalProof,
};
use avalon_protocol::events::{ProtocolEvent, ProtocolEventKindVariant};
use avalon_protocol::ids::GlobalId;
use avalon_protocol::recovery_rules::{
    parse_hex32, required_approvals, MIN_RECOVERY_THRESHOLD, RECOVERY_DELAY,
};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;
use webauthn_rs::prelude::*;

use crate::error::{AppError, IdPath};
use crate::friends::friend_partners;
use crate::handlers::authenticate;
use crate::outbox;
use crate::signature_gate::{canonical_message, require_fresh_signature};
use crate::state::AppState;
use utoipa::ToSchema;

const CEREMONY_TTL_MINUTES: i64 = 5;
const RECOVERY_START_CEREMONY_KIND: &str = "recovery_start";

/// An M-of-N scheme with more than this many guardians stops being
/// reviewable by a user choosing them ("who are all these people") and
/// starts looking like a mistake rather than a deliberate trust decision.
/// Not a protocol-level limit, just a sane UX ceiling.
const MAX_GUARDIANS: usize = 10;

/// How many recovery requests may be *initiated against* a single identity
/// within `RATE_LIMIT_WINDOW_HOURS` before further attempts are refused —
/// the abuse-resistance backstop for the necessarily-unauthenticated
/// initiation endpoint, on top of `recovery_requests_one_active_per_identity`
/// already capping concurrent attempts to one. Counts every request ever
/// created in the window regardless of outcome (cancelled ones included),
/// so repeatedly initiating-then-getting-vetoed doesn't reopen the door.
const MAX_RECOVERY_ATTEMPTS_PER_WINDOW: i64 = 5;
const RATE_LIMIT_WINDOW_HOURS: i64 = 24;

fn identity_ref(identity_id: IdentityId, verb: &str) -> GlobalId {
    GlobalId::new("identity", &identity_id.to_string(), "self", verb)
}

// ---------------------------------------------------------------------
// Pure guard functions — the actual state-machine invariants, factored
// out so they're unit-testable without a live Postgres, same convention
// `passkeys::guard_revoke_last_passkey` established.
// ---------------------------------------------------------------------

/// A guardian set must have at least one member and at most
/// [`MAX_GUARDIANS`]; the threshold must be satisfiable and at least the protocol minimum
/// (`MIN_RECOVERY_THRESHOLD..=guardian_count`).
pub(crate) fn validate_guardian_settings(
    guardian_count: usize,
    threshold: i32,
) -> Result<(), AppError> {
    if guardian_count == 0 {
        return Err(AppError::InvalidGuardianSet);
    }
    if guardian_count > MAX_GUARDIANS {
        return Err(AppError::InvalidGuardianSet);
    }
    if threshold < MIN_RECOVERY_THRESHOLD || threshold as usize > guardian_count {
        return Err(AppError::InvalidGuardianSet);
    }
    Ok(())
}

/// Whether the standing approvals clear the threshold frozen at request time (never below the protocol
/// minimum); `>=` because the count can jump past it in one step.
pub(crate) fn meets_threshold(approvals_count: i64, threshold_at_request: i32) -> bool {
    approvals_count >= required_approvals(threshold_at_request) as i64
}

/// Whether a request may try to finalize: it has its approvals. The delay is verified from cosigned heads.
pub(crate) fn guard_can_finalize(status: &str) -> Result<(), AppError> {
    match status {
        "completed" => Err(AppError::RecoveryAlreadyResolved),
        "delay" => Ok(()),
        _ => Err(AppError::RecoveryNotReadyToFinalize),
    }
}

/// Only the identity's own owner may veto: the veto is a chained event only an owner key can sign.
/// A guardian's recourse is to withdraw their own approval.
pub(crate) fn guard_cancel_authority(
    caller: IdentityId,
    identity_id: IdentityId,
) -> Result<(), AppError> {
    if caller == identity_id {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

/// Rolling-window abuse-resistance for the unauthenticated initiation
/// endpoint (see module docs). `recent_count` is however many requests
/// already exist for this identity within the window, counted *before*
/// this attempt.
pub(crate) fn guard_rate_limit(recent_count: i64) -> Result<(), AppError> {
    if recent_count >= MAX_RECOVERY_ATTEMPTS_PER_WINDOW {
        return Err(AppError::RecoveryRateLimited);
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Guardian configuration — session-authenticated.
// ---------------------------------------------------------------------

#[derive(Deserialize, ToSchema)]
pub struct SetGuardiansRequest {
    pub guardian_ids: Vec<IdentityId>,
    pub threshold: i32,
    /// #697/#698: only required when this write *removes* an existing
    /// guardian or *raises* the threshold — see the conditional check in
    /// `set_guardians` below. Naming/adding guardians or lowering the
    /// threshold stays ambient.
    pub signing_key_id: Option<Uuid>,
    pub signature: Option<String>,
    /// The owner's signature of the `identity.recovery_configured` event this authors.
    pub chain_event: crate::identity_chain::ChainEventSignature,
}

#[derive(Serialize, ToSchema)]
pub struct GuardianSettingsResponse {
    pub guardian_ids: Vec<IdentityId>,
    pub threshold: i32,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = "date-time")]
    pub updated_at: Option<OffsetDateTime>,
}

/// `PUT /me/recovery/guardians` — (re)configures the caller's guardian set
/// and threshold in one call, requiring the caller's *current* session
/// (`authenticate`) the whole invariant rests on. Every guardian must be a
/// current friend (the network-level primitive is deliberately the
/// only pool this draws from — see the ticket) and not the caller
/// themselves; the full set is validated together via
/// [`validate_guardian_settings`] rather than incrementally, so a client
/// can't build up an invalid configuration one add-guardian call at a
/// time. Replaces the set wholesale (delete-then-insert in one
/// transaction) rather than diffing — simpler, and this isn't a
/// high-frequency operation.
#[utoipa::path(
    put,
    path = "/me/recovery/guardians",
    tag = "recovery",
    request_body = SetGuardiansRequest,
    responses((status = 200, description = "The resulting guardian settings", body = GuardianSettingsResponse)),
)]
pub async fn set_guardians(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SetGuardiansRequest>,
) -> Result<Json<GuardianSettingsResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let mut unique_guardians: Vec<IdentityId> = body.guardian_ids.clone();
    unique_guardians.sort();
    unique_guardians.dedup();
    if unique_guardians.len() != body.guardian_ids.len() {
        return Err(AppError::InvalidGuardianSet);
    }
    if unique_guardians.contains(&identity_id) {
        return Err(AppError::InvalidGuardianSet);
    }
    validate_guardian_settings(unique_guardians.len(), body.threshold)?;

    let friends = friend_partners(&state, identity_id).await?;
    if !unique_guardians.iter().all(|g| friends.contains(g)) {
        return Err(AppError::InvalidGuardianSet);
    }

    // #697/#698: removing a guardian or raising the threshold can neuter
    // the owner's own recovery path — adding guardians or lowering the
    // threshold only ever makes recovery easier, so those stay ambient.
    let previous = fetch_guardian_settings(&state, identity_id).await?;
    let previous_guardian_ids: HashSet<IdentityId> =
        previous.guardian_ids.iter().copied().collect();
    let removes_a_guardian = !previous_guardian_ids
        .iter()
        .all(|g| unique_guardians.contains(g));
    let raises_threshold = body.threshold > previous.threshold;
    if removes_a_guardian || raises_threshold {
        let message = canonical_message(
            "recovery.guardians.set",
            &[
                &identity_id.to_string(),
                &unique_guardians
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
                &body.threshold.to_string(),
            ],
        );
        require_fresh_signature(
            &state,
            identity_id,
            &message,
            body.signing_key_id,
            body.signature.as_deref(),
        )
        .await?;
    }

    let mut tx = state.pool.begin().await?;
    crate::identity_chain::ensure_not_forked(&mut *tx, identity_id).await?;

    sqlx::query("DELETE FROM recovery_guardians WHERE identity_id = $1")
        .bind(identity_id)
        .execute(&mut *tx)
        .await?;
    for guardian_id in &unique_guardians {
        sqlx::query(
            "INSERT INTO recovery_guardians (identity_id, guardian_identity_id) VALUES ($1, $2)",
        )
        .bind(identity_id)
        .bind(guardian_id)
        .execute(&mut *tx)
        .await?;
    }

    let updated_at = OffsetDateTime::now_utc();
    sqlx::query(
        r#"
        INSERT INTO recovery_guardian_settings (identity_id, threshold, updated_at)
        VALUES ($1, $2, $3)
        ON CONFLICT (identity_id) DO UPDATE SET threshold = $2, updated_at = $3
        "#,
    )
    .bind(identity_id)
    .bind(body.threshold)
    .bind(updated_at)
    .execute(&mut *tx)
    .await?;

    let mut event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::IdentityRecoveryConfigured
            .as_str()
            .to_string(),
        issuer: identity_ref(identity_id, "recovery_configured"),
        subject: identity_ref(identity_id, "recovery_configured"),
        payload: serde_json::to_value(IdentityRecoveryConfiguredPayload {
            guardian_ids: unique_guardians.clone(),
            threshold: body.threshold,
        })
        .expect("IdentityRecoveryConfiguredPayload should serialize"),
        timestamp: updated_at,
        version: 1,
        identity_chain: None,
    };
    crate::identity_chain::place_signed(&state, &mut tx, &mut event, &body.chain_event, None)
        .await?;
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(GuardianSettingsResponse {
        guardian_ids: unique_guardians,
        threshold: body.threshold,
        updated_at: Some(updated_at),
    }))
}

/// `GET /me/recovery/guardians` — the caller's own current configuration.
/// An identity with none configured gets an empty list and threshold 0,
/// not a 404 — "not configured yet" is a normal state, not an error.
#[utoipa::path(
    get,
    path = "/me/recovery/guardians",
    tag = "recovery",
    responses((status = 200, description = "The guardian settings", body = GuardianSettingsResponse)),
)]
pub async fn get_guardians(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<GuardianSettingsResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;
    fetch_guardian_settings(&state, identity_id).await.map(Json)
}

async fn fetch_guardian_settings(
    state: &AppState,
    identity_id: IdentityId,
) -> Result<GuardianSettingsResponse, AppError> {
    let guardian_rows = sqlx::query(
        "SELECT guardian_identity_id FROM recovery_guardians WHERE identity_id = $1 ORDER BY added_at",
    )
    .bind(identity_id)
    .fetch_all(&state.pool)
    .await?;
    let mut guardian_ids = Vec::with_capacity(guardian_rows.len());
    for row in guardian_rows {
        guardian_ids.push(row.try_get("guardian_identity_id")?);
    }

    let settings_row = sqlx::query(
        "SELECT threshold, updated_at FROM recovery_guardian_settings WHERE identity_id = $1",
    )
    .bind(identity_id)
    .fetch_optional(&state.pool)
    .await?;
    let (threshold, updated_at) = match settings_row {
        Some(row) => (row.try_get("threshold")?, Some(row.try_get("updated_at")?)),
        None => (0, None),
    };

    Ok(GuardianSettingsResponse {
        guardian_ids,
        threshold,
        updated_at,
    })
}

#[derive(Serialize, ToSchema)]
pub struct GuardianOfSummary {
    pub identity_id: IdentityId,
    pub display_name: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub added_at: OffsetDateTime,
}

/// `GET /me/recovery/guardian-of` — every identity that currently names the
/// caller as one of their recovery guardians (the opt-out consent
/// model: a guardian can always see who's relying on them and self-remove
/// via [`resign_guardian`] below, without the owner's cooperation — there is
/// no accept step, matching `set_guardians`'s existing "active the moment
/// the owner names you" behavior).
#[utoipa::path(
    get,
    path = "/me/recovery/guardian-of",
    tag = "recovery",
    responses((status = 200, description = "List of guardian of summary entries", body = Vec<GuardianOfSummary>)),
)]
pub async fn guardian_of(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<GuardianOfSummary>>, AppError> {
    let caller = authenticate(&state, &headers).await?;

    let rows = sqlx::query(
        r#"
        SELECT g.identity_id, g.added_at, p.display_name
        FROM recovery_guardians g
        JOIN profiles p ON p.identity_id = g.identity_id
        WHERE g.guardian_identity_id = $1
        ORDER BY g.added_at
        "#,
    )
    .bind(caller)
    .fetch_all(&state.pool)
    .await?;

    let mut summaries = Vec::with_capacity(rows.len());
    for row in rows {
        summaries.push(GuardianOfSummary {
            identity_id: row.try_get("identity_id")?,
            display_name: row.try_get("display_name")?,
            added_at: row.try_get("added_at")?,
        });
    }
    Ok(Json(summaries))
}

/// `DELETE /me/recovery/guardian-of/{identity_id}` — a guardian removing
/// themselves from someone else's guardian set, without that owner's
/// cooperation. If this drops the owner's guardian count below
/// their configured threshold, the threshold is clamped down to the new
/// count instead — the same "recovery must stay satisfiable" invariant
/// [`validate_guardian_settings`] enforces on the owner's own writes, kept
/// true here too rather than left as a silent trap the owner discovers only
/// when trying to actually recover.
#[utoipa::path(
    delete,
    path = "/me/recovery/guardian-of/{identity_id}",
    tag = "recovery",
    params(("identity_id" = IdentityId, Path)),
    responses((status = 200, description = "Resigned as guardian")),
)]
pub async fn resign_guardian(
    State(state): State<AppState>,
    headers: HeaderMap,
    IdPath(identity_id): IdPath<IdentityId>,
) -> Result<Json<()>, AppError> {
    let caller = authenticate(&state, &headers).await?;

    let mut tx = state.pool.begin().await?;

    let removed = sqlx::query(
        "DELETE FROM recovery_guardians WHERE identity_id = $1 AND guardian_identity_id = $2",
    )
    .bind(identity_id)
    .bind(caller)
    .execute(&mut *tx)
    .await?;
    if removed.rows_affected() == 0 {
        return Err(AppError::NotAGuardian);
    }

    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM recovery_guardians WHERE identity_id = $1")
            .bind(identity_id)
            .fetch_one(&mut *tx)
            .await?;

    sqlx::query(
        r#"
        UPDATE recovery_guardian_settings
        SET threshold = LEAST(threshold, $2), updated_at = now()
        WHERE identity_id = $1 AND threshold > $2
        "#,
    )
    .bind(identity_id)
    .bind(remaining as i32)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(Json(()))
}

async fn current_guardian_set(
    state: &AppState,
    identity_id: IdentityId,
) -> Result<HashSet<IdentityId>, AppError> {
    let rows =
        sqlx::query("SELECT guardian_identity_id FROM recovery_guardians WHERE identity_id = $1")
            .bind(identity_id)
            .fetch_all(&state.pool)
            .await?;
    let mut set = HashSet::with_capacity(rows.len());
    for row in rows {
        set.insert(row.try_get("guardian_identity_id")?);
    }
    Ok(set)
}

// ---------------------------------------------------------------------
// Recovery initiation — necessarily unauthenticated (see module docs).
// ---------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct RecoveryStartCeremonyState {
    identity_id: IdentityId,
    device_label: Option<String>,
    /// Chain hash of the recovery configuration the request will rely on, fixed at start.
    config_event_hash: String,
    webauthn_state: PasskeyRegistration,
}

#[derive(Deserialize, ToSchema)]
pub struct RecoveryStartRequest {
    pub identity_id: IdentityId,
    pub device_label: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct RecoveryStartResponse {
    pub ticket_id: Uuid,
    #[schema(value_type = Object)]
    pub challenge: CreationChallengeResponse,
    /// Lowercase hex chain hash of the owner's current `identity.recovery_configured` event: the
    /// configuration the request must name and sign.
    pub config_event_hash: String,
}

/// The chain hash of the identity's latest accepted `identity.recovery_configured` event.
async fn current_config_hash(
    state: &AppState,
    identity_id: IdentityId,
) -> Result<Option<[u8; 32]>, AppError> {
    let mut tx = state.pool.begin().await?;
    let events =
        avalon_indexer::identity_chain_store::accepted_events(&mut tx, identity_id).await?;
    Ok(events
        .iter()
        .rev()
        .find(|e| e.kind == "identity.recovery_configured")
        .and_then(|e| avalon_protocol::identity_chain_wire::event_hash(e).ok()))
}

async fn recent_request_count(state: &AppState, identity_id: IdentityId) -> Result<i64, AppError> {
    let window_start = OffsetDateTime::now_utc() - time::Duration::hours(RATE_LIMIT_WINDOW_HOURS);
    let row = sqlx::query(
        "SELECT COUNT(*) AS count FROM recovery_requests WHERE identity_id = $1 AND requested_at >= $2",
    )
    .bind(identity_id)
    .bind(window_start)
    .fetch_one(&state.pool)
    .await?;
    Ok(row.try_get("count")?)
}

/// `POST /recovery/requests/start` — begins the new device's WebAuthn
/// registration ceremony. Unauthenticated by necessity (the caller has no
/// valid session for `identity_id` — that's the entire premise of
/// recovery), so this and `finish_request` below are the one deliberate
/// exception to this crate's "every route requires a session" norm.
/// Guarded three ways rather than left as an open door: `identity_id` must
/// name a real identity, and must actually have guardians configured (an
/// unconfigured identity can never satisfy any M, so there's nothing to
/// spam toward) — both cases return the exact same
/// `AppError::RecoveryNotAvailable` (same status, same body), so an
/// unauthenticated prober can never distinguish "this identity doesn't
/// exist" from "this identity exists but has no guardians set up." The
/// rolling-window rate limit ([`guard_rate_limit`]) is checked *before*
/// any WebAuthn ceremony work happens, since that ceremony is the
/// expensive part.
#[utoipa::path(
    post,
    path = "/recovery/requests/start",
    tag = "recovery",
    request_body = RecoveryStartRequest,
    responses((status = 200, description = "The resulting recovery start", body = RecoveryStartResponse)),
)]
pub async fn start_request(
    State(state): State<AppState>,
    Json(body): Json<RecoveryStartRequest>,
) -> Result<Json<RecoveryStartResponse>, AppError> {
    let identity_exists = sqlx::query("SELECT 1 FROM identities WHERE id = $1")
        .bind(body.identity_id)
        .fetch_optional(&state.pool)
        .await?;
    if identity_exists.is_none() {
        return Err(AppError::RecoveryNotAvailable);
    }

    let settings_row =
        sqlx::query("SELECT threshold FROM recovery_guardian_settings WHERE identity_id = $1")
            .bind(body.identity_id)
            .fetch_optional(&state.pool)
            .await?;
    let Some(settings_row) = settings_row else {
        return Err(AppError::RecoveryNotAvailable);
    };
    let threshold: i32 = settings_row.try_get("threshold")?;
    if threshold < MIN_RECOVERY_THRESHOLD {
        return Err(AppError::RecoveryNotAvailable);
    }
    let config_event_hash = current_config_hash(&state, body.identity_id)
        .await?
        .ok_or(AppError::RecoveryNotAvailable)?;

    let recent_count = recent_request_count(&state, body.identity_id).await?;
    guard_rate_limit(recent_count)?;

    let existing_credential_rows =
        sqlx::query("SELECT credential_id FROM identity_keys WHERE identity_id = $1")
            .bind(body.identity_id)
            .fetch_all(&state.pool)
            .await?;
    let exclude_credentials: Vec<CredentialID> = existing_credential_rows
        .iter()
        .map(|row| -> Result<CredentialID, AppError> {
            let bytes: Vec<u8> = row.try_get("credential_id")?;
            Ok(CredentialID::from(bytes))
        })
        .collect::<Result<_, _>>()?;

    let (challenge, webauthn_state) = state
        .webauthn
        .start_passkey_registration(
            body.identity_id.webauthn_user_handle(),
            &body.identity_id.to_string(),
            &format!("{} (recovery)", body.identity_id),
            Some(exclude_credentials),
        )
        .map_err(|_| AppError::WebauthnFailed)?;

    let ceremony = RecoveryStartCeremonyState {
        identity_id: body.identity_id,
        device_label: body.device_label,
        config_event_hash: hex::encode(config_event_hash),
        webauthn_state,
    };
    let ticket_id = Uuid::new_v4();
    let expires_at = OffsetDateTime::now_utc() + time::Duration::minutes(CEREMONY_TTL_MINUTES);

    sqlx::query(
        "INSERT INTO webauthn_ceremonies (id, kind, state, expires_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(ticket_id)
    .bind(RECOVERY_START_CEREMONY_KIND)
    .bind(serde_json::to_value(&ceremony).expect("ceremony state should serialize"))
    .bind(expires_at)
    .execute(&state.pool)
    .await?;

    Ok(Json(RecoveryStartResponse {
        ticket_id,
        challenge,
        config_event_hash: hex::encode(config_event_hash),
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct RecoveryFinishRequest {
    pub ticket_id: Uuid,
    #[schema(value_type = Object)]
    pub webauthn_credential: RegisterPublicKeyCredential,
    /// Base64 Ed25519 public key the recovered identity will sign with.
    pub new_signing_public_key: String,
    /// The request id, chosen by the requester; it is also the id of the new signing key.
    pub request_id: Uuid,
    /// Base64 signature by the new key over `recovery_request_signing_bytes` for this request, key
    /// and the `config_event_hash` the start response named.
    pub signature: String,
}

#[derive(Serialize, ToSchema)]
pub struct RecoveryRequestResponse {
    pub id: Uuid,
    pub identity_id: IdentityId,
    pub status: String,
    pub threshold: i32,
    /// Approvals that stand, that is, not withdrawn.
    pub approvals_count: i64,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub requested_at: OffsetDateTime,
    /// First covering cosigned head time plus the protocol delay; null until one exists. Display only.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = "date-time")]
    pub delay_ends_at: Option<OffsetDateTime>,
    /// Base64 of the key the recovered identity will sign with.
    pub new_signing_public_key: String,
    /// Lowercase hex chain hash of the recovery configuration the request relies on.
    pub config_event_hash: String,
    /// Lowercase hex ledger entry hash of the request marker; guardians approve once it exists.
    pub request_entry_hash: Option<String>,
}

/// `POST /recovery/requests/finish` — completes the ceremony and creates
/// the `recovery_requests` row. Re-checks guardian configuration and the
/// rate limit (both may have changed since `start`) and relies on
/// `recovery_requests_one_active_per_identity`'s unique index as the final
/// word on "at most one active attempt" — a second `finish` racing this
/// one for the same identity loses to the constraint, not to a
/// check-then-act gap in application code.
#[utoipa::path(
    post,
    path = "/recovery/requests/finish",
    tag = "recovery",
    request_body = RecoveryFinishRequest,
    responses((status = 200, description = "The resulting recovery request", body = RecoveryRequestResponse)),
)]
pub async fn finish_request(
    State(state): State<AppState>,
    Json(body): Json<RecoveryFinishRequest>,
) -> Result<Json<RecoveryRequestResponse>, AppError> {
    let row = sqlx::query(
        "DELETE FROM webauthn_ceremonies WHERE id = $1 AND kind = $2 RETURNING state, expires_at",
    )
    .bind(body.ticket_id)
    .bind(RECOVERY_START_CEREMONY_KIND)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::CeremonyNotFound)?;

    let expires_at: OffsetDateTime = row.try_get("expires_at")?;
    if expires_at < OffsetDateTime::now_utc() {
        return Err(AppError::CeremonyExpired);
    }
    let state_json: serde_json::Value = row.try_get("state")?;
    let ceremony: RecoveryStartCeremonyState =
        serde_json::from_value(state_json).map_err(|_| AppError::CeremonyNotFound)?;

    // Same unified error as `start_request` above, for the same reason —
    // even though reaching this point already required a valid ceremony
    // ticket (not something an outside prober can guess), re-checking
    // guardian configuration here (it could have changed between start and
    // finish) should stay consistent rather than reintroducing a
    // distinguishable signal on a code path that's easy to overlook later.
    let settings_row =
        sqlx::query("SELECT threshold FROM recovery_guardian_settings WHERE identity_id = $1")
            .bind(ceremony.identity_id)
            .fetch_optional(&state.pool)
            .await?;
    let Some(settings_row) = settings_row else {
        return Err(AppError::RecoveryNotAvailable);
    };
    let threshold: i32 = settings_row.try_get("threshold")?;
    if threshold < MIN_RECOVERY_THRESHOLD {
        return Err(AppError::RecoveryNotAvailable);
    }
    let config_hash = parse_hex32(&ceremony.config_event_hash).ok_or(AppError::CeremonyNotFound)?;
    if current_config_hash(&state, ceremony.identity_id).await? != Some(config_hash) {
        return Err(AppError::RecoveryConfigChanged);
    }

    let recent_count = recent_request_count(&state, ceremony.identity_id).await?;
    guard_rate_limit(recent_count)?;

    let new_key: [u8; 32] = BASE64
        .decode(&body.new_signing_public_key)
        .ok()
        .and_then(|raw| raw.try_into().ok())
        .filter(|key| avalon_protocol::ed25519_key::parse_ed25519_public_key(key).is_some())
        .ok_or(AppError::InvalidRecoveryKey)?;
    let signature: [u8; 64] = BASE64
        .decode(&body.signature)
        .ok()
        .and_then(|raw| raw.try_into().ok())
        .ok_or(AppError::InvalidAuthorSignature)?;
    let request_bytes = avalon_protocol::identity_id::recovery_request_signing_bytes(
        state.chain.network_id(),
        &ceremony.identity_id,
        body.request_id,
        &new_key,
        &config_hash,
    );
    let signed =
        avalon_protocol::ed25519_key::parse_ed25519_public_key(&new_key).is_some_and(|key| {
            avalon_protocol::ed25519_key::verify_strict_signature(&key, &request_bytes, &signature)
        });
    if !signed {
        return Err(AppError::InvalidAuthorSignature);
    }
    let passkey = state
        .webauthn
        .finish_passkey_registration(&body.webauthn_credential, &ceremony.webauthn_state)
        .map_err(|_| AppError::WebauthnFailed)?;
    let passkey_json = serde_json::to_value(&passkey).expect("Passkey should serialize");
    let credential_id: &[u8] = passkey.cred_id().as_ref();

    let mut tx = state.pool.begin().await?;

    let requested_at = OffsetDateTime::now_utc();
    let request_id = body.request_id;
    let request_event_id = Uuid::new_v4();
    let inserted = sqlx::query(
        r#"
        INSERT INTO recovery_requests
            (id, identity_id, pending_passkey_data, pending_credential_id, pending_device_label,
             threshold_at_request, status, requested_at, new_signing_public_key,
             config_event_hash, request_signature, request_event_id)
        VALUES ($1, $2, $3, $4, $5, $6, 'pending_approvals', $7, $8, $9, $10, $11)
        "#,
    )
    .bind(request_id)
    .bind(ceremony.identity_id)
    .bind(&passkey_json)
    .bind(credential_id)
    .bind(&ceremony.device_label)
    .bind(threshold)
    .bind(requested_at)
    .bind(&body.new_signing_public_key)
    .bind(&ceremony.config_event_hash)
    .bind(&body.signature)
    .bind(request_event_id)
    .execute(&mut *tx)
    .await;
    if let Err(sqlx::Error::Database(db_err)) = &inserted {
        if db_err.is_unique_violation() {
            return Err(AppError::RecoveryAlreadyInProgress);
        }
    }
    inserted?;

    let event = ProtocolEvent {
        id: request_event_id,
        kind: ProtocolEventKindVariant::IdentityRecoveryRequested
            .as_str()
            .to_string(),
        issuer: identity_ref(ceremony.identity_id, "recovery_requested"),
        subject: identity_ref(ceremony.identity_id, "recovery_requested"),
        payload: serde_json::to_value(IdentityRecoveryRequestedPayload {
            request_id,
            threshold,
            new_signing_public_key: body.new_signing_public_key.clone(),
            config_event_hash: ceremony.config_event_hash.clone(),
            signature: body.signature.clone(),
        })
        .expect("IdentityRecoveryRequestedPayload should serialize"),
        timestamp: requested_at,
        version: 1,
        identity_chain: None,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    request_response(&state, request_id).await.map(Json)
}

// ---------------------------------------------------------------------
// Approval, cancellation, finalize, and status.
// ---------------------------------------------------------------------

struct RequestRow {
    identity_id: IdentityId,
    status: String,
    threshold_at_request: i32,
    requested_at: OffsetDateTime,
    new_signing_public_key: String,
    config_event_hash: String,
    request_event_id: Uuid,
}

async fn fetch_request(state: &AppState, request_id: Uuid) -> Result<RequestRow, AppError> {
    let row = sqlx::query(
        "SELECT identity_id, status, threshold_at_request, requested_at, new_signing_public_key, \
                config_event_hash, request_event_id \
         FROM recovery_requests WHERE id = $1",
    )
    .bind(request_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::RecoveryRequestNotFound)?;
    Ok(RequestRow {
        identity_id: row.try_get("identity_id")?,
        status: row.try_get("status")?,
        threshold_at_request: row.try_get("threshold_at_request")?,
        requested_at: row.try_get("requested_at")?,
        new_signing_public_key: row
            .try_get::<Option<String>, _>("new_signing_public_key")?
            .ok_or(AppError::InvalidRecoveryKey)?,
        config_event_hash: row
            .try_get::<Option<String>, _>("config_event_hash")?
            .ok_or(AppError::RecoveryConfigChanged)?,
        request_event_id: row
            .try_get::<Option<Uuid>, _>("request_event_id")?
            .ok_or(AppError::RecoveryRequestNotFound)?,
    })
}

/// Approvals that stand: a withdrawn approval does not count.
async fn approvals_count(state: &AppState, request_id: Uuid) -> Result<i64, AppError> {
    let row = sqlx::query(
        "SELECT COUNT(*) AS count FROM recovery_approvals \
         WHERE request_id = $1 AND withdrawal_signature IS NULL",
    )
    .bind(request_id)
    .fetch_one(&state.pool)
    .await?;
    Ok(row.try_get("count")?)
}

/// The ledger entry hash of the request marker, once the outbox has committed it.
async fn request_entry_hash(
    state: &AppState,
    request_event_id: Uuid,
) -> Result<Option<String>, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT entry_hash FROM ledger_entries WHERE event_id = $1 AND kind = $2",
    )
    .bind(request_event_id)
    .bind(ProtocolEventKindVariant::IdentityRecoveryRequested.as_str())
    .fetch_optional(&state.pool)
    .await?)
}

/// The earliest moment the delay can have run, from the first cosigned head covering the marker.
async fn delay_ends_at(state: &AppState, entry_hash: Option<&str>) -> Option<OffsetDateTime> {
    let (heads, entry_hash) = (state.indexer.head_attestor()?, entry_hash?);
    let mut conn = state.pool.acquire().await.ok()?;
    let entry = heads.by_entry_hash(&mut conn, entry_hash).await.ok()??;
    entry.anchor_at.map(|anchor| anchor + RECOVERY_DELAY)
}

async fn request_response(
    state: &AppState,
    request_id: Uuid,
) -> Result<RecoveryRequestResponse, AppError> {
    let row = fetch_request(state, request_id).await?;
    let approvals_count = approvals_count(state, request_id).await?;
    let request_entry_hash = request_entry_hash(state, row.request_event_id).await?;
    let delay_ends_at = delay_ends_at(state, request_entry_hash.as_deref()).await;
    Ok(RecoveryRequestResponse {
        id: request_id,
        identity_id: row.identity_id,
        status: row.status,
        threshold: row.threshold_at_request,
        approvals_count,
        requested_at: row.requested_at,
        delay_ends_at,
        new_signing_public_key: row.new_signing_public_key,
        config_event_hash: row.config_event_hash,
        request_entry_hash,
    })
}

fn decode_new_key(request: &RequestRow) -> Result<[u8; 32], AppError> {
    BASE64
        .decode(&request.new_signing_public_key)
        .ok()
        .and_then(|raw| raw.try_into().ok())
        .ok_or(AppError::InvalidRecoveryKey)
}

/// The guardian key at its chain position, judged from the guardian's accepted chain; a key not valid
/// there, or a chain not accepted far enough, is an invalid signature.
async fn guardian_key_at(
    state: &AppState,
    guardian: IdentityId,
    signing_key_id: Uuid,
    seq: u64,
    prev_hash: Option<&[u8; 32]>,
) -> Result<[u8; 32], AppError> {
    use avalon_indexer::IndexError;
    let refused = |err: IndexError| match err {
        IndexError::Rejected(_) | IndexError::AwaitingKey(_) => AppError::InvalidAuthorSignature,
        other => other.into(),
    };
    let mut tx = state.pool.begin().await?;
    let chain = avalon_indexer::identity_keys::load(&mut tx, guardian)
        .await
        .map_err(refused)?;
    chain.require_extends(seq, prev_hash).map_err(refused)?;
    let judged = chain.key_at(signing_key_id, seq).map_err(refused)?;
    avalon_indexer::identity_keys::require_valid(judged, "guardian").map_err(refused)
}

fn verifies(key: &[u8; 32], bytes: &[u8], signature_b64: &str) -> bool {
    let Some(signature) = BASE64
        .decode(signature_b64)
        .ok()
        .and_then(|raw| <[u8; 64]>::try_from(raw).ok())
    else {
        return false;
    };
    avalon_protocol::ed25519_key::parse_ed25519_public_key(key).is_some_and(|key| {
        avalon_protocol::ed25519_key::verify_strict_signature(&key, bytes, &signature)
    })
}

/// A guardian's approval: their key, chain position and signature over the request, new key, marker
/// entry and configuration.
#[derive(Deserialize, ToSchema)]
pub struct ApproveRecoveryRequest {
    pub signing_key_id: Uuid,
    /// The guardian's chain position the key is judged at: their chain head `seq` plus one.
    pub seq: u64,
    /// Lowercase hex head event hash of the guardian's chain; absent for an empty chain.
    pub prev_hash: Option<String>,
    pub signature: String,
}

/// `POST /recovery/requests/:id/approve` — a current guardian approves once the marker is in a ledger
/// entry and the request relies on the owner's current configuration; a withdrawn guardian may redo it.
#[utoipa::path(
    post,
    path = "/recovery/requests/{id}/approve",
    tag = "recovery",
    params(("id" = Uuid, Path)),
    request_body = ApproveRecoveryRequest,
    responses((status = 200, description = "The resulting recovery request", body = RecoveryRequestResponse)),
)]
pub async fn approve_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(request_id): Path<Uuid>,
    Json(body): Json<ApproveRecoveryRequest>,
) -> Result<Json<RecoveryRequestResponse>, AppError> {
    let caller = authenticate(&state, &headers).await?;
    let request = fetch_request(&state, request_id).await?;

    if request.status != "pending_approvals" && request.status != "delay" {
        return Err(AppError::RecoveryAlreadyResolved);
    }

    let guardians = current_guardian_set(&state, request.identity_id).await?;
    if !guardians.contains(&caller) {
        return Err(AppError::NotAGuardian);
    }

    let entry_hash = request_entry_hash(&state, request.request_event_id)
        .await?
        .and_then(|h| parse_hex32(&h))
        .ok_or(AppError::RecoveryRequestNotCommitted)?;
    let config_hash =
        parse_hex32(&request.config_event_hash).ok_or(AppError::RecoveryConfigChanged)?;
    if current_config_hash(&state, request.identity_id).await? != Some(config_hash) {
        return Err(AppError::RecoveryConfigChanged);
    }
    let new_key = decode_new_key(&request)?;
    let prev_hash = crate::identity_chain::parse_prev_hash(body.prev_hash.as_deref())?;
    let guardian_key = guardian_key_at(
        &state,
        caller,
        body.signing_key_id,
        body.seq,
        prev_hash.as_ref(),
    )
    .await?;
    let bytes = avalon_protocol::identity_id::recovery_approval_signing_bytes(
        state.chain.network_id(),
        &request.identity_id,
        request_id,
        &caller,
        body.signing_key_id,
        &new_key,
        &avalon_protocol::identity_id::RecoveryApprovalBinding {
            request_entry_hash: &entry_hash,
            config_event_hash: &config_hash,
            guardian_seq: body.seq,
            guardian_prev_hash: prev_hash.as_ref(),
        },
    );
    if !verifies(&guardian_key, &bytes, &body.signature) {
        return Err(AppError::InvalidAuthorSignature);
    }

    let mut tx = state.pool.begin().await?;

    // Re-check status under a row lock: a concurrent cancel must not be overwritten by this approval.
    let locked = sqlx::query(
        "SELECT status, threshold_at_request FROM recovery_requests WHERE id = $1 FOR UPDATE",
    )
    .bind(request_id)
    .fetch_one(&mut *tx)
    .await?;
    let locked_status: String = locked.try_get("status")?;
    let locked_threshold: i32 = locked.try_get("threshold_at_request")?;
    if locked_status != "pending_approvals" && locked_status != "delay" {
        return Err(AppError::RecoveryAlreadyResolved);
    }

    let approved_at = OffsetDateTime::now_utc();
    // A guardian who withdrew may approve again; the new approval replaces the withdrawn one.
    let written = sqlx::query(
        "INSERT INTO recovery_approvals AS a \
         (request_id, guardian_identity_id, approved_at, signing_key_id, signature, \
          guardian_seq, guardian_prev_hash) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (request_id, guardian_identity_id) DO UPDATE SET \
            id = gen_random_uuid(), approved_at = EXCLUDED.approved_at, \
            signing_key_id = EXCLUDED.signing_key_id, signature = EXCLUDED.signature, \
            guardian_seq = EXCLUDED.guardian_seq, guardian_prev_hash = EXCLUDED.guardian_prev_hash, \
            withdrawal_signing_key_id = NULL, withdrawal_seq = NULL, \
            withdrawal_prev_hash = NULL, withdrawal_signature = NULL \
         WHERE a.withdrawal_signature IS NOT NULL",
    )
    .bind(request_id)
    .bind(caller)
    .bind(approved_at)
    .bind(body.signing_key_id)
    .bind(&body.signature)
    .bind(body.seq as i64)
    .bind(&body.prev_hash)
    .execute(&mut *tx)
    .await?;
    if written.rows_affected() == 0 {
        return Err(AppError::AlreadyApproved);
    }

    let count_row = sqlx::query(
        "SELECT COUNT(*) AS count FROM recovery_approvals \
         WHERE request_id = $1 AND withdrawal_signature IS NULL",
    )
    .bind(request_id)
    .fetch_one(&mut *tx)
    .await?;
    let count: i64 = count_row.try_get("count")?;

    if locked_status == "pending_approvals" && meets_threshold(count, locked_threshold) {
        // Guarded by `WHERE status = 'pending_approvals'` as a second layer on top of the row lock.
        sqlx::query(
            "UPDATE recovery_requests SET status = 'delay' WHERE id = $1 AND status = 'pending_approvals'",
        )
        .bind(request_id)
        .execute(&mut *tx)
        .await?;
    }

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::IdentityRecoveryApproved
            .as_str()
            .to_string(),
        issuer: identity_ref(caller, "recovery_approved"),
        subject: identity_ref(request.identity_id, "recovery_approved"),
        payload: serde_json::to_value(IdentityRecoveryApprovedPayload {
            request_id,
            guardian_id: caller,
            approvals_count: count,
            threshold: request.threshold_at_request,
            delay_ends_at: None,
            request_entry_hash: hex::encode(entry_hash),
            config_event_hash: request.config_event_hash.clone(),
            signing_key_id: body.signing_key_id,
            seq: body.seq,
            prev_hash: body.prev_hash.clone(),
            signature: body.signature.clone(),
        })
        .expect("IdentityRecoveryApprovedPayload should serialize"),
        timestamp: approved_at,
        version: 1,
        identity_chain: None,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    request_response(&state, request_id).await.map(Json)
}

/// A guardian's signed withdrawal of their approval.
#[derive(Deserialize, ToSchema)]
pub struct WithdrawApprovalRequest {
    pub signing_key_id: Uuid,
    /// The guardian's chain position the key is judged at: their chain head `seq` plus one.
    pub seq: u64,
    /// Lowercase hex head event hash of the guardian's chain; absent for an empty chain.
    pub prev_hash: Option<String>,
    /// Base64 signature over `recovery_withdrawal_signing_bytes`.
    pub signature: String,
}

/// `POST /recovery/requests/:id/withdraw` — a guardian withdraws their approval; the signed withdrawal
/// rides in the proof and a withdrawn approval does not count.
#[utoipa::path(
    post,
    path = "/recovery/requests/{id}/withdraw",
    tag = "recovery",
    params(("id" = Uuid, Path)),
    request_body = WithdrawApprovalRequest,
    responses((status = 200, description = "The resulting recovery request", body = RecoveryRequestResponse)),
)]
pub async fn withdraw_approval(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(request_id): Path<Uuid>,
    Json(body): Json<WithdrawApprovalRequest>,
) -> Result<Json<RecoveryRequestResponse>, AppError> {
    let caller = authenticate(&state, &headers).await?;
    let request = fetch_request(&state, request_id).await?;
    if request.status != "pending_approvals" && request.status != "delay" {
        return Err(AppError::RecoveryAlreadyResolved);
    }
    let prev_hash = crate::identity_chain::parse_prev_hash(body.prev_hash.as_deref())?;
    let guardian_key = guardian_key_at(
        &state,
        caller,
        body.signing_key_id,
        body.seq,
        prev_hash.as_ref(),
    )
    .await?;
    let bytes = avalon_protocol::identity_id::recovery_withdrawal_signing_bytes(
        state.chain.network_id(),
        &request.identity_id,
        request_id,
        &caller,
        body.signing_key_id,
        body.seq,
        prev_hash.as_ref(),
    );
    if !verifies(&guardian_key, &bytes, &body.signature) {
        return Err(AppError::InvalidAuthorSignature);
    }

    let mut tx = state.pool.begin().await?;
    let locked = sqlx::query(
        "SELECT status, threshold_at_request FROM recovery_requests WHERE id = $1 FOR UPDATE",
    )
    .bind(request_id)
    .fetch_one(&mut *tx)
    .await?;
    let status: String = locked.try_get("status")?;
    let threshold: i32 = locked.try_get("threshold_at_request")?;
    if status != "pending_approvals" && status != "delay" {
        return Err(AppError::RecoveryAlreadyResolved);
    }
    let withdrawn = sqlx::query(
        "UPDATE recovery_approvals SET withdrawal_signing_key_id = $3, withdrawal_seq = $4, \
                withdrawal_prev_hash = $5, withdrawal_signature = $6 \
         WHERE request_id = $1 AND guardian_identity_id = $2 AND withdrawal_signature IS NULL",
    )
    .bind(request_id)
    .bind(caller)
    .bind(body.signing_key_id)
    .bind(body.seq as i64)
    .bind(&body.prev_hash)
    .bind(&body.signature)
    .execute(&mut *tx)
    .await?;
    if withdrawn.rows_affected() == 0 {
        return Err(AppError::NoApprovalToWithdraw);
    }
    let standing: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM recovery_approvals \
         WHERE request_id = $1 AND withdrawal_signature IS NULL",
    )
    .bind(request_id)
    .fetch_one(&mut *tx)
    .await?;
    if status == "delay" && !meets_threshold(standing, threshold) {
        sqlx::query(
            "UPDATE recovery_requests SET status = 'pending_approvals' WHERE id = $1 AND status = 'delay'",
        )
        .bind(request_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    request_response(&state, request_id).await.map(Json)
}

#[derive(Deserialize, ToSchema)]
pub struct CancelRecoveryRequest {
    pub reason: Option<String>,
    /// The owner's signature of the chained `identity.recovery_cancelled` event this authors.
    pub chain_event: crate::identity_chain::ChainEventSignature,
}

/// `POST /recovery/requests/:id/cancel` — the owner's veto: a chained `identity.recovery_cancelled` that
/// invalidates `identity.recovered` for this request. A guardian's recourse is [`withdraw_approval`].
#[utoipa::path(
    post,
    path = "/recovery/requests/{id}/cancel",
    tag = "recovery",
    params(("id" = Uuid, Path)),
    request_body = CancelRecoveryRequest,
    responses((status = 200, description = "The resulting recovery request", body = RecoveryRequestResponse)),
)]
pub async fn cancel_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(request_id): Path<Uuid>,
    Json(body): Json<CancelRecoveryRequest>,
) -> Result<Json<RecoveryRequestResponse>, AppError> {
    let caller = authenticate(&state, &headers).await?;
    let request = fetch_request(&state, request_id).await?;

    if request.status == "completed" || request.status == "cancelled" {
        return Err(AppError::RecoveryAlreadyResolved);
    }
    guard_cancel_authority(caller, request.identity_id)?;

    let mut tx = state.pool.begin().await?;

    let cancelled_at = OffsetDateTime::now_utc();
    let updated = sqlx::query(
        r#"
        UPDATE recovery_requests
        SET status = 'cancelled', cancelled_at = $2, cancelled_by = $3, cancel_reason = $4
        WHERE id = $1 AND status IN ('pending_approvals', 'delay')
        "#,
    )
    .bind(request_id)
    .bind(cancelled_at)
    .bind(caller)
    .bind(&body.reason)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(AppError::RecoveryAlreadyResolved);
    }

    let mut event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::IdentityRecoveryCancelled
            .as_str()
            .to_string(),
        issuer: identity_ref(caller, "recovery_cancelled"),
        subject: identity_ref(request.identity_id, "recovery_cancelled"),
        payload: serde_json::to_value(IdentityRecoveryCancelledPayload {
            request_id,
            cancelled_by: caller,
            reason: body.reason.clone(),
        })
        .expect("IdentityRecoveryCancelledPayload should serialize"),
        timestamp: cancelled_at,
        version: 1,
        identity_chain: None,
    };
    crate::identity_chain::place_signed(&state, &mut tx, &mut event, &body.chain_event, None)
        .await?;
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    request_response(&state, request_id).await.map(Json)
}

/// The `identity.recovered` payload and the ids of the approvals it carries (a withdrawn one with its
/// withdrawal), ordered by guardian id so every party derives the same bytes.
async fn recovered_payload(
    conn: &mut sqlx::PgConnection,
    request_id: Uuid,
) -> Result<(IdentityRecoveredPayload, Vec<Uuid>), AppError> {
    let request = sqlx::query(
        "SELECT pending_device_label, new_signing_public_key, threshold_at_request, \
                config_event_hash, request_signature, request_event_id \
         FROM recovery_requests WHERE id = $1",
    )
    .bind(request_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(AppError::RecoveryRequestNotFound)?;
    let new_signing_public_key: String = request
        .try_get::<Option<String>, _>("new_signing_public_key")?
        .ok_or(AppError::InvalidRecoveryKey)?;
    let request_event_id: Uuid = request
        .try_get::<Option<Uuid>, _>("request_event_id")?
        .ok_or(AppError::RecoveryRequestNotFound)?;
    let request_entry_hash: String = sqlx::query_scalar(
        "SELECT entry_hash FROM ledger_entries WHERE event_id = $1 AND kind = $2",
    )
    .bind(request_event_id)
    .bind(ProtocolEventKindVariant::IdentityRecoveryRequested.as_str())
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(AppError::RecoveryRequestNotCommitted)?;
    let marker = IdentityRecoveryRequestedPayload {
        request_id,
        threshold: request.try_get("threshold_at_request")?,
        new_signing_public_key: new_signing_public_key.clone(),
        config_event_hash: request
            .try_get::<Option<String>, _>("config_event_hash")?
            .ok_or(AppError::RecoveryConfigChanged)?,
        signature: request
            .try_get::<Option<String>, _>("request_signature")?
            .ok_or(AppError::InvalidAuthorSignature)?,
    };
    let rows = sqlx::query(
        "SELECT id, guardian_identity_id, signing_key_id, signature, guardian_seq, \
                guardian_prev_hash, withdrawal_signing_key_id, withdrawal_seq, \
                withdrawal_prev_hash, withdrawal_signature \
         FROM recovery_approvals \
         WHERE request_id = $1 AND signing_key_id IS NOT NULL AND signature IS NOT NULL \
         ORDER BY guardian_identity_id",
    )
    .bind(request_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut approvals = Vec::with_capacity(rows.len());
    let mut approval_ids = Vec::with_capacity(rows.len());
    for row in &rows {
        let withdrawal = match row.try_get::<Option<String>, _>("withdrawal_signature")? {
            Some(signature) => Some(RecoveryWithdrawalProof {
                signing_key_id: row
                    .try_get::<Option<Uuid>, _>("withdrawal_signing_key_id")?
                    .ok_or(AppError::InvalidAuthorSignature)?,
                seq: row
                    .try_get::<Option<i64>, _>("withdrawal_seq")?
                    .ok_or(AppError::InvalidAuthorSignature)? as u64,
                prev_hash: row.try_get("withdrawal_prev_hash")?,
                signature,
            }),
            None => None,
        };
        approval_ids.push(row.try_get("id")?);
        approvals.push(RecoveryApprovalProof {
            guardian_id: row.try_get("guardian_identity_id")?,
            signing_key_id: row.try_get("signing_key_id")?,
            seq: row
                .try_get::<Option<i64>, _>("guardian_seq")?
                .ok_or(AppError::InvalidAuthorSignature)? as u64,
            prev_hash: row.try_get("guardian_prev_hash")?,
            signature: row.try_get("signature")?,
            withdrawal,
        });
    }
    Ok((
        IdentityRecoveredPayload {
            request_id,
            device_label: request.try_get("pending_device_label")?,
            new_signing_public_key,
            request: marker,
            request_entry_hash,
            approvals,
        },
        approval_ids,
    ))
}

/// Lowercase hex SHA-256 of the draft payload's canonical form.
fn draft_hash(payload: &IdentityRecoveredPayload) -> Result<String, AppError> {
    let value = serde_json::to_value(payload).expect("IdentityRecoveredPayload should serialize");
    avalon_protocol::ledger_entry::payload_hash(&value)
        .map(hex::encode)
        .map_err(|_| AppError::InvalidRecoveryKey)
}

/// The `identity.recovered` event the holder of the new key signs to finalize a recovery.
#[derive(Serialize, ToSchema)]
pub struct RecoveredDraft {
    pub kind: String,
    pub version: u32,
    pub issuer: String,
    pub subject: String,
    #[schema(value_type = Object)]
    pub payload: serde_json::Value,
    /// The ids of the approvals the payload carries; finalize takes exactly these.
    pub approval_ids: Vec<Uuid>,
    /// Lowercase hex SHA-256 of the payload's canonical form.
    pub draft_hash: String,
}

/// `GET /recovery/requests/{id}/recovered-draft` — public: exactly the event `finalize` will
/// author, so the holder of the new key can sign it.
#[utoipa::path(
    get,
    path = "/recovery/requests/{id}/recovered-draft",
    tag = "recovery",
    params(("id" = Uuid, Path)),
    responses((status = 200, description = "The event to sign", body = RecoveredDraft)),
)]
pub async fn recovered_draft(
    State(state): State<AppState>,
    Path(request_id): Path<Uuid>,
) -> Result<Json<RecoveredDraft>, AppError> {
    let request = fetch_request(&state, request_id).await?;
    let mut conn = state.pool.acquire().await?;
    let (payload, approval_ids) = recovered_payload(&mut conn, request_id).await?;
    let draft_hash = draft_hash(&payload)?;
    Ok(Json(RecoveredDraft {
        kind: ProtocolEventKindVariant::IdentityRecovered
            .as_str()
            .to_string(),
        version: 1,
        issuer: identity_ref(request.identity_id, "recovered").to_string(),
        subject: identity_ref(request.identity_id, "recovered").to_string(),
        payload: serde_json::to_value(payload).expect("IdentityRecoveredPayload should serialize"),
        approval_ids,
        draft_hash,
    }))
}

/// What finalize takes: the signed event and the approvals its draft carried.
#[derive(Deserialize, ToSchema)]
pub struct FinalizeRecoveryRequest {
    pub chain_event: crate::identity_chain::ChainEventSignature,
    /// The `approval_ids` of the draft that was signed. If the approvals changed since, finalize
    /// answers 409 `RECOVERY_DRAFT_STALE` with the current draft hash.
    pub approval_ids: Vec<Uuid>,
}

/// `POST /recovery/requests/:id/finalize` — public and idempotent. The delay is verified from cosigned
/// heads when the event is applied; until it has run this answers `RECOVERY_NOT_READY_TO_FINALIZE`.
#[utoipa::path(
    post,
    path = "/recovery/requests/{id}/finalize",
    tag = "recovery",
    params(("id" = Uuid, Path)),
    request_body = FinalizeRecoveryRequest,
    responses(
        (status = 200, description = "The resulting recovery request", body = RecoveryRequestResponse),
        (status = 409, description = "RECOVERY_DRAFT_STALE (typed body: draft_hash of the current draft) or RECOVERY_NOT_READY_TO_FINALIZE", body = crate::error::RecoveryDraftStaleBody),
    ),
)]
pub async fn finalize_request(
    State(state): State<AppState>,
    Path(request_id): Path<Uuid>,
    Json(body): Json<FinalizeRecoveryRequest>,
) -> Result<Json<RecoveryRequestResponse>, AppError> {
    let request = fetch_request(&state, request_id).await?;

    if request.status == "completed" {
        return request_response(&state, request_id).await.map(Json);
    }
    guard_can_finalize(&request.status)?;
    if state.indexer.head_attestor().is_none() {
        return Err(AppError::RecoveryNotReadyToFinalize);
    }

    let pending_row = sqlx::query(
        "SELECT pending_passkey_data, pending_credential_id, pending_device_label FROM recovery_requests WHERE id = $1",
    )
    .bind(request_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::RecoveryRequestNotFound)?;
    let pending_passkey_data: serde_json::Value = pending_row.try_get("pending_passkey_data")?;
    let pending_credential_id: Vec<u8> = pending_row.try_get("pending_credential_id")?;
    let pending_device_label: Option<String> = pending_row.try_get("pending_device_label")?;

    let now = OffsetDateTime::now_utc();
    let mut tx = state.pool.begin().await?;

    // Re-check status inside the transaction against a row lock, so a concurrent cancel racing this
    // finalize can't both "win" — whichever commits first is authoritative.
    let locked = sqlx::query("SELECT status FROM recovery_requests WHERE id = $1 FOR UPDATE")
        .bind(request_id)
        .fetch_one(&mut *tx)
        .await?;
    let locked_status: String = locked.try_get("status")?;
    guard_can_finalize(&locked_status)?;

    let (recovered_payload, current_ids) = recovered_payload(&mut tx, request_id).await?;
    let mut signed_ids = body.approval_ids.clone();
    signed_ids.sort();
    let mut current_sorted = current_ids.clone();
    current_sorted.sort();
    if signed_ids != current_sorted {
        return Err(AppError::RecoveryDraftStale {
            draft_hash: draft_hash(&recovered_payload)?,
        });
    }

    // Recovery replaces every credential: the passkeys and signing keys that existed are revoked
    // (`identity.recovered` implies it) and the new passkey and key take their place.
    sqlx::query("DELETE FROM identity_keys WHERE identity_id = $1")
        .bind(request.identity_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE identity_signing_keys SET revoked_at = $2 WHERE identity_id = $1 AND revoked_at IS NULL",
    )
    .bind(request.identity_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    let new_key = decode_new_key(&request)?;
    sqlx::query(
        "INSERT INTO identity_signing_keys (id, identity_id, public_key, label) VALUES ($1, $2, $3, $4)",
    )
    .bind(request_id)
    .bind(request.identity_id)
    .bind(new_key.as_slice())
    .bind(&pending_device_label)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO identity_keys (identity_id, credential_id, passkey_data, label) VALUES ($1, $2, $3, $4)",
    )
    .bind(request.identity_id)
    .bind(&pending_credential_id)
    .bind(&pending_passkey_data)
    .bind(&pending_device_label)
    .execute(&mut *tx)
    .await?;

    // Whoever held a session before recovery is exactly who recovery is meant to lock out.
    crate::sessions::end_all_for_identity(&mut tx, request.identity_id).await?;

    let completed_at = now;
    sqlx::query(
        "UPDATE recovery_requests SET status = 'completed', completed_at = $2 WHERE id = $1",
    )
    .bind(request_id)
    .bind(completed_at)
    .execute(&mut *tx)
    .await?;

    let mut event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::IdentityRecovered
            .as_str()
            .to_string(),
        issuer: identity_ref(request.identity_id, "recovered"),
        subject: identity_ref(request.identity_id, "recovered"),
        payload: serde_json::to_value(&recovered_payload)
            .expect("IdentityRecoveredPayload should serialize"),
        timestamp: completed_at,
        version: 1,
        identity_chain: None,
    };
    crate::identity_chain::place_signed(
        &state,
        &mut tx,
        &mut event,
        &body.chain_event,
        Some(new_key),
    )
    .await?;
    outbox::enqueue(&mut tx, &event).await?;
    // The indexer decides the delay from cosigned heads; until it has run the whole finalize rolls back.
    state
        .indexer
        .apply_in_tx(&mut tx, &event)
        .await
        .map_err(|err| match err {
            avalon_indexer::IndexError::AwaitingKey(_)
            | avalon_indexer::IndexError::Deferred(_) => AppError::RecoveryNotReadyToFinalize,
            other => other.into(),
        })?;

    tx.commit().await?;
    state.indexer.apply_after_commit(&event).await?;

    request_response(&state, request_id).await.map(Json)
}

/// `GET /recovery/requests/:id` — public, deliberately: the ticket's
/// "mandatory *public* time-delay" invariant means the fact of an
/// in-flight recovery, and when its delay ends, must be checkable by
/// anyone, not just the owner or guardians — a public marker on the
/// identity, same alternative #99 itself named. Never exposes which
/// specific guardians have approved, only the count.
#[utoipa::path(
    get,
    path = "/recovery/requests/{id}",
    tag = "recovery",
    params(("id" = Uuid, Path)),
    responses((status = 200, description = "The recovery request", body = RecoveryRequestResponse)),
)]
pub async fn get_request(
    State(state): State<AppState>,
    Path(request_id): Path<Uuid>,
) -> Result<Json<RecoveryRequestResponse>, AppError> {
    request_response(&state, request_id).await.map(Json)
}

/// `GET /identities/:id/recovery/status` — same public data as
/// [`get_request`], but keyed by identity rather than request id, for a
/// caller (the real owner, glancing at their own profile from a device
/// that still has *some* access, or literally anyone else per the "public"
/// requirement above) who doesn't already know a request id. Reports "no
/// active recovery" rather than searching historical/cancelled ones — the
/// at-most-one-active-request index means there is at most one row to
/// find.
#[utoipa::path(
    get,
    path = "/identities/{id}/recovery/status",
    tag = "recovery",
    params(("id" = IdentityId, Path)),
    responses((status = 200, description = "The recovery request, or null if there is none", body = Option<RecoveryRequestResponse>)),
)]
pub async fn identity_recovery_status(
    State(state): State<AppState>,
    IdPath(identity_id): IdPath<IdentityId>,
) -> Result<Json<Option<RecoveryRequestResponse>>, AppError> {
    let row = sqlx::query(
        "SELECT id FROM recovery_requests WHERE identity_id = $1 AND status IN ('pending_approvals', 'delay')",
    )
    .bind(identity_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some(row) = row else {
        return Ok(Json(None));
    };
    let request_id: Uuid = row.try_get("id")?;
    request_response(&state, request_id)
        .await
        .map(|response| Json(Some(response)))
}

/// `GET /me/recovery/status` — the session-authenticated equivalent of
/// [`identity_recovery_status`] for the caller's own identity. Exists
/// alongside the public endpoint specifically so the Hub can surface a
/// prominent "a recovery is in progress against your identity" notice
/// wherever the owner still has *some* working session — reusing the same
/// data shape rather than inventing a separate notification channel, per
/// the ticket's "reuse rather than invent" guidance.
#[utoipa::path(
    get,
    path = "/me/recovery/status",
    tag = "recovery",
    responses((status = 200, description = "The recovery request, or null if there is none", body = Option<RecoveryRequestResponse>)),
)]
pub async fn my_recovery_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Option<RecoveryRequestResponse>>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;
    identity_recovery_status(State(state), IdPath(identity_id)).await
}

#[derive(Serialize, ToSchema)]
pub struct GuardianRequestSummary {
    pub request: RecoveryRequestResponse,
    pub already_approved: bool,
}

/// `GET /me/recovery/guardian-requests` — every active recovery request
/// (across every identity, not just one) where the caller is currently a
/// guardian, for the Hub's guardian-approval UI: "a friend of yours is
/// trying to recover their identity, here's the pending request."
#[utoipa::path(
    get,
    path = "/me/recovery/guardian-requests",
    tag = "recovery",
    responses((status = 200, description = "List of guardian request summary entries", body = Vec<GuardianRequestSummary>)),
)]
pub async fn guardian_requests(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<GuardianRequestSummary>>, AppError> {
    let caller = authenticate(&state, &headers).await?;

    let rows = sqlx::query(
        r#"
        SELECT r.id
        FROM recovery_requests r
        JOIN recovery_guardians g
          ON g.identity_id = r.identity_id AND g.guardian_identity_id = $1
        WHERE r.status IN ('pending_approvals', 'delay')
        ORDER BY r.requested_at
        "#,
    )
    .bind(caller)
    .fetch_all(&state.pool)
    .await?;

    let mut summaries = Vec::with_capacity(rows.len());
    for row in rows {
        let id: Uuid = row.try_get("id")?;
        let already_approved_row = sqlx::query(
            "SELECT 1 FROM recovery_approvals \
             WHERE request_id = $1 AND guardian_identity_id = $2 AND withdrawal_signature IS NULL",
        )
        .bind(id)
        .bind(caller)
        .fetch_optional(&state.pool)
        .await?;
        summaries.push(GuardianRequestSummary {
            request: request_response(&state, id).await?,
            already_approved: already_approved_row.is_some(),
        });
    }

    Ok(Json(summaries))
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here (see crate-level docs) — every
    //! invariant the ticket calls out as security-load-bearing is factored
    //! into a pure function above specifically so it's testable without
    //! one, same convention `passkeys::guard_revoke_last_passkey` and
    //! `devices.rs` already established.

    use super::*;

    #[test]
    fn threshold_cannot_exceed_guardian_count() {
        assert!(matches!(
            validate_guardian_settings(2, 3),
            Err(AppError::InvalidGuardianSet)
        ));
    }

    #[test]
    fn threshold_cannot_be_below_the_protocol_minimum() {
        for threshold in [-1, 0, MIN_RECOVERY_THRESHOLD - 1] {
            assert!(matches!(
                validate_guardian_settings(3, threshold),
                Err(AppError::InvalidGuardianSet)
            ));
        }
    }

    #[test]
    fn zero_guardians_is_rejected() {
        assert!(matches!(
            validate_guardian_settings(0, MIN_RECOVERY_THRESHOLD),
            Err(AppError::InvalidGuardianSet)
        ));
    }

    #[test]
    fn more_than_max_guardians_is_rejected() {
        assert!(matches!(
            validate_guardian_settings(MAX_GUARDIANS + 1, MIN_RECOVERY_THRESHOLD),
            Err(AppError::InvalidGuardianSet)
        ));
    }

    #[test]
    fn a_valid_m_of_n_is_accepted() {
        assert!(validate_guardian_settings(5, 3).is_ok());
        assert!(validate_guardian_settings(2, MIN_RECOVERY_THRESHOLD).is_ok());
        assert!(validate_guardian_settings(MAX_GUARDIANS, MAX_GUARDIANS as i32).is_ok());
    }

    // --- "no set smaller than M can authorize recovery alone" ---

    #[test]
    fn below_threshold_approvals_never_meet_it() {
        assert!(!meets_threshold(0, 2));
        assert!(!meets_threshold(1, 2));
        // A stored threshold below the protocol minimum is raised to it.
        assert!(!meets_threshold(1, 1));
    }

    #[test]
    fn meeting_or_exceeding_threshold_is_recognized() {
        assert!(meets_threshold(2, 2));
        assert!(meets_threshold(3, 2));
    }

    // --- finalize gate (the delay itself is verified from cosigned heads by the indexer) ---

    #[test]
    fn cannot_finalize_while_still_collecting_approvals() {
        assert!(matches!(
            guard_can_finalize("pending_approvals"),
            Err(AppError::RecoveryNotReadyToFinalize)
        ));
    }

    #[test]
    fn a_request_with_its_approvals_may_try_to_finalize() {
        assert!(guard_can_finalize("delay").is_ok());
    }

    #[test]
    fn cannot_finalize_a_cancelled_request() {
        assert!(matches!(
            guard_can_finalize("cancelled"),
            Err(AppError::RecoveryNotReadyToFinalize)
        ));
    }

    #[test]
    fn a_completed_request_cannot_finalize_again() {
        assert!(matches!(
            guard_can_finalize("completed"),
            Err(AppError::RecoveryAlreadyResolved)
        ));
    }

    // --- veto authority ---

    #[test]
    fn the_owner_can_always_cancel_their_own_recovery() {
        let identity_id = IdentityId::random_for_tests();
        assert!(guard_cancel_authority(identity_id, identity_id).is_ok());
    }

    #[test]
    fn a_guardian_or_bystander_cannot_cancel() {
        let identity_id = IdentityId::random_for_tests();
        let other = IdentityId::random_for_tests();
        assert!(matches!(
            guard_cancel_authority(other, identity_id),
            Err(AppError::Forbidden)
        ));
    }

    // --- rate limiting ---

    #[test]
    fn requests_under_the_window_cap_are_allowed() {
        assert!(guard_rate_limit(0).is_ok());
        assert!(guard_rate_limit(MAX_RECOVERY_ATTEMPTS_PER_WINDOW - 1).is_ok());
    }

    #[test]
    fn requests_at_or_over_the_window_cap_are_refused() {
        assert!(matches!(
            guard_rate_limit(MAX_RECOVERY_ATTEMPTS_PER_WINDOW),
            Err(AppError::RecoveryRateLimited)
        ));
        assert!(matches!(
            guard_rate_limit(MAX_RECOVERY_ATTEMPTS_PER_WINDOW + 10),
            Err(AppError::RecoveryRateLimited)
        ));
    }
}
