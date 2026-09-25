//! Integrator Space schema-to-schema mapping model. See
//! `crates/protocol/src/integrator_schema_mappings.rs`'s own module doc
//! comment: a mapping documents a correspondence between two of an
//! integrator's own already-published schema versions — never an
//! execution engine, never interpreted or run by this module.
//!
//! Mirrors `crate::integrator_schemas`'s own publish/list/get shape and
//! owning-integrator auth check almost exactly — the one real difference
//! is that a mapping has no `superseded_by`/lineage concept (see the
//! protocol module doc comment for why) and validates its `from`/`to`
//! schema references instead of parsing `proto_source`.

use std::collections::BTreeMap;

use avalon_protocol::event_payloads::GameSchemaMappingPublishedPayload;
use avalon_protocol::events::{ProtocolEvent, ProtocolEventKindVariant};
use avalon_protocol::ids::GlobalId;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::AppError;
use crate::integrator_schemas::fetch_schema_by_id;
use crate::integrators::{authenticate_integrator, fetch_integrator_id_by_slug, integrator_ref};
use crate::outbox;
use crate::state::AppState;

/// `game:<slug>:schema_mapping:<seq>` — a published mapping's immutable,
/// globally unique id. `pub(crate)` for this module's own tests.
pub(crate) fn mapping_ref(slug: &str, seq: u32) -> GlobalId {
    GlobalId::new("game", slug, "schema_mapping", &seq.to_string())
}

/// Same shape as `integrator_schemas::authenticate_owning_integrator` —
/// duplicated rather than shared since the two modules' forbidden variants
/// are deliberately distinct error codes (a caller debugging a 403 should
/// see which endpoint rejected it).
async fn authenticate_owning_integrator(
    state: &AppState,
    headers: &HeaderMap,
    slug: &str,
) -> Result<Uuid, AppError> {
    let path_integrator_id = fetch_integrator_id_by_slug(state, slug).await?;
    let caller_integrator_id = authenticate_integrator(state, headers).await?;
    if caller_integrator_id != path_integrator_id {
        return Err(AppError::IntegratorSchemaMappingForbidden);
    }
    Ok(path_integrator_id)
}

struct MappingRow {
    id: String,
    from_schema_id: String,
    to_schema_id: String,
    description: String,
    field_correspondence: serde_json::Value,
    published_at: OffsetDateTime,
}

#[derive(Serialize, ToSchema)]
pub struct IntegratorSchemaMappingResponse {
    pub id: String,
    pub integrator_id: Uuid,
    pub from_schema_id: String,
    pub to_schema_id: String,
    pub description: String,
    pub field_correspondence: BTreeMap<String, String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub published_at: OffsetDateTime,
}

fn mapping_response(integrator_id: Uuid, row: MappingRow) -> IntegratorSchemaMappingResponse {
    let field_correspondence: BTreeMap<String, String> =
        serde_json::from_value(row.field_correspondence).unwrap_or_default();
    IntegratorSchemaMappingResponse {
        id: row.id,
        integrator_id,
        from_schema_id: row.from_schema_id,
        to_schema_id: row.to_schema_id,
        description: row.description,
        field_correspondence,
        published_at: row.published_at,
    }
}

#[derive(Deserialize, ToSchema)]
pub struct PublishIntegratorSchemaMappingRequest {
    /// The schema version this mapping maps *from* — must be a real,
    /// already-published version owned by the same integrator publishing
    /// the mapping.
    pub from_schema_id: String,
    /// The schema version this mapping maps *to* — same ownership/
    /// existence requirement as `from_schema_id`.
    pub to_schema_id: String,
    /// Free text documenting whatever `field_correspondence` can't
    /// capture (merges, splits, dropped fields, default values).
    #[serde(default)]
    pub description: String,
    /// A simple old-field -> new-field correspondence map. Never
    /// interpreted or executed — see module doc comment.
    #[serde(default)]
    pub field_correspondence: BTreeMap<String, String>,
}

/// Confirms `schema_id` exists and is owned by `integrator_id` — the same
/// two-part check `crate::integrator_data::publish_instance` already makes
/// against a single schema, applied here to both of a mapping's
/// references.
async fn require_owned_schema(
    state: &AppState,
    integrator_id: Uuid,
    schema_id: &str,
) -> Result<(), AppError> {
    let schema = fetch_schema_by_id(state, schema_id)
        .await?
        .ok_or(AppError::IntegratorSchemaMappingSchemaNotFound)?;
    if schema.integrator_id != integrator_id {
        return Err(AppError::IntegratorSchemaMappingSchemaOwnershipMismatch);
    }
    Ok(())
}

/// `POST /integrations/{slug}/mappings` — publish a mapping between two of
/// this integrator's own schema versions. Always an insert; mappings have
/// no lineage/superseding concept (see module doc comment), so unlike
/// schema publication this never touches an existing row.
#[utoipa::path(
    post,
    path = "/integrations/{slug}/mappings",
    tag = "integrator-space",
    params(("slug" = String, Path)),
    request_body = PublishIntegratorSchemaMappingRequest,
    responses((status = 200, body = IntegratorSchemaMappingResponse)),
)]
pub async fn publish_mapping(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(body): Json<PublishIntegratorSchemaMappingRequest>,
) -> Result<Json<IntegratorSchemaMappingResponse>, AppError> {
    let integrator_id = authenticate_owning_integrator(&state, &headers, &slug).await?;

    if body.from_schema_id == body.to_schema_id {
        return Err(AppError::InvalidIntegratorSchemaMapping);
    }
    require_owned_schema(&state, integrator_id, &body.from_schema_id).await?;
    require_owned_schema(&state, integrator_id, &body.to_schema_id).await?;

    let field_correspondence_json = serde_json::to_value(&body.field_correspondence)
        .expect("BTreeMap<String, String> is always representable as a JSON object");

    let mut tx = state.pool.begin().await?;

    // Same race-avoidance lock `integrator_schemas::publish_schema_version`
    // takes, for the same reason: two concurrent publishes for the same
    // integrator must serialize rather than both computing the same
    // `MAX(seq)` under READ COMMITTED.
    sqlx::query("SELECT id FROM integrators WHERE id = $1 FOR UPDATE")
        .bind(integrator_id)
        .fetch_one(&mut *tx)
        .await?;

    let current_max: Option<i32> = sqlx::query(
        "SELECT MAX(CAST(split_part(id, ':', 4) AS INT)) AS max_seq \
         FROM integrator_schema_mappings WHERE integrator_id = $1",
    )
    .bind(integrator_id)
    .fetch_one(&mut *tx)
    .await?
    .try_get("max_seq")?;
    let new_seq = current_max.map(|v| v as u32 + 1).unwrap_or(1);

    let id = mapping_ref(&slug, new_seq);
    let now = OffsetDateTime::now_utc();

    sqlx::query(
        "INSERT INTO integrator_schema_mappings \
         (id, integrator_id, from_schema_id, to_schema_id, description, \
          field_correspondence, published_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id.as_str())
    .bind(integrator_id)
    .bind(&body.from_schema_id)
    .bind(&body.to_schema_id)
    .bind(&body.description)
    .bind(&field_correspondence_json)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::GameSchemaMappingPublished
            .as_str()
            .to_string(),
        issuer: integrator_ref(&slug, "schema_mapping_published"),
        subject: id.clone(),
        payload: serde_json::to_value(GameSchemaMappingPublishedPayload {
            id: id.as_str().to_string(),
            integrator_id,
            slug: slug.to_string(),
            from_schema_id: body.from_schema_id.clone(),
            to_schema_id: body.to_schema_id.clone(),
            description: body.description.clone(),
            field_correspondence: field_correspondence_json.clone(),
        })
        .expect("GameSchemaMappingPublishedPayload should serialize"),
        timestamp: now,
        version: 1,
        identity_chain: None,
    };
    outbox::enqueue(&mut tx, &event).await?;

    // Same "read model updates commit atomically with the write it derives
    // from" posture `integrator_schemas::publish_schema_version` already
    // established.
    state.indexer.apply_in_tx(&mut tx, &event).await?;

    tx.commit().await?;
    state.indexer.apply_after_commit(&event).await?;

    Ok(Json(mapping_response(
        integrator_id,
        MappingRow {
            id: id.as_str().to_string(),
            from_schema_id: body.from_schema_id,
            to_schema_id: body.to_schema_id,
            description: body.description,
            field_correspondence: field_correspondence_json,
            published_at: now,
        },
    )))
}

/// `GET /integrations/{slug}/mappings` — every published mapping for this
/// integrator, oldest first. Public, unauthenticated, same posture as
/// schema-version listing. Empty for an integrator that has never
/// published one.
#[utoipa::path(
    get,
    path = "/integrations/{slug}/mappings",
    tag = "integrator-space",
    params(("slug" = String, Path)),
    responses((status = 200, body = Vec<IntegratorSchemaMappingResponse>)),
)]
pub async fn list_mappings(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<Vec<IntegratorSchemaMappingResponse>>, AppError> {
    let integrator_id = fetch_integrator_id_by_slug(&state, &slug).await?;

    let rows = sqlx::query(
        "SELECT id, from_schema_id, to_schema_id, description, field_correspondence, published_at \
         FROM integrator_schema_mappings WHERE integrator_id = $1 ORDER BY published_at",
    )
    .bind(integrator_id)
    .fetch_all(&state.pool)
    .await?;

    let mut mappings = Vec::with_capacity(rows.len());
    for row in rows {
        mappings.push(mapping_response(
            integrator_id,
            MappingRow {
                id: row.try_get("id")?,
                from_schema_id: row.try_get("from_schema_id")?,
                to_schema_id: row.try_get("to_schema_id")?,
                description: row.try_get("description")?,
                field_correspondence: row.try_get("field_correspondence")?,
                published_at: row.try_get("published_at")?,
            },
        ));
    }
    Ok(Json(mappings))
}

/// `GET /integrations/{slug}/mappings/{seq}` — one published mapping,
/// verbatim. Public, unauthenticated.
#[utoipa::path(
    get,
    path = "/integrations/{slug}/mappings/{seq}",
    tag = "integrator-space",
    params(("slug" = String, Path), ("seq" = u32, Path)),
    responses((status = 200, body = IntegratorSchemaMappingResponse)),
)]
pub async fn get_mapping(
    State(state): State<AppState>,
    Path((slug, seq)): Path<(String, u32)>,
) -> Result<Json<IntegratorSchemaMappingResponse>, AppError> {
    let integrator_id = fetch_integrator_id_by_slug(&state, &slug).await?;
    let id = mapping_ref(&slug, seq);

    let row = sqlx::query(
        "SELECT id, from_schema_id, to_schema_id, description, field_correspondence, published_at \
         FROM integrator_schema_mappings WHERE integrator_id = $1 AND id = $2",
    )
    .bind(integrator_id)
    .bind(id.as_str())
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::IntegratorSchemaMappingNotFound)?;

    Ok(Json(mapping_response(
        integrator_id,
        MappingRow {
            id: row.try_get("id")?,
            from_schema_id: row.try_get("from_schema_id")?,
            to_schema_id: row.try_get("to_schema_id")?,
            description: row.try_get("description")?,
            field_correspondence: row.try_get("field_correspondence")?,
            published_at: row.try_get("published_at")?,
        },
    )))
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — pure-logic checks only. The
    //! endpoint-level flows (publish, round-trip fetch, ownership
    //! rejection, cross-slug 403) are covered by
    //! `crates/server/tests/integrator_schema_mappings.rs`, gated
    //! `--ignored`.

    use super::*;

    #[test]
    fn mapping_ref_namespaces_by_slug_and_seq() {
        let id = mapping_ref("ashen-realms", 1);
        assert_eq!(id.as_str(), "game:ashen-realms:schema_mapping:1");
    }

    #[test]
    fn two_integrators_publishing_produce_distinct_ids_for_the_same_seq() {
        let a = mapping_ref("ashen-realms", 1);
        let b = mapping_ref("worldzero", 1);
        assert_ne!(a, b);
    }

    #[test]
    fn successive_mappings_for_the_same_integrator_produce_distinct_ids() {
        let first = mapping_ref("ashen-realms", 1);
        let second = mapping_ref("ashen-realms", 2);
        assert_ne!(first, second);
    }
}
