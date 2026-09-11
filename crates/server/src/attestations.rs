//! Attestation reads and revocation (issues #33 and #85, implementing ADR
//! #76's "authentic, valid, recognized are three separate questions"
//! model and #81's decided revocation mechanics). `GET
//! /attestations/{id}` is a public, unauthenticated read — same visibility
//! level `games::get_game`/`achievements::list_achievement_definitions`
//! already use — returning an attestation with its computed authenticity
//! ([`avalon_chain::attestations::verify_authenticity`]) and validity
//! (`avalon_protocol::achievements::validity`, now revocation-aware).
//!
//! **Recognition is deliberately absent from this response.** Whether a
//! *specific* consumer recognizes a claim is that consumer's own policy
//! evaluation (`avalon_protocol::achievements::recognize`), never a
//! boolean this endpoint computes — the same claim is `Authentic`/`Valid`
//! for every observer, but "recognized" only makes sense relative to one
//! consumer's own `TrustRelationship`. Publishing/serving a game's own
//! declared recognition policy (the ticket's `PUT
//! /games/{slug}/recognition`) is deferred, not built in this pass — see
//! this module's own tracking note in `docs/architecture/trust-model.md`.
//!
//! **Revocation is a signed, appended entry (#85), never a mutation.**
//! `POST /attestations/{id}/revoke` inserts a new row into the separate
//! `attestation_revocations` table — `achievement_attestations` itself is
//! never touched. Only the original issuer (any of its currently-valid
//! keys authenticates the caller; the embedded signature, checked the same
//! way issuance's is, proves that key specifically authorized *this*
//! revocation) may revoke, matching #81's "never the node operator, never
//! the subject" invariant. Reinstatement and supersession are explicitly
//! not built — see [`avalon_protocol::achievements::AttestationStatus`]'s
//! own doc comment for why.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use avalon_chain::attestations::{verify_authenticity, verify_signature, Authenticity};
use avalon_protocol::achievements::validity as compute_validity;
use avalon_protocol::achievements::{attestation_status_at, revocation_signing_bytes};
use avalon_protocol::ids::AttestationId;

use avalon_protocol::events::ProtocolEvent;

use crate::error::AppError;
use crate::games::{
    authenticate_game, fetch_game_category, fetch_game_status, fetch_issuer_keys, issuer_ref,
};
use crate::outbox;
use crate::state::AppState;

/// `GlobalId` derives `Deserialize` as a transparent newtype over `String`
/// (no public raw-string constructor exists) — same round trip
/// `mirror_watcher.rs::global_id_from_str` already uses for the same
/// reason, here to rebuild the stored `achievement` column back into the
/// type `verify_authenticity` expects.
fn global_id_from_str(raw: &str) -> Option<avalon_protocol::ids::GlobalId> {
    serde_json::from_value(serde_json::Value::String(raw.to_string())).ok()
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AuthenticityResponse {
    Authentic { key_id: String },
    NotAuthentic { reason: String },
}

impl From<Authenticity> for AuthenticityResponse {
    fn from(value: Authenticity) -> Self {
        match value {
            Authenticity::Authentic { key_id } => AuthenticityResponse::Authentic { key_id },
            Authenticity::NotAuthentic { reason } => AuthenticityResponse::NotAuthentic { reason },
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ValidityResponse {
    Valid,
    Invalid { reason: String },
}

impl From<avalon_protocol::achievements::Validity> for ValidityResponse {
    fn from(value: avalon_protocol::achievements::Validity) -> Self {
        match value {
            avalon_protocol::achievements::Validity::Valid => ValidityResponse::Valid,
            avalon_protocol::achievements::Validity::Invalid { reason } => {
                ValidityResponse::Invalid { reason }
            }
        }
    }
}

#[derive(Serialize)]
pub struct AttestationProofResponse {
    pub key_id: String,
    pub algorithm: String,
}

/// One entry in an attestation's history — `"issued"` always, plus
/// `"revoked"` if a revocation entry exists (#85). Reinstatement/
/// supersession entries would append here too, once either exists.
#[derive(Serialize)]
pub struct AttestationHistoryEntry {
    pub event: String,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Serialize)]
pub struct AttestationReadResponse {
    pub id: Uuid,
    pub issuer: String,
    pub subject: Uuid,
    pub achievement: String,
    #[serde(with = "time::serde::rfc3339")]
    pub issued_at: OffsetDateTime,
    pub proof: AttestationProofResponse,
    pub authenticity: AuthenticityResponse,
    pub validity: ValidityResponse,
    pub history: Vec<AttestationHistoryEntry>,
    // Deliberately no `recognition` field — see module doc comment.
}

/// `GET /attestations/{id}` (#33) — public, unauthenticated.
pub async fn get_attestation(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<AttestationReadResponse>, AppError> {
    let row = sqlx::query(
        "SELECT id, game_id, issuer, subject, achievement, issued_at, \
                proof_key_id, proof_algorithm, proof_bytes \
         FROM achievement_attestations WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::AttestationNotFound)?;

    let game_id: Uuid = row.try_get("game_id")?;
    let issuer: String = row.try_get("issuer")?;
    let subject: Uuid = row.try_get("subject")?;
    let achievement: String = row.try_get("achievement")?;
    let issued_at: OffsetDateTime = row.try_get("issued_at")?;
    let proof_key_id: Uuid = row.try_get("proof_key_id")?;
    let proof_algorithm: String = row.try_get("proof_algorithm")?;
    let proof_bytes: Vec<u8> = row.try_get("proof_bytes")?;

    let category = fetch_game_category(&state, game_id).await?;
    let status = fetch_game_status(&state, game_id).await?;
    let issuer_keys = fetch_issuer_keys(&state, game_id).await?;

    let attestation = avalon_protocol::achievements::AchievementAttestation {
        id: avalon_protocol::ids::AttestationId(id),
        issuer: match category {
            avalon_protocol::games::IntegratorCategory::Game => {
                avalon_protocol::achievements::Issuer::Game(avalon_protocol::ids::GameId(game_id))
            }
            avalon_protocol::games::IntegratorCategory::App => {
                avalon_protocol::achievements::Issuer::App(avalon_protocol::ids::GameId(game_id))
            }
            avalon_protocol::games::IntegratorCategory::Service => {
                avalon_protocol::achievements::Issuer::Service(avalon_protocol::ids::GameId(
                    game_id,
                ))
            }
        },
        subject: avalon_protocol::ids::IdentityId(subject),
        achievement: global_id_from_str(&achievement).ok_or(AppError::AttestationNotFound)?,
        issued_at,
        proof: avalon_protocol::achievements::Signature {
            key_id: proof_key_id.to_string(),
            algorithm: proof_algorithm.clone(),
            bytes: proof_bytes,
        },
    };

    let authenticity =
        verify_authenticity(&attestation, category.claim_kind(), &issuer, &issuer_keys);

    let revocation_row = sqlx::query(
        "SELECT revoked_at, reason_code, reason FROM attestation_revocations \
         WHERE attestation_id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    let revoked_at: Option<OffsetDateTime> = match &revocation_row {
        Some(row) => Some(row.try_get("revoked_at")?),
        None => None,
    };
    let now = OffsetDateTime::now_utc();
    let attestation_status = attestation_status_at(revoked_at, now);
    let validity = compute_validity(status, attestation_status);

    let mut history = vec![AttestationHistoryEntry {
        event: "issued".to_string(),
        at: issued_at,
        reason_code: None,
        reason: None,
    }];
    if let Some(row) = revocation_row {
        history.push(AttestationHistoryEntry {
            event: "revoked".to_string(),
            at: row.try_get("revoked_at")?,
            reason_code: Some(row.try_get("reason_code")?),
            reason: Some(row.try_get("reason")?),
        });
    }

    Ok(Json(AttestationReadResponse {
        id,
        issuer,
        subject,
        achievement,
        issued_at,
        proof: AttestationProofResponse {
            key_id: proof_key_id.to_string(),
            algorithm: proof_algorithm,
        },
        authenticity: authenticity.into(),
        validity: validity.into(),
        history,
    }))
}

#[derive(Deserialize)]
pub struct RevokeAttestationRequest {
    pub key_id: Uuid,
    /// Standard-base64-encoded detached Ed25519 signature over
    /// [`revocation_signing_bytes`].
    pub signature: String,
    pub reason_code: String,
    pub reason: String,
}

#[derive(Serialize)]
pub struct RevocationResponse {
    pub attestation_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub revoked_at: OffsetDateTime,
    pub reason_code: String,
    pub reason: String,
}

/// `POST /attestations/{id}/revoke` (#85). Only the attestation's original
/// issuer may revoke it — authenticated via the same challenge-response
/// scheme every issuer-credentialed endpoint uses, plus (like issuance) an
/// independently-checked embedded signature over
/// [`revocation_signing_bytes`], so the revocation record itself carries
/// cryptographic proof of who authorized it, not just an HTTP-layer claim.
pub async fn revoke_attestation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<RevokeAttestationRequest>,
) -> Result<Json<RevocationResponse>, AppError> {
    let row = sqlx::query(
        "SELECT game_id, issuer, achievement FROM achievement_attestations WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::AttestationNotFound)?;
    let game_id: Uuid = row.try_get("game_id")?;
    let issuer: String = row.try_get("issuer")?;

    // Only the attestation's own issuer — never a different issuer, never
    // the node operator, never the subject.
    let caller_game_id = authenticate_game(&state, &headers).await?;
    if caller_game_id != game_id {
        return Err(AppError::AttestationRevocationForbidden);
    }

    let already_revoked =
        sqlx::query("SELECT 1 FROM attestation_revocations WHERE attestation_id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?
            .is_some();
    if already_revoked {
        return Err(AppError::AttestationAlreadyRevoked);
    }

    let category = fetch_game_category(&state, game_id).await?;
    let claim_kind = category.claim_kind();
    let signature_bytes = BASE64
        .decode(&body.signature)
        .map_err(|_| AppError::InvalidAttestationSignature)?;
    let signing_bytes =
        revocation_signing_bytes(claim_kind, &issuer, AttestationId(id), &body.reason_code);

    let issuer_keys = fetch_issuer_keys(&state, game_id).await?;
    let now = OffsetDateTime::now_utc();
    let Authenticity::Authentic { .. } = verify_signature(
        &body.key_id.to_string(),
        &signing_bytes,
        &signature_bytes,
        now,
        &issuer_keys,
    ) else {
        return Err(AppError::InvalidAttestationSignature);
    };

    let revocation_id = Uuid::new_v4();
    let mut tx = state.pool.begin().await?;

    sqlx::query(
        "INSERT INTO attestation_revocations \
         (id, attestation_id, issuer, reason_code, reason, revoked_at, proof_key_id, proof_algorithm, proof_bytes) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(revocation_id)
    .bind(id)
    .bind(&issuer)
    .bind(&body.reason_code)
    .bind(&body.reason)
    .bind(now)
    .bind(body.key_id)
    .bind("ed25519")
    .bind(&signature_bytes)
    .execute(&mut *tx)
    .await?;

    // `issuer` is already the full "<namespace>:<slug>" ref
    // (`revocation_signing_bytes` above needs exactly that shape) — strip
    // the namespace prefix back off before handing the bare slug to
    // `issuer_ref`, which prepends its own namespace argument. Passing
    // `issuer` directly here would double up the namespace
    // (`game:game:<slug>:self:...`), a real bug caught by inspecting the
    // actual ledger entry during manual verification, not by any type
    // system, since both are plain strings.
    let slug = issuer
        .strip_prefix(&format!("{}:", category.as_str()))
        .unwrap_or(&issuer);
    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: format!("{claim_kind}.revoked"),
        issuer: issuer_ref(category.as_str(), slug, &format!("{claim_kind}_revoked")),
        subject: issuer_ref(
            "attestation",
            &id.to_string(),
            &format!("{claim_kind}_revoked"),
        ),
        payload: serde_json::json!({
            "id": revocation_id,
            "attestation_id": id,
            "issuer": issuer,
            "reason_code": body.reason_code,
            "reason": body.reason,
        }),
        timestamp: now,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(RevocationResponse {
        attestation_id: id,
        revoked_at: now,
        reason_code: body.reason_code,
        reason: body.reason,
    }))
}
