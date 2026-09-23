//! Social recovery via an M-of-N set of trusted guardians —
//! the answer decided on for losing every registered device at once.
//! See `docs/architecture/identity.md`'s "Social recovery via M-of-N
//! guardians" and "Today in the repo" sections for the full
//! configure/request/approve/finalize state machine and its abuse-
//! resistance measures.

use std::collections::HashSet;

use avalon_protocol::event_payloads::{
    IdentityRecoveredPayload, IdentityRecoveryApprovedPayload, IdentityRecoveryCancelledPayload,
    IdentityRecoveryConfiguredPayload, IdentityRecoveryRequestedPayload,
};
use avalon_protocol::events::{ProtocolEvent, ProtocolEventKindVariant};
use avalon_protocol::ids::GlobalId;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;
use webauthn_rs::prelude::*;

use crate::error::AppError;
use crate::friends::friend_partners;
use crate::handlers::authenticate;
use crate::outbox;
use crate::signature_gate::{canonical_message, require_fresh_signature};
use crate::state::AppState;
use utoipa::ToSchema;

const CEREMONY_TTL_MINUTES: i64 = 5;
const RECOVERY_START_CEREMONY_KIND: &str = "recovery_start";

/// A mandatory *public* time-delay so the real owner — if they still have
/// any access at all, even just visibility via `identity_recovery_status`
/// below — can veto a malicious recovery before it takes effect. 48 hours
/// is the documented default: long enough to plausibly be noticed by an
/// owner who only logs in occasionally, short enough that a genuine
/// all-devices-lost recovery doesn't drag on for a week. Configurable via
/// `AVALON_RECOVERY_DELAY_HOURS` per the ticket, since different
/// deployments may reasonably want a different point in the 24-72h range
/// it suggests.
const DEFAULT_RECOVERY_DELAY_HOURS: i64 = 48;

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

fn recovery_delay_hours_from_env() -> i64 {
    std::env::var("AVALON_RECOVERY_DELAY_HOURS")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|hours| *hours > 0)
        .unwrap_or(DEFAULT_RECOVERY_DELAY_HOURS)
}

fn identity_ref(identity_id: Uuid, verb: &str) -> GlobalId {
    GlobalId::new("identity", &identity_id.to_string(), "self", verb)
}

// ---------------------------------------------------------------------
// Pure guard functions — the actual state-machine invariants, factored
// out so they're unit-testable without a live Postgres, same convention
// `passkeys::guard_revoke_last_passkey` established.
// ---------------------------------------------------------------------

/// A guardian set must have at least one member and at most
/// [`MAX_GUARDIANS`]; the threshold must be satisfiable (`1..=guardian_count`).
/// This is the only thing standing between "no single guardian can
/// authorize recovery alone" being true and being an accident of whatever
/// value a client happened to send.
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
    if threshold < 1 || threshold as usize > guardian_count {
        return Err(AppError::InvalidGuardianSet);
    }
    Ok(())
}

/// Whether a fresh approval count clears the threshold captured at request
/// time. Deliberately `>=`, not `==`: a guardian set can shrink between
/// request and approval (the owner regaining access and tightening it,
/// say), and `threshold_at_request` is frozen at request creation, so a
/// later approval count could in principle jump past it in one step.
pub(crate) fn meets_threshold(approvals_count: i64, threshold_at_request: i32) -> bool {
    approvals_count >= threshold_at_request as i64
}

/// The core "may this request finalize right now" check: it must actually
/// be in the delay phase (not still collecting approvals, not already
/// cancelled or completed), and the delay must have elapsed. Both checks
/// fail closed — a missing `delay_ends_at` on a `delay`-status row should
/// never happen, but is treated as "not ready" rather than panicking or,
/// worse, treated as already elapsed.
pub(crate) fn guard_can_finalize(
    status: &str,
    delay_ends_at: Option<OffsetDateTime>,
    now: OffsetDateTime,
) -> Result<(), AppError> {
    if status == "completed" {
        return Err(AppError::RecoveryAlreadyResolved);
    }
    if status != "delay" {
        return Err(AppError::RecoveryNotReadyToFinalize);
    }
    match delay_ends_at {
        Some(ends_at) if now >= ends_at => Ok(()),
        _ => Err(AppError::RecoveryNotReadyToFinalize),
    }
}

/// Only the identity's own owner or one of its *current* guardians may
/// veto an in-flight attempt — never a bystander, and never a guardian who
/// has since been removed from the set (removing a compromised guardian is
/// exactly the owner's own recourse, via the guardian-set endpoints below,
/// not a reason that former guardian should retain a cancel button).
pub(crate) fn guard_cancel_authority(
    caller: Uuid,
    identity_id: Uuid,
    current_guardians: &HashSet<Uuid>,
) -> Result<(), AppError> {
    if caller == identity_id || current_guardians.contains(&caller) {
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
    pub guardian_ids: Vec<Uuid>,
    pub threshold: i32,
    /// #697/#698: only required when this write *removes* an existing
    /// guardian or *raises* the threshold — see the conditional check in
    /// `set_guardians` below. Naming/adding guardians or lowering the
    /// threshold stays ambient.
    pub signing_key_id: Option<Uuid>,
    pub signature: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct GuardianSettingsResponse {
    pub guardian_ids: Vec<Uuid>,
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
    responses((status = 200, body = GuardianSettingsResponse)),
)]
pub async fn set_guardians(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SetGuardiansRequest>,
) -> Result<Json<GuardianSettingsResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let mut unique_guardians: Vec<Uuid> = body.guardian_ids.clone();
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
    let previous_guardian_ids: HashSet<Uuid> = previous.guardian_ids.iter().copied().collect();
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
                    .map(Uuid::to_string)
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

    let event = ProtocolEvent {
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
    };
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
    responses((status = 200, body = GuardianSettingsResponse)),
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
    identity_id: Uuid,
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
    pub identity_id: Uuid,
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
    responses((status = 200, body = Vec<GuardianOfSummary>)),
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
    params(("identity_id" = Uuid, Path)),
    responses((status = 200, description = "Resigned as guardian")),
)]
pub async fn resign_guardian(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(identity_id): Path<Uuid>,
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
    identity_id: Uuid,
) -> Result<HashSet<Uuid>, AppError> {
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
    identity_id: Uuid,
    device_label: Option<String>,
    webauthn_state: PasskeyRegistration,
}

#[derive(Deserialize, ToSchema)]
pub struct RecoveryStartRequest {
    pub identity_id: Uuid,
    pub device_label: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct RecoveryStartResponse {
    pub ticket_id: Uuid,
    #[schema(value_type = Object)]
    pub challenge: CreationChallengeResponse,
}

async fn recent_request_count(state: &AppState, identity_id: Uuid) -> Result<i64, AppError> {
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
    responses((status = 200, body = RecoveryStartResponse)),
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
    if threshold < 1 {
        return Err(AppError::RecoveryNotAvailable);
    }

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
            body.identity_id,
            &body.identity_id.to_string(),
            &format!("{} (recovery)", body.identity_id),
            Some(exclude_credentials),
        )
        .map_err(|_| AppError::WebauthnFailed)?;

    let ceremony = RecoveryStartCeremonyState {
        identity_id: body.identity_id,
        device_label: body.device_label,
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
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct RecoveryFinishRequest {
    pub ticket_id: Uuid,
    #[schema(value_type = Object)]
    pub webauthn_credential: RegisterPublicKeyCredential,
}

#[derive(Serialize, ToSchema)]
pub struct RecoveryRequestResponse {
    pub id: Uuid,
    pub identity_id: Uuid,
    pub status: String,
    pub threshold: i32,
    pub approvals_count: i64,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub requested_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = "date-time")]
    pub delay_ends_at: Option<OffsetDateTime>,
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
    responses((status = 200, body = RecoveryRequestResponse)),
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
    if threshold < 1 {
        return Err(AppError::RecoveryNotAvailable);
    }

    let recent_count = recent_request_count(&state, ceremony.identity_id).await?;
    guard_rate_limit(recent_count)?;

    let passkey = state
        .webauthn
        .finish_passkey_registration(&body.webauthn_credential, &ceremony.webauthn_state)
        .map_err(|_| AppError::WebauthnFailed)?;
    let passkey_json = serde_json::to_value(&passkey).expect("Passkey should serialize");
    let credential_id: &[u8] = passkey.cred_id().as_ref();

    let mut tx = state.pool.begin().await?;

    let requested_at = OffsetDateTime::now_utc();
    let request_id = Uuid::new_v4();
    let inserted = sqlx::query(
        r#"
        INSERT INTO recovery_requests
            (id, identity_id, pending_passkey_data, pending_credential_id, pending_device_label,
             threshold_at_request, status, requested_at)
        VALUES ($1, $2, $3, $4, $5, $6, 'pending_approvals', $7)
        "#,
    )
    .bind(request_id)
    .bind(ceremony.identity_id)
    .bind(&passkey_json)
    .bind(credential_id)
    .bind(&ceremony.device_label)
    .bind(threshold)
    .bind(requested_at)
    .execute(&mut *tx)
    .await;
    if let Err(sqlx::Error::Database(db_err)) = &inserted {
        if db_err.is_unique_violation() {
            return Err(AppError::RecoveryAlreadyInProgress);
        }
    }
    inserted?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::IdentityRecoveryRequested
            .as_str()
            .to_string(),
        issuer: identity_ref(ceremony.identity_id, "recovery_requested"),
        subject: identity_ref(ceremony.identity_id, "recovery_requested"),
        payload: serde_json::to_value(IdentityRecoveryRequestedPayload {
            request_id,
            threshold,
        })
        .expect("IdentityRecoveryRequestedPayload should serialize"),
        timestamp: requested_at,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(RecoveryRequestResponse {
        id: request_id,
        identity_id: ceremony.identity_id,
        status: "pending_approvals".to_string(),
        threshold,
        approvals_count: 0,
        requested_at,
        delay_ends_at: None,
    }))
}

// ---------------------------------------------------------------------
// Approval, cancellation, finalize, and status.
// ---------------------------------------------------------------------

struct RequestRow {
    identity_id: Uuid,
    status: String,
    threshold_at_request: i32,
    requested_at: OffsetDateTime,
    delay_ends_at: Option<OffsetDateTime>,
}

async fn fetch_request(state: &AppState, request_id: Uuid) -> Result<RequestRow, AppError> {
    let row = sqlx::query(
        "SELECT identity_id, status, threshold_at_request, requested_at, delay_ends_at \
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
        delay_ends_at: row.try_get("delay_ends_at")?,
    })
}

async fn approvals_count(state: &AppState, request_id: Uuid) -> Result<i64, AppError> {
    let row = sqlx::query("SELECT COUNT(*) AS count FROM recovery_approvals WHERE request_id = $1")
        .bind(request_id)
        .fetch_one(&state.pool)
        .await?;
    Ok(row.try_get("count")?)
}

fn to_response(id: Uuid, row: RequestRow, approvals_count: i64) -> RecoveryRequestResponse {
    RecoveryRequestResponse {
        id,
        identity_id: row.identity_id,
        status: row.status,
        threshold: row.threshold_at_request,
        approvals_count,
        requested_at: row.requested_at,
        delay_ends_at: row.delay_ends_at,
    }
}

/// `POST /recovery/requests/:id/approve` — a guardian's independent
/// approval. Requires the caller to currently be one of the identity's
/// guardians (not just at request time — a guardian removed since can no
/// longer approve, mirroring [`guard_cancel_authority`]'s same "current,
/// not historical, guardian" rule). Idempotency: a guardian approving
/// twice hits `recovery_approvals`'s primary key and gets
/// `AppError::AlreadyApproved`, not a double-counted approval. Reaching
/// [`meets_threshold`] against `threshold_at_request` (frozen at request
/// creation, not the identity's possibly-since-changed live threshold)
/// transitions the row into the delay phase in the same transaction as
/// this approval.
#[utoipa::path(
    post,
    path = "/recovery/requests/{id}/approve",
    tag = "recovery",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = RecoveryRequestResponse)),
)]
pub async fn approve_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(request_id): Path<Uuid>,
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

    let mut tx = state.pool.begin().await?;

    // Re-check status inside the transaction against a row lock, the same
    // way `finalize_request` does — the `request.status` read above (before
    // this transaction even opened) is only a cheap early-reject, not the
    // authority for the decision below. Without this lock, a concurrent
    // `cancel_request` could commit a veto between our earlier unlocked
    // read and this point, and this approval would then unconditionally
    // overwrite that veto's `status` back to `delay` — silently undoing an
    // owner's/guardian's cancellation. Whichever of a concurrent cancel or
    // approve commits first is authoritative; the loser sees a status this
    // guard rejects.
    let locked = sqlx::query(
        "SELECT status, delay_ends_at, threshold_at_request FROM recovery_requests WHERE id = $1 FOR UPDATE",
    )
    .bind(request_id)
    .fetch_one(&mut *tx)
    .await?;
    let locked_status: String = locked.try_get("status")?;
    let locked_delay_ends_at: Option<OffsetDateTime> = locked.try_get("delay_ends_at")?;
    let locked_threshold: i32 = locked.try_get("threshold_at_request")?;
    if locked_status != "pending_approvals" && locked_status != "delay" {
        return Err(AppError::RecoveryAlreadyResolved);
    }

    let approved_at = OffsetDateTime::now_utc();
    let inserted = sqlx::query(
        "INSERT INTO recovery_approvals (request_id, guardian_identity_id, approved_at) VALUES ($1, $2, $3)",
    )
    .bind(request_id)
    .bind(caller)
    .bind(approved_at)
    .execute(&mut *tx)
    .await;
    if let Err(sqlx::Error::Database(db_err)) = &inserted {
        if db_err.is_unique_violation() {
            return Err(AppError::AlreadyApproved);
        }
    }
    inserted?;

    let count_row =
        sqlx::query("SELECT COUNT(*) AS count FROM recovery_approvals WHERE request_id = $1")
            .bind(request_id)
            .fetch_one(&mut *tx)
            .await?;
    let count: i64 = count_row.try_get("count")?;

    let mut delay_ends_at = locked_delay_ends_at;
    let mut status = locked_status;
    if status == "pending_approvals" && meets_threshold(count, locked_threshold) {
        status = "delay".to_string();
        delay_ends_at = Some(approved_at + time::Duration::hours(recovery_delay_hours_from_env()));
        // Still guarded by `WHERE status = 'pending_approvals'` as a second,
        // belt-and-suspenders layer on top of the row lock above — this
        // UPDATE can only ever affect the row we're already holding locked,
        // so it can't race, but keeping the guard makes the invariant this
        // statement relies on explicit at the call site, not just upheld by
        // the lock elsewhere in the function.
        sqlx::query(
            "UPDATE recovery_requests SET status = 'delay', delay_ends_at = $2 WHERE id = $1 AND status = 'pending_approvals'",
        )
        .bind(request_id)
        .bind(delay_ends_at)
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
            delay_ends_at,
        })
        .expect("IdentityRecoveryApprovedPayload should serialize"),
        timestamp: approved_at,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(RecoveryRequestResponse {
        id: request_id,
        identity_id: request.identity_id,
        status,
        threshold: request.threshold_at_request,
        approvals_count: count,
        requested_at: request.requested_at,
        delay_ends_at,
    }))
}

#[derive(Deserialize, Default, ToSchema)]
pub struct CancelRecoveryRequest {
    pub reason: Option<String>,
}

/// `POST /recovery/requests/:id/cancel` — the veto path. Either the
/// identity's own owner (any session for `identity_id` itself — including,
/// deliberately, a session opened on a still-trusted device the owner
/// never lost) or any of its *current* guardians may cancel an in-flight
/// attempt they believe is malicious, per [`guard_cancel_authority`]. A
/// request already `completed`/`cancelled` cannot be cancelled again.
#[utoipa::path(
    post,
    path = "/recovery/requests/{id}/cancel",
    tag = "recovery",
    params(("id" = Uuid, Path)),
    request_body = CancelRecoveryRequest,
    responses((status = 200, body = RecoveryRequestResponse)),
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

    let guardians = current_guardian_set(&state, request.identity_id).await?;
    guard_cancel_authority(caller, request.identity_id, &guardians)?;

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

    let event = ProtocolEvent {
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
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    let approvals = approvals_count(&state, request_id).await?;
    Ok(Json(RecoveryRequestResponse {
        id: request_id,
        identity_id: request.identity_id,
        status: "cancelled".to_string(),
        threshold: request.threshold_at_request,
        approvals_count: approvals,
        requested_at: request.requested_at,
        delay_ends_at: request.delay_ends_at,
    }))
}

/// `POST /recovery/requests/:id/finalize` — deliberately public and
/// idempotent (see module docs): it grants nothing beyond what
/// `approve_request`/the elapsed delay already durably authorized, so
/// there is no meaningful caller identity to check. Calling it on an
/// already-`completed` request simply returns the current (already
/// finalized) state rather than erroring, so a client that finalizes
/// eagerly and loses the race to another caller (or to a future
/// auto-finalize sweep, not built this pass — see PR description) doesn't
/// need special-case handling.
#[utoipa::path(
    post,
    path = "/recovery/requests/{id}/finalize",
    tag = "recovery",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = RecoveryRequestResponse)),
)]
pub async fn finalize_request(
    State(state): State<AppState>,
    Path(request_id): Path<Uuid>,
) -> Result<Json<RecoveryRequestResponse>, AppError> {
    let request = fetch_request(&state, request_id).await?;

    if request.status == "completed" {
        let approvals = approvals_count(&state, request_id).await?;
        return Ok(Json(to_response(request_id, request, approvals)));
    }

    let now = OffsetDateTime::now_utc();
    guard_can_finalize(&request.status, request.delay_ends_at, now)?;

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

    let mut tx = state.pool.begin().await?;

    // Re-check status/delay inside the transaction against a row lock, so
    // a concurrent cancel racing this finalize can't both "win" — whichever
    // commits first is authoritative, and the loser's UPDATE below simply
    // affects zero rows.
    let locked =
        sqlx::query("SELECT status, delay_ends_at FROM recovery_requests WHERE id = $1 FOR UPDATE")
            .bind(request_id)
            .fetch_one(&mut *tx)
            .await?;
    let locked_status: String = locked.try_get("status")?;
    let locked_delay_ends_at: Option<OffsetDateTime> = locked.try_get("delay_ends_at")?;
    guard_can_finalize(&locked_status, locked_delay_ends_at, now)?;

    sqlx::query(
        "INSERT INTO identity_keys (identity_id, credential_id, passkey_data, label) VALUES ($1, $2, $3, $4)",
    )
    .bind(request.identity_id)
    .bind(&pending_credential_id)
    .bind(&pending_passkey_data)
    .bind(&pending_device_label)
    .execute(&mut *tx)
    .await?;

    let completed_at = now;
    sqlx::query(
        "UPDATE recovery_requests SET status = 'completed', completed_at = $2 WHERE id = $1",
    )
    .bind(request_id)
    .bind(completed_at)
    .execute(&mut *tx)
    .await?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::IdentityRecovered
            .as_str()
            .to_string(),
        issuer: identity_ref(request.identity_id, "recovered"),
        subject: identity_ref(request.identity_id, "recovered"),
        payload: serde_json::to_value(IdentityRecoveredPayload {
            request_id,
            device_label: pending_device_label.clone(),
        })
        .expect("IdentityRecoveredPayload should serialize"),
        timestamp: completed_at,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    let approvals = approvals_count(&state, request_id).await?;
    Ok(Json(RecoveryRequestResponse {
        id: request_id,
        identity_id: request.identity_id,
        status: "completed".to_string(),
        threshold: request.threshold_at_request,
        approvals_count: approvals,
        requested_at: request.requested_at,
        delay_ends_at: request.delay_ends_at,
    }))
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
    responses((status = 200, body = RecoveryRequestResponse)),
)]
pub async fn get_request(
    State(state): State<AppState>,
    Path(request_id): Path<Uuid>,
) -> Result<Json<RecoveryRequestResponse>, AppError> {
    let request = fetch_request(&state, request_id).await?;
    let approvals = approvals_count(&state, request_id).await?;
    Ok(Json(to_response(request_id, request, approvals)))
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
    params(("id" = Uuid, Path)),
    responses((status = 200, body = Option<RecoveryRequestResponse>)),
)]
pub async fn identity_recovery_status(
    State(state): State<AppState>,
    Path(identity_id): Path<Uuid>,
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
    let request = fetch_request(&state, request_id).await?;
    let approvals = approvals_count(&state, request_id).await?;
    Ok(Json(Some(to_response(request_id, request, approvals))))
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
    responses((status = 200, body = Option<RecoveryRequestResponse>)),
)]
pub async fn my_recovery_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Option<RecoveryRequestResponse>>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;
    identity_recovery_status(State(state), Path(identity_id)).await
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
    responses((status = 200, body = Vec<GuardianRequestSummary>)),
)]
pub async fn guardian_requests(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<GuardianRequestSummary>>, AppError> {
    let caller = authenticate(&state, &headers).await?;

    let rows = sqlx::query(
        r#"
        SELECT r.id, r.identity_id, r.status, r.threshold_at_request, r.requested_at, r.delay_ends_at
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
        let request = RequestRow {
            identity_id: row.try_get("identity_id")?,
            status: row.try_get("status")?,
            threshold_at_request: row.try_get("threshold_at_request")?,
            requested_at: row.try_get("requested_at")?,
            delay_ends_at: row.try_get("delay_ends_at")?,
        };
        let approvals = approvals_count(&state, id).await?;
        let already_approved_row = sqlx::query(
            "SELECT 1 FROM recovery_approvals WHERE request_id = $1 AND guardian_identity_id = $2",
        )
        .bind(id)
        .bind(caller)
        .fetch_optional(&state.pool)
        .await?;
        summaries.push(GuardianRequestSummary {
            request: to_response(id, request, approvals),
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
    fn threshold_cannot_be_zero() {
        assert!(matches!(
            validate_guardian_settings(3, 0),
            Err(AppError::InvalidGuardianSet)
        ));
    }

    #[test]
    fn zero_guardians_is_rejected() {
        assert!(matches!(
            validate_guardian_settings(0, 1),
            Err(AppError::InvalidGuardianSet)
        ));
    }

    #[test]
    fn more_than_max_guardians_is_rejected() {
        assert!(matches!(
            validate_guardian_settings(MAX_GUARDIANS + 1, 1),
            Err(AppError::InvalidGuardianSet)
        ));
    }

    #[test]
    fn a_valid_m_of_n_is_accepted() {
        assert!(validate_guardian_settings(5, 3).is_ok());
        assert!(validate_guardian_settings(1, 1).is_ok());
        assert!(validate_guardian_settings(MAX_GUARDIANS, MAX_GUARDIANS as i32).is_ok());
    }

    // --- "no set smaller than M can authorize recovery alone" ---

    #[test]
    fn below_threshold_approvals_never_meet_it() {
        assert!(!meets_threshold(0, 2));
        assert!(!meets_threshold(1, 2));
    }

    #[test]
    fn meeting_or_exceeding_threshold_is_recognized() {
        assert!(meets_threshold(2, 2));
        assert!(meets_threshold(3, 2));
    }

    // --- time-delay enforcement ---

    #[test]
    fn cannot_finalize_while_still_collecting_approvals() {
        let now = OffsetDateTime::now_utc();
        assert!(matches!(
            guard_can_finalize("pending_approvals", None, now),
            Err(AppError::RecoveryNotReadyToFinalize)
        ));
    }

    #[test]
    fn cannot_finalize_before_the_delay_elapses() {
        let now = OffsetDateTime::now_utc();
        let delay_ends_at = now + time::Duration::hours(1);
        assert!(matches!(
            guard_can_finalize("delay", Some(delay_ends_at), now),
            Err(AppError::RecoveryNotReadyToFinalize)
        ));
    }

    #[test]
    fn can_finalize_once_the_delay_has_elapsed() {
        let now = OffsetDateTime::now_utc();
        let delay_ends_at = now - time::Duration::seconds(1);
        assert!(guard_can_finalize("delay", Some(delay_ends_at), now).is_ok());
    }

    #[test]
    fn cannot_finalize_a_cancelled_request_even_past_its_delay() {
        let now = OffsetDateTime::now_utc();
        let delay_ends_at = now - time::Duration::hours(1);
        assert!(matches!(
            guard_can_finalize("cancelled", Some(delay_ends_at), now),
            Err(AppError::RecoveryNotReadyToFinalize)
        ));
    }

    #[test]
    fn a_completed_request_cannot_finalize_again() {
        let now = OffsetDateTime::now_utc();
        assert!(matches!(
            guard_can_finalize("completed", None, now),
            Err(AppError::RecoveryAlreadyResolved)
        ));
    }

    // --- veto authority ---

    #[test]
    fn the_owner_can_always_cancel_their_own_recovery() {
        let identity_id = Uuid::new_v4();
        let guardians = HashSet::new();
        assert!(guard_cancel_authority(identity_id, identity_id, &guardians).is_ok());
    }

    #[test]
    fn a_current_guardian_can_cancel() {
        let identity_id = Uuid::new_v4();
        let guardian = Uuid::new_v4();
        let mut guardians = HashSet::new();
        guardians.insert(guardian);
        assert!(guard_cancel_authority(guardian, identity_id, &guardians).is_ok());
    }

    #[test]
    fn a_bystander_cannot_cancel() {
        let identity_id = Uuid::new_v4();
        let bystander = Uuid::new_v4();
        let guardians = HashSet::new();
        assert!(matches!(
            guard_cancel_authority(bystander, identity_id, &guardians),
            Err(AppError::Forbidden)
        ));
    }

    #[test]
    fn a_removed_former_guardian_cannot_cancel() {
        let identity_id = Uuid::new_v4();
        let former_guardian = Uuid::new_v4();
        // Not in the *current* set — simulates having been removed since
        // the recovery started.
        let guardians = HashSet::new();
        assert!(matches!(
            guard_cancel_authority(former_guardian, identity_id, &guardians),
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

    #[test]
    fn recovery_delay_env_var_overrides_default() {
        // SAFETY-of-intent note: `std::env::set_var` is process-global;
        // this test does not run concurrently with anything else reading
        // this exact var (no other test in this crate touches
        // `AVALON_RECOVERY_DELAY_HOURS`), so it's safe here despite being
        // `unsafe` in edition-2024 terms.
        unsafe {
            std::env::set_var("AVALON_RECOVERY_DELAY_HOURS", "72");
        }
        assert_eq!(recovery_delay_hours_from_env(), 72);
        unsafe {
            std::env::remove_var("AVALON_RECOVERY_DELAY_HOURS");
        }
        assert_eq!(
            recovery_delay_hours_from_env(),
            DEFAULT_RECOVERY_DELAY_HOURS
        );
    }

    #[test]
    fn a_non_positive_recovery_delay_env_var_falls_back_to_default() {
        unsafe {
            std::env::set_var("AVALON_RECOVERY_DELAY_HOURS", "0");
        }
        assert_eq!(
            recovery_delay_hours_from_env(),
            DEFAULT_RECOVERY_DELAY_HOURS
        );
        unsafe {
            std::env::remove_var("AVALON_RECOVERY_DELAY_HOURS");
        }
    }
}
