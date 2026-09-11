//! Attestation reads (issue #33, implementing ADR #76's "authentic,
//! valid, recognized are three separate questions" model). `GET
//! /attestations/{id}` is a public, unauthenticated read — same visibility
//! level `games::get_game`/`achievements::list_achievement_definitions`
//! already use — returning an attestation with its computed authenticity
//! ([`avalon_chain::attestations::verify_authenticity`]) and validity
//! (`avalon_protocol::achievements::validity`).
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

use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use avalon_chain::attestations::{verify_authenticity, Authenticity};
use avalon_protocol::achievements::validity as compute_validity;

use crate::error::AppError;
use crate::games::{fetch_game_category, fetch_game_status, fetch_issuer_keys};
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

/// One entry in an attestation's history — today, only ever "issued",
/// since no revocation/supersession mechanism exists yet (#85). A forward
/// -compatible shape: once #85 lands, its entries append here rather than
/// requiring a new response field.
#[derive(Serialize)]
pub struct AttestationHistoryEntry {
    pub event: String,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
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
    let validity = compute_validity(status);

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
        history: vec![AttestationHistoryEntry {
            event: "issued".to_string(),
            at: issued_at,
        }],
    }))
}
