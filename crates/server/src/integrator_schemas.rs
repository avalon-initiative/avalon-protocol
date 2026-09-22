//! Integrator Space schema publication (issue #255, closing the decision
//! made in #181) — an integrator publishing how its own data is
//! structured, versioned and immutable once published. See
//! `docs/architecture/integrator-space.md`'s "Today in the repo" for
//! namespacing, the owning-integrator auth check, and the
//! immutable-rows-plus-`superseded_by`-lineage write model.

use std::collections::BTreeMap;

use avalon_protocol::event_payloads::GameSchemaPublishedPayload;
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
use crate::integrators::{authenticate_integrator, fetch_integrator_id_by_slug, integrator_ref};
use crate::outbox;
use crate::proto_schema;
use crate::state::AppState;

/// `"public"` | `"private"` — the fixed vocabulary both
/// `default_visibility` and each `field_visibility` value are restricted
/// to (#384/#381). Not an enum serialized directly from the request body:
/// kept as validated strings so the stored JSONB and the wire request
/// shape match byte-for-byte, same choice this module already makes for
/// `proto_source`.
fn is_valid_visibility(value: &str) -> bool {
    matches!(value, "public" | "private")
}

/// `game:<slug>:schema:<version>` — a published version's immutable,
/// globally unique id. `pub(crate)` so this module's own tests (and any
/// future caller) can build the same id rather than reimplementing the
/// format.
pub(crate) fn schema_ref(slug: &str, version: u32) -> GlobalId {
    GlobalId::new("game", slug, "schema", &version.to_string())
}

/// Authenticates the calling integrator and checks it is the one named by
/// `slug` — see this module's doc comment for why this exists instead of
/// `crate::authz::require_capability`. Returns the path slug's own
/// `integrator_id` (already resolved, so callers don't do it twice).
async fn authenticate_owning_integrator(
    state: &AppState,
    headers: &HeaderMap,
    slug: &str,
) -> Result<Uuid, AppError> {
    let path_integrator_id = fetch_integrator_id_by_slug(state, slug).await?;
    let caller_integrator_id = authenticate_integrator(state, headers).await?;
    if caller_integrator_id != path_integrator_id {
        return Err(AppError::IntegratorSchemaForbidden);
    }
    Ok(path_integrator_id)
}

struct SchemaVersionRow {
    id: String,
    version: i32,
    proto_source: String,
    published_at: OffsetDateTime,
    superseded_by: Option<String>,
    default_visibility: String,
    field_visibility: serde_json::Value,
}

#[derive(Serialize, ToSchema)]
pub struct IntegratorSchemaVersionResponse {
    pub id: String,
    pub integrator_id: Uuid,
    pub version: u32,
    pub proto_source: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub published_at: OffsetDateTime,
    pub superseded_by: Option<String>,
    pub default_visibility: String,
    pub field_visibility: BTreeMap<String, String>,
}

fn version_response(integrator_id: Uuid, row: SchemaVersionRow) -> IntegratorSchemaVersionResponse {
    let field_visibility: BTreeMap<String, String> =
        serde_json::from_value(row.field_visibility).unwrap_or_default();
    IntegratorSchemaVersionResponse {
        id: row.id,
        integrator_id,
        version: row.version as u32,
        proto_source: row.proto_source,
        published_at: row.published_at,
        superseded_by: row.superseded_by,
        default_visibility: row.default_visibility,
        field_visibility,
    }
}

#[derive(Deserialize, ToSchema)]
pub struct PublishIntegratorSchemaVersionRequest {
    /// Raw `.proto` source text — parsed for real as of #384 (see
    /// `crate::proto_schema`), no longer stored opaque. Must declare
    /// exactly one top-level `message`, which becomes this schema's root
    /// type for both `field_visibility` validation here and instance
    /// validation in `crate::integrator_data`.
    pub proto_source: String,
    /// `"public"` (default) or `"private"` — #381's schema-level opt-out.
    /// Omitted entirely by a pre-#384 publisher, which keeps today's
    /// fully-open behavior.
    #[serde(default = "default_visibility_public")]
    pub default_visibility: String,
    /// Field name -> `"public"`/`"private"`, overriding `default_visibility`
    /// for that field specifically, in either direction (#381). Every key
    /// must name a real field of the parsed root message — see
    /// `proto_schema::validate_field_visibility_keys`.
    #[serde(default)]
    pub field_visibility: BTreeMap<String, String>,
}

fn default_visibility_public() -> String {
    "public".to_string()
}

/// `POST /integrations/{slug}/schemas` — publish the next version. Always an
/// insert, never an update to an existing row (see module doc comment).
#[utoipa::path(
    post,
    path = "/integrations/{slug}/schemas",
    tag = "integrator-space",
    params(("slug" = String, Path)),
    request_body = PublishIntegratorSchemaVersionRequest,
    responses((status = 200, body = IntegratorSchemaVersionResponse)),
)]
pub async fn publish_schema_version(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(body): Json<PublishIntegratorSchemaVersionRequest>,
) -> Result<Json<IntegratorSchemaVersionResponse>, AppError> {
    let integrator_id = authenticate_owning_integrator(&state, &headers, &slug).await?;
    if body.proto_source.trim().is_empty() {
        return Err(AppError::InvalidIntegratorSchema);
    }
    if !is_valid_visibility(&body.default_visibility) {
        return Err(AppError::InvalidProtoSchema {
            detail: format!(
                "default_visibility must be \"public\" or \"private\", got \"{}\"",
                body.default_visibility
            ),
        });
    }
    for value in body.field_visibility.values() {
        if !is_valid_visibility(value) {
            return Err(AppError::InvalidProtoSchema {
                detail: format!(
                    "field_visibility values must be \"public\" or \"private\", got \"{value}\""
                ),
            });
        }
    }

    // Real parsing, per #384's amendment: reject cleanly (never panic) on
    // malformed source, on zero/multiple top-level messages, and on a
    // `field_visibility` key that isn't a real field of the resolved root
    // message.
    let root_message = proto_schema::parse_root_message(&body.proto_source)?;
    proto_schema::validate_field_visibility_keys(&root_message, &body.field_visibility)?;

    let field_visibility_json = serde_json::to_value(&body.field_visibility)
        .expect("BTreeMap<String, String> is always representable as a JSON object");

    let mut tx = state.pool.begin().await?;

    // Lock the integrator's own row for the duration of this transaction so two
    // concurrent publishes for the same integrator serialize instead of racing:
    // without this, both transactions can read the same `MAX(version)`
    // under READ COMMITTED, compute the same `new_version`, and have one
    // lose to the `UNIQUE (integrator_id, version)` constraint with a raw,
    // unhandled `AppError::Database` 500. `FOR UPDATE` makes the second
    // transaction block here until the first commits (or rolls back), at
    // which point it re-reads `MAX(version)` and correctly computes the
    // next one.
    sqlx::query("SELECT id FROM integrators WHERE id = $1 FOR UPDATE")
        .bind(integrator_id)
        .fetch_one(&mut *tx)
        .await?;

    let current_max: Option<i32> = sqlx::query(
        "SELECT MAX(version) AS max_version FROM integrator_schemas WHERE integrator_id = $1",
    )
    .bind(integrator_id)
    .fetch_one(&mut *tx)
    .await?
    .try_get("max_version")?;
    let previous_version = current_max.map(|v| v as u32);
    let new_version = previous_version.map(|v| v + 1).unwrap_or(1);

    let id = schema_ref(&slug, new_version);
    let now = OffsetDateTime::now_utc();
    let previous_id = previous_version.map(|v| schema_ref(&slug, v));

    sqlx::query(
        "INSERT INTO integrator_schemas \
         (id, integrator_id, version, proto_source, published_at, superseded_by, \
          default_visibility, field_visibility) \
         VALUES ($1, $2, $3, $4, $5, NULL, $6, $7)",
    )
    .bind(id.as_str())
    .bind(integrator_id)
    .bind(new_version as i32)
    .bind(&body.proto_source)
    .bind(now)
    .bind(&body.default_visibility)
    .bind(&field_visibility_json)
    .execute(&mut *tx)
    .await?;

    if let Some(previous_id) = &previous_id {
        sqlx::query("UPDATE integrator_schemas SET superseded_by = $2 WHERE id = $1")
            .bind(previous_id.as_str())
            .bind(id.as_str())
            .execute(&mut *tx)
            .await?;
    }

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::GameSchemaPublished
            .as_str()
            .to_string(),
        issuer: integrator_ref(&slug, "schema_published"),
        subject: id.clone(),
        payload: serde_json::to_value(GameSchemaPublishedPayload {
            id: id.as_str().to_string(),
            game_id: integrator_id,
            slug: slug.to_string(),
            version: new_version,
            proto_source: body.proto_source.clone(),
            supersedes: previous_id.as_ref().map(|g| g.as_str().to_string()),
            default_visibility: body.default_visibility.clone(),
            field_visibility: field_visibility_json.clone(),
        })
        .expect("GameSchemaPublishedPayload should serialize"),
        timestamp: now,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    // `indexer_integrator_schemas` is a projection (issue #42): populated by the
    // indexer applying `event` in this same transaction, not by a direct
    // `INSERT` here — same "read model updates commit atomically with the
    // write it derives from" posture `handlers::register_finish` already
    // established. This is what makes `integrator_data`'s visibility-aware read
    // endpoint (#384) see a schema's visibility metadata immediately,
    // rather than only after some separate replay pass.
    state.indexer.apply_in_tx(&mut tx, &event).await?;

    tx.commit().await?;
    state.indexer.apply_after_commit(&event).await?;

    // Warms `proto_schema`'s in-process root-message cache immediately —
    // parsing happens here, once, rather than being deferred to (and
    // repeated across) this schema's first instance-data write.
    proto_schema::cache_root_message(id.as_str(), root_message);

    Ok(Json(version_response(
        integrator_id,
        SchemaVersionRow {
            id: id.as_str().to_string(),
            version: new_version as i32,
            proto_source: body.proto_source,
            published_at: now,
            superseded_by: None,
            default_visibility: body.default_visibility,
            field_visibility: field_visibility_json,
        },
    )))
}

/// `GET /integrations/{slug}/schemas` — every published version for this integrator,
/// oldest first. Public, unauthenticated (see module doc comment). Empty
/// for an integrator that has never published.
#[utoipa::path(
    get,
    path = "/integrations/{slug}/schemas",
    tag = "integrator-space",
    params(("slug" = String, Path)),
    responses((status = 200, body = Vec<IntegratorSchemaVersionResponse>)),
)]
pub async fn list_schema_versions(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<Vec<IntegratorSchemaVersionResponse>>, AppError> {
    let integrator_id = fetch_integrator_id_by_slug(&state, &slug).await?;

    let rows = sqlx::query(
        "SELECT id, version, proto_source, published_at, superseded_by, \
                default_visibility, field_visibility \
         FROM integrator_schemas WHERE integrator_id = $1 ORDER BY version",
    )
    .bind(integrator_id)
    .fetch_all(&state.pool)
    .await?;

    let mut versions = Vec::with_capacity(rows.len());
    for row in rows {
        versions.push(version_response(
            integrator_id,
            SchemaVersionRow {
                id: row.try_get("id")?,
                version: row.try_get("version")?,
                proto_source: row.try_get("proto_source")?,
                published_at: row.try_get("published_at")?,
                superseded_by: row.try_get("superseded_by")?,
                default_visibility: row.try_get("default_visibility")?,
                field_visibility: row.try_get("field_visibility")?,
            },
        ));
    }
    Ok(Json(versions))
}

/// `GET /integrations/{slug}/schemas/{version}` — one published version, verbatim.
/// Public, unauthenticated. This is the endpoint a round-trip fetch of a
/// just-published version calls to prove the stored `proto_source` matches
/// what was submitted exactly.
#[utoipa::path(
    get,
    path = "/integrations/{slug}/schemas/{version}",
    tag = "integrator-space",
    params(("slug" = String, Path), ("version" = u32, Path)),
    responses((status = 200, body = IntegratorSchemaVersionResponse)),
)]
pub async fn get_schema_version(
    State(state): State<AppState>,
    Path((slug, version)): Path<(String, u32)>,
) -> Result<Json<IntegratorSchemaVersionResponse>, AppError> {
    let integrator_id = fetch_integrator_id_by_slug(&state, &slug).await?;

    let row = sqlx::query(
        "SELECT id, version, proto_source, published_at, superseded_by, \
                default_visibility, field_visibility \
         FROM integrator_schemas WHERE integrator_id = $1 AND version = $2",
    )
    .bind(integrator_id)
    .bind(version as i32)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::IntegratorSchemaNotFound)?;

    Ok(Json(version_response(
        integrator_id,
        SchemaVersionRow {
            id: row.try_get("id")?,
            version: row.try_get("version")?,
            proto_source: row.try_get("proto_source")?,
            published_at: row.try_get("published_at")?,
            superseded_by: row.try_get("superseded_by")?,
            default_visibility: row.try_get("default_visibility")?,
            field_visibility: row.try_get("field_visibility")?,
        },
    )))
}

/// Fetches one schema version's owning integrator + `proto_source` directly —
/// used by [`crate::integrator_data::publish_instance`] to check schema
/// ownership and re-parse the root message for instance validation.
/// Visibility metadata for the *read* side comes from the indexer's own
/// projection instead (`avalon_indexer::projections::integrator_schemas::get_visibility`),
/// matching this crate's settlement-vs-querying split. `pub(crate)` rather
/// than duplicating this query.
pub(crate) struct SchemaForInstanceOps {
    pub integrator_id: Uuid,
    pub proto_source: String,
}

pub(crate) async fn fetch_schema_by_id(
    state: &AppState,
    schema_id: &str,
) -> Result<Option<SchemaForInstanceOps>, AppError> {
    let row =
        sqlx::query("SELECT integrator_id, proto_source FROM integrator_schemas WHERE id = $1")
            .bind(schema_id)
            .fetch_optional(&state.pool)
            .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(SchemaForInstanceOps {
        integrator_id: row.try_get("integrator_id")?,
        proto_source: row.try_get("proto_source")?,
    }))
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — pure-logic checks only. The
    //! endpoint-level flows (publish, round-trip fetch, immutability
    //! across a second version, cross-slug 403) are covered by
    //! `crates/server/tests/integrator_schemas.rs`, gated `--ignored`.

    use super::*;

    #[test]
    fn schema_ref_namespaces_by_slug_and_version() {
        let id = schema_ref("ashen-realms", 1);
        assert_eq!(id.as_str(), "game:ashen-realms:schema:1");
    }

    #[test]
    fn two_integrators_publishing_produce_distinct_ids_for_the_same_version_number() {
        let a = schema_ref("ashen-realms", 1);
        let b = schema_ref("worldzero", 1);
        assert_ne!(a, b);
    }

    #[test]
    fn successive_versions_for_the_same_integrator_produce_distinct_ids() {
        let v1 = schema_ref("ashen-realms", 1);
        let v2 = schema_ref("ashen-realms", 2);
        assert_ne!(v1, v2);
    }
}
