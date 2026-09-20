//! Integrator Space instance-data publication and read (issue #384, implementing
//! #381's decided policy on top of #255's schema publication). Two halves:
//!
//! **Write** (`POST /integrations/{slug}/schemas/{version}/data`): the schema's
//! own publishing integrator — proven the same way `integrator_schemas::publish_schema_version`
//! proves it (`authenticate_owning_integrator`, so `{slug}` names the caller,
//! never a request body field) — submits a JSON `instance` for a `subject`
//! identity. Two more checks beyond that self-authentication, mirroring
//! `achievements::issue_attestation`'s "the user's own consent: an
//! active binding to this issuer" pattern: `subject` must have an active
//! binding to the calling integrator (`authz::has_active_binding`), and the
//! instance must actually conform to the schema's parsed protobuf root
//! message (`crate::proto_schema::validate_instance_json` — #384's
//! amendment). Append-only, `superseded_by` lineage, matching
//! `integrator_schemas`'s own immutability posture exactly — see this module's
//! `publish_instance`.
//!
//! **Read** (`GET /identities/{id}/integrator-data`): public, unauthenticated
//! (matching `GET /attestations/{id}`'s posture — #381's whole point is
//! that publishing instance data is itself the opt-in), applying the
//! bidirectional visibility rule from `resolve_visible_fields` per
//! instance. Reads go through the indexer projection
//! (`avalon_indexer::projections::integrator_data_instances`), never raw
//! ledger/outbox data.

use std::collections::BTreeMap;

use avalon_indexer::projections::{
    integrator_data_instances, integrator_schemas as indexed_integrator_schemas,
};
use avalon_protocol::event_payloads::{GameDataDeletedPayload, GameDataPublishedPayload};
use avalon_protocol::events::{ProtocolEvent, ProtocolEventKindVariant};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::authz;
use crate::error::AppError;
use crate::integrator_schemas::{fetch_schema_by_id, schema_ref};
use crate::integrators::{
    authenticate_integrator, fetch_integrator_id_by_slug, integrator_ref, issuer_ref,
};
use crate::outbox;
use crate::proto_schema;
use crate::state::AppState;

/// Authenticates the calling integrator and checks it is the one named by
/// `slug` — the exact same guard `integrator_schemas::authenticate_owning_integrator`
/// uses (kept private to that module, so duplicated here rather than
/// exposed only for this one other call site; the logic itself is two
/// lines and must never drift from that module's copy).
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

#[derive(Deserialize)]
pub struct PublishInstanceRequest {
    pub subject: Uuid,
    pub instance: serde_json::Value,
}

#[derive(Serialize)]
pub struct IntegratorDataInstanceResponse {
    pub id: String,
    pub schema_id: String,
    pub integrator_id: Uuid,
    pub subject: Uuid,
    pub instance: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub published_at: OffsetDateTime,
    pub superseded_by: Option<String>,
}

/// `POST /integrations/{slug}/schemas/{version}/data` — publish (or supersede)
/// this integrator's instance data for `subject` against the named schema
/// version. Always an insert, never an update to an existing row (see
/// module doc comment).
pub async fn publish_instance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((slug, version)): Path<(String, u32)>,
    Json(body): Json<PublishInstanceRequest>,
) -> Result<Json<IntegratorDataInstanceResponse>, AppError> {
    // Proves the caller *is* {slug} — combined with resolving the schema
    // by (slug, version) below, this is what makes "only the schema's own
    // publishing integrator can publish instances against it" (#384)
    // structurally true, not just a convention: the schema id used below
    // is derived from the authenticated slug, never taken from the
    // request body.
    let integrator_id = authenticate_owning_integrator(&state, &headers, &slug).await?;

    let schema_id = schema_ref(&slug, version);
    let schema = fetch_schema_by_id(&state, schema_id.as_str())
        .await?
        .ok_or(AppError::IntegratorSchemaNotFound)?;
    // Belt-and-suspenders: `schema_ref(&slug, version)` can only ever
    // resolve to a row this exact integrator owns (the id is namespaced by
    // slug), but check the stored `integrator_id` explicitly anyway rather than
    // relying solely on id construction never drifting from that
    // invariant elsewhere in the codebase.
    if schema.integrator_id != integrator_id {
        return Err(AppError::IntegratorDataSchemaOwnershipMismatch);
    }

    // The user's own consent: an active binding to this issuer
    // (`achievements::issue_attestation`'s pattern, #384's own text) — no
    // specific capability grant beyond that, since #384 doesn't define
    // one for this action.
    if !authz::has_active_binding(&state, body.subject, integrator_id).await? {
        return Err(AppError::Forbidden);
    }

    // Real validation, per #384's amendment: reject cleanly (never panic)
    // on an instance that doesn't conform to the schema's parsed root
    // message — wrong types, unknown fields, missing required fields.
    // Cached by schema id (`proto_schema::parse_root_message_cached`) so
    // repeated instance writes against the same immutable schema version
    // don't re-run the `.proto` parser on every request — only the first
    // write after a process (re)start (or the publish itself, which warms
    // this cache directly) actually parses.
    let root_message =
        proto_schema::parse_root_message_cached(schema_id.as_str(), &schema.proto_source)?;
    proto_schema::validate_instance_json(&root_message, &body.instance)?;

    let mut tx = state.pool.begin().await?;

    // Lock the schema's own row for the duration of this transaction —
    // same race-prevention shape `publish_schema_version` uses for
    // `integrator_schemas` — so two concurrent publishes for the same
    // (schema, subject) pair serialize rather than both reading "no
    // current instance" and racing to insert.
    sqlx::query("SELECT id FROM integrator_schemas WHERE id = $1 FOR UPDATE")
        .bind(schema_id.as_str())
        .fetch_one(&mut *tx)
        .await?;

    let previous_id: Option<String> = sqlx::query(
        "SELECT id FROM integrator_data_instances \
         WHERE schema_id = $1 AND subject = $2 AND superseded_by IS NULL",
    )
    .bind(schema_id.as_str())
    .bind(body.subject)
    .fetch_optional(&mut *tx)
    .await?
    .map(|row| row.try_get("id"))
    .transpose()?;

    let id = Uuid::new_v4();
    let now = OffsetDateTime::now_utc();

    sqlx::query(
        "INSERT INTO integrator_data_instances \
         (id, schema_id, integrator_id, subject, instance, published_at, superseded_by) \
         VALUES ($1, $2, $3, $4, $5, $6, NULL)",
    )
    .bind(id.to_string())
    .bind(schema_id.as_str())
    .bind(integrator_id)
    .bind(body.subject)
    .bind(&body.instance)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    if let Some(previous_id) = &previous_id {
        sqlx::query("UPDATE integrator_data_instances SET superseded_by = $2 WHERE id = $1")
            .bind(previous_id)
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
    }

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::GameDataPublished
            .as_str()
            .to_string(),
        issuer: integrator_ref(&slug, "data_published"),
        subject: issuer_ref(
            "identity",
            &body.subject.to_string(),
            "integrator_data_published",
        ),
        payload: serde_json::to_value(GameDataPublishedPayload {
            id: id.to_string(),
            schema: schema_id.as_str().to_string(),
            game_id: integrator_id,
            subject: body.subject,
            instance: body.instance.clone(),
            supersedes: previous_id.clone(),
        })
        .expect("GameDataPublishedPayload should serialize"),
        timestamp: now,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    // `indexer_integrator_data_instances` is a projection (issue #42), populated
    // by the indexer applying `event` in this same transaction — so the
    // read endpoint below sees this write immediately, matching
    // `integrator_schemas::publish_schema_version`'s own posture.
    state.indexer.apply_in_tx(&mut tx, &event).await?;

    tx.commit().await?;
    state.indexer.apply_after_commit(&event).await?;

    Ok(Json(IntegratorDataInstanceResponse {
        id: id.to_string(),
        schema_id: schema_id.as_str().to_string(),
        integrator_id,
        subject: body.subject,
        instance: body.instance,
        published_at: now,
        superseded_by: None,
    }))
}

#[derive(Deserialize)]
pub struct DeleteInstanceRequest {
    #[serde(default = "default_delete_reason_code")]
    pub reason_code: String,
    #[serde(default)]
    pub reason: Option<String>,
}

fn default_delete_reason_code() -> String {
    "deleted".to_string()
}

/// `DELETE /integrations/{slug}/schemas/{version}/data/{subject}` (#533) —
/// append-only tombstone for the schema's current (non-superseded,
/// non-deleted) instance belonging to `subject`, following
/// `docs/architecture/revocation.md`'s pattern: the original
/// `integrator_data_instances` row's `instance`/`published_at` are never
/// touched, only `deleted_at`/`delete_reason_code`/`delete_reason` are
/// set — the same "add a lifecycle marker, never mutate the substantive
/// content" shape this module already uses for `superseded_by` above. The
/// original `game_data.published` event, and the new `game_data.deleted`
/// event this appends, both stay observable in raw ledger history.
pub async fn delete_instance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((slug, version, subject)): Path<(String, u32, Uuid)>,
    Json(body): Json<DeleteInstanceRequest>,
) -> Result<Json<()>, AppError> {
    let integrator_id = authenticate_owning_integrator(&state, &headers, &slug).await?;

    let schema_id = schema_ref(&slug, version);
    let schema = fetch_schema_by_id(&state, schema_id.as_str())
        .await?
        .ok_or(AppError::IntegratorSchemaNotFound)?;
    if schema.integrator_id != integrator_id {
        return Err(AppError::IntegratorDataSchemaOwnershipMismatch);
    }

    let mut tx = state.pool.begin().await?;

    // Same race-prevention shape `publish_instance` uses: lock the current
    // instance row for the duration of this transaction so a concurrent
    // publish/delete for the same (schema, subject) pair serializes rather
    // than racing.
    let instance_id: Option<String> = sqlx::query(
        "SELECT id FROM integrator_data_instances \
         WHERE schema_id = $1 AND subject = $2 AND superseded_by IS NULL AND deleted_at IS NULL \
         FOR UPDATE",
    )
    .bind(schema_id.as_str())
    .bind(subject)
    .fetch_optional(&mut *tx)
    .await?
    .map(|row| row.try_get("id"))
    .transpose()?;
    let instance_id = instance_id.ok_or(AppError::IntegratorDataInstanceNotFound)?;

    let now = OffsetDateTime::now_utc();

    sqlx::query(
        "UPDATE integrator_data_instances \
         SET deleted_at = $2, delete_reason_code = $3, delete_reason = $4 \
         WHERE id = $1",
    )
    .bind(&instance_id)
    .bind(now)
    .bind(&body.reason_code)
    .bind(&body.reason)
    .execute(&mut *tx)
    .await?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::GameDataDeleted
            .as_str()
            .to_string(),
        issuer: integrator_ref(&slug, "data_deleted"),
        subject: issuer_ref("identity", &subject.to_string(), "integrator_data_deleted"),
        payload: serde_json::to_value(GameDataDeletedPayload {
            instance_id,
            schema: schema_id.as_str().to_string(),
            game_id: integrator_id,
            subject,
            reason_code: body.reason_code,
            reason: body.reason,
        })
        .expect("GameDataDeletedPayload should serialize"),
        timestamp: now,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;
    state.indexer.apply_in_tx(&mut tx, &event).await?;

    tx.commit().await?;
    state.indexer.apply_after_commit(&event).await?;

    Ok(Json(()))
}

/// The entire visibility decision, pure and DB-free — the field-inclusion
/// rule #384/#381 define, applied to one instance's top-level JSON keys
/// against its schema's visibility metadata. Matches
/// `avalon_chain::mirror::detect_equivocation`'s "small pure function,
/// unit-tested directly, no I/O" precedent for this style of logic.
///
/// Only literal top-level JSON key matching against `field_visibility` —
/// no nested-field visibility (documented limitation, #384).
pub(crate) fn resolve_visible_fields(
    instance: &serde_json::Map<String, serde_json::Value>,
    default_visibility: &str,
    field_visibility: &BTreeMap<String, String>,
) -> serde_json::Map<String, serde_json::Value> {
    instance
        .iter()
        .filter(|(field, _)| {
            let overridden = field_visibility.get(*field).map(String::as_str);
            match default_visibility {
                "private" => overridden == Some("public"),
                // "public" (and any unrecognized value — never restrict on
                // an unrecognized default, since "public" is the safe,
                // pre-#384 default every existing schema keeps behaving
                // as) — visible unless explicitly marked private.
                _ => overridden != Some("private"),
            }
        })
        .map(|(field, value)| (field.clone(), value.clone()))
        .collect()
}

#[derive(Serialize)]
pub struct VisibleIntegratorDataInstanceResponse {
    pub schema: String,
    pub integrator_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub published_at: OffsetDateTime,
    /// Only the fields the instance's schema currently makes visible —
    /// see `resolve_visible_fields`.
    pub fields: serde_json::Map<String, serde_json::Value>,
}

/// `GET /identities/{id}/integrator-data` (#384) — public, unauthenticated (see
/// module doc comment). Every current (non-superseded) instance published
/// about `id`, across every integrator/schema, each filtered to only the fields
/// its schema currently makes visible. A schema whose visibility metadata
/// isn't found in the indexer's projection (should not happen for any
/// instance the same projection itself produced) is skipped defensively
/// rather than ever guessing a default — see the loop below.
pub async fn get_identity_integrator_data(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<VisibleIntegratorDataInstanceResponse>>, AppError> {
    // #533: `list_current_for_subject` already excludes deleted instances
    // (`deleted_at IS NULL`), so a deleted character simply stops
    // appearing here — the tombstone event stays in raw history, not this
    // filtered read.
    let instances = integrator_data_instances::list_current_for_subject(&state.pool, id).await?;

    let mut response = Vec::with_capacity(instances.len());
    for row in instances {
        let Some(visibility) =
            indexed_integrator_schemas::get_visibility(&state.pool, &row.schema_id).await?
        else {
            continue;
        };
        let Some(instance_obj) = row.instance.as_object() else {
            continue;
        };
        let field_visibility: BTreeMap<String, String> =
            serde_json::from_value(visibility.field_visibility).unwrap_or_default();
        let fields = resolve_visible_fields(
            instance_obj,
            &visibility.default_visibility,
            &field_visibility,
        );
        response.push(VisibleIntegratorDataInstanceResponse {
            schema: row.schema_id,
            integrator_id: row.integrator_id,
            published_at: row.published_at,
            fields,
        });
    }
    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — pure-logic checks only, exercising
    //! `resolve_visible_fields` directly (this module's own
    //! `mirror::detect_equivocation`-style pure function). Endpoint-level
    //! flows live in `crates/server/tests/integrator_data.rs`, gated `--ignored`.

    use super::*;

    fn instance(pairs: &[(&str, i64)]) -> serde_json::Map<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), serde_json::json!(v)))
            .collect()
    }

    #[test]
    fn public_default_hides_only_the_explicitly_private_field() {
        let inst = instance(&[("level", 5), ("secret_stat", 99)]);
        let mut overrides = BTreeMap::new();
        overrides.insert("secret_stat".to_string(), "private".to_string());
        let visible = resolve_visible_fields(&inst, "public", &overrides);
        assert!(visible.contains_key("level"));
        assert!(!visible.contains_key("secret_stat"));
    }

    #[test]
    fn private_default_shows_only_the_explicitly_public_field() {
        let inst = instance(&[("level", 5), ("guild_tag", 1)]);
        let mut overrides = BTreeMap::new();
        overrides.insert("guild_tag".to_string(), "public".to_string());
        let visible = resolve_visible_fields(&inst, "private", &overrides);
        assert!(!visible.contains_key("level"));
        assert!(visible.contains_key("guild_tag"));
    }

    #[test]
    fn public_default_with_no_overrides_shows_everything() {
        let inst = instance(&[("a", 1), ("b", 2)]);
        let visible = resolve_visible_fields(&inst, "public", &BTreeMap::new());
        assert_eq!(visible.len(), 2);
    }

    #[test]
    fn private_default_with_no_overrides_shows_nothing() {
        let inst = instance(&[("a", 1), ("b", 2)]);
        let visible = resolve_visible_fields(&inst, "private", &BTreeMap::new());
        assert!(visible.is_empty());
    }

    #[test]
    fn an_override_for_a_field_absent_from_the_instance_is_simply_never_seen() {
        let inst = instance(&[("a", 1)]);
        let mut overrides = BTreeMap::new();
        overrides.insert("b".to_string(), "public".to_string());
        let visible = resolve_visible_fields(&inst, "private", &overrides);
        assert!(visible.is_empty());
    }
}
