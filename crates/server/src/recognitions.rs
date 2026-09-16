//! Public recognition relationships (issue #89) — one integrator declaring
//! "I recognize `<other integrator>`'s claims, for `<scope>`" as a durable,
//! queryable fact. See `docs/architecture/registry.md`'s "Recognition
//! relationships and the network graph" section: this is a graph in its
//! own right ("who does A recognize" / "who recognizes A"), never a single
//! boolean or score on either integrator's own row.
//!
//! **Auth** — the same `authenticate_owning_integrator` pattern
//! `integrator_schemas.rs`/`achievements.rs` already established:
//! declaring a recognition is something an integrator does about its own
//! policy, not something that touches user data, so it needs nothing
//! beyond the integrator proving its own identity
//! (`integrators::authenticate_integrator`'s challenge-response scheme),
//! never a user-granted capability.
//!
//! **Upserted, not append-only.** Republishing a recognition of the same
//! target updates `scope`/`published_at` in place and clears
//! `revoked_at`; revoking sets `revoked_at` without deleting the row — "A
//! used to recognize B, then stopped" stays visible, never silently
//! erased.
//!
//! **Reads** are public and unauthenticated, same posture every other
//! registry-adjacent read in this crate takes: aggregates/facts about
//! integrators, never per-player data.

use avalon_protocol::event_payloads::{
    IntegratorRecognitionPublishedPayload, IntegratorRecognitionRevokedPayload,
};
use avalon_protocol::events::{ProtocolEvent, ProtocolEventKindVariant};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::AppError;
use crate::integrators::{authenticate_integrator, fetch_integrator_id_by_slug, integrator_ref};
use crate::outbox;
use crate::state::AppState;

async fn authenticate_owning_integrator(
    state: &AppState,
    headers: &HeaderMap,
    slug: &str,
) -> Result<Uuid, AppError> {
    let path_integrator_id = fetch_integrator_id_by_slug(state, slug).await?;
    let caller_integrator_id = authenticate_integrator(state, headers).await?;
    if caller_integrator_id != path_integrator_id {
        return Err(AppError::IntegratorRecognitionForbidden);
    }
    Ok(path_integrator_id)
}

async fn fetch_slug_by_id(state: &AppState, id: Uuid) -> Result<String, AppError> {
    let row = sqlx::query("SELECT slug FROM integrators WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::IntegratorNotFound)?;
    row.try_get("slug").map_err(AppError::from)
}

#[derive(Deserialize)]
pub struct PublishRecognitionRequest {
    pub recognized_slug: String,
    pub scope: Vec<String>,
}

#[derive(Serialize)]
pub struct RecognitionResponse {
    pub recognizer_slug: String,
    pub recognized_slug: String,
    pub scope: Vec<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub published_at: OffsetDateTime,
}

/// `POST /integrations/{slug}/recognitions` — publishes (or updates)
/// `slug`'s recognition of `recognized_slug`. Always the caller's own
/// declared policy about itself; never anything read from or written
/// about the target beyond this one directional fact.
pub async fn publish_recognition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(body): Json<PublishRecognitionRequest>,
) -> Result<Json<RecognitionResponse>, AppError> {
    let recognizer_id = authenticate_owning_integrator(&state, &headers, &slug).await?;
    let recognized_id = fetch_integrator_id_by_slug(&state, &body.recognized_slug).await?;
    if recognized_id == recognizer_id {
        return Err(AppError::InvalidRecognitionScope);
    }
    if body.scope.is_empty() || body.scope.iter().any(|s| s.trim().is_empty()) {
        return Err(AppError::InvalidRecognitionScope);
    }

    let now = OffsetDateTime::now_utc();
    let mut tx = state.pool.begin().await?;

    sqlx::query(
        "INSERT INTO integrator_recognitions (recognizer_id, recognized_id, scope, published_at, revoked_at) \
         VALUES ($1, $2, $3, $4, NULL) \
         ON CONFLICT (recognizer_id, recognized_id) DO UPDATE SET \
             scope = EXCLUDED.scope, published_at = EXCLUDED.published_at, revoked_at = NULL",
    )
    .bind(recognizer_id)
    .bind(recognized_id)
    .bind(&body.scope)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::IntegratorRecognitionPublished
            .as_str()
            .to_string(),
        issuer: integrator_ref(&slug, "recognition_published"),
        subject: integrator_ref(&body.recognized_slug, "recognized"),
        payload: serde_json::to_value(IntegratorRecognitionPublishedPayload {
            recognizer_id,
            recognized_id,
            scope: body.scope.clone(),
        })
        .expect("IntegratorRecognitionPublishedPayload should serialize"),
        timestamp: now,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;
    state.indexer.apply_in_tx(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(RecognitionResponse {
        recognizer_slug: slug,
        recognized_slug: body.recognized_slug,
        scope: body.scope,
        published_at: now,
    }))
}

#[derive(Deserialize)]
pub struct RevokeRecognitionRequest {
    pub recognized_slug: String,
}

/// `POST /integrations/{slug}/recognitions/revoke` — marks `slug`'s
/// recognition of `recognized_slug` revoked (`revoked_at` set, row kept).
/// A no-op, not an error, if no recognition was ever published.
pub async fn revoke_recognition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(body): Json<RevokeRecognitionRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let recognizer_id = authenticate_owning_integrator(&state, &headers, &slug).await?;
    let recognized_id = fetch_integrator_id_by_slug(&state, &body.recognized_slug).await?;

    let now = OffsetDateTime::now_utc();
    let mut tx = state.pool.begin().await?;

    let updated = sqlx::query(
        "UPDATE integrator_recognitions SET revoked_at = $3 \
         WHERE recognizer_id = $1 AND recognized_id = $2 AND revoked_at IS NULL",
    )
    .bind(recognizer_id)
    .bind(recognized_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    if updated.rows_affected() > 0 {
        let event = ProtocolEvent {
            id: Uuid::new_v4(),
            kind: ProtocolEventKindVariant::IntegratorRecognitionRevoked
                .as_str()
                .to_string(),
            issuer: integrator_ref(&slug, "recognition_revoked"),
            subject: integrator_ref(&body.recognized_slug, "recognized"),
            payload: serde_json::to_value(IntegratorRecognitionRevokedPayload {
                recognizer_id,
                recognized_id,
            })
            .expect("IntegratorRecognitionRevokedPayload should serialize"),
            timestamp: now,
            version: 1,
        };
        outbox::enqueue(&mut tx, &event).await?;
        state.indexer.apply_in_tx(&mut tx, &event).await?;
    }

    tx.commit().await?;
    Ok(Json(
        serde_json::json!({ "revoked": updated.rows_affected() > 0 }),
    ))
}

/// `GET /integrations/{slug}/recognitions` — every integrator `slug`
/// currently, actively recognizes. Public, unauthenticated.
pub async fn list_recognitions(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<Vec<RecognitionResponse>>, AppError> {
    let integrator_id = fetch_integrator_id_by_slug(&state, &slug).await?;
    let rows = avalon_indexer::projections::integrator_recognitions::list_recognized_by(
        &state.pool,
        integrator_id,
    )
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(RecognitionResponse {
            recognizer_slug: slug.clone(),
            recognized_slug: fetch_slug_by_id(&state, row.recognized_id).await?,
            scope: row.scope,
            published_at: row.published_at,
        });
    }
    Ok(Json(out))
}

/// `GET /integrations/{slug}/recognized-by` — every integrator that
/// currently, actively recognizes `slug`. Public, unauthenticated.
pub async fn list_recognized_by(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<Vec<RecognitionResponse>>, AppError> {
    let integrator_id = fetch_integrator_id_by_slug(&state, &slug).await?;
    let rows = avalon_indexer::projections::integrator_recognitions::list_recognizers_of(
        &state.pool,
        integrator_id,
    )
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(RecognitionResponse {
            recognizer_slug: fetch_slug_by_id(&state, row.recognizer_id).await?,
            recognized_slug: slug.clone(),
            scope: row.scope,
            published_at: row.published_at,
        });
    }
    Ok(Json(out))
}
