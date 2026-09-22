//! Integrator registration and server-to-server integrator authentication (issue #26).
//! See `docs/architecture/issuers.md` and `docs/architecture/registry.md`'s
//! "Today in the repo" sections for registration, key rotation, and listing details.

use std::collections::HashMap;

use avalon_protocol::event_payloads::{
    GameRegisteredKeyPayload, GameRegisteredPayload, IssuerKeyAddedPayload, IssuerKeyRevokedPayload,
};
use avalon_protocol::events::{ProtocolEvent, ProtocolEventKindVariant};
use avalon_protocol::ids::GlobalId;
use avalon_protocol::integrators::{
    IntegratorCategory, IntegratorStatus, IssuerKey, KeyPurpose, KeyRole,
};
use avalon_protocol::permissions::Capability;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, QueryBuilder, Row};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::verify_event_signature;
use crate::error::AppError;
use crate::outbox;
use crate::state::AppState;

const GAME_CHALLENGE_TTL_MINUTES: i64 = 5;
const GAME_CHALLENGE_NONCE_BYTES: usize = 32;

/// Default/maximum page size for `GET /integrations` (issue #270) — same
/// "small default, capped maximum" shape
/// `guilds::DEFAULT_DISCOVER_PAGE_SIZE`/`MAX_DISCOVER_PAGE_SIZE` already use.
const DEFAULT_GAMES_LIST_PAGE_SIZE: i64 = 20;
const MAX_GAMES_LIST_PAGE_SIZE: i64 = 100;

/// Header names accepted for integrator/integrator server-to-server auth.
///
/// `x-avalon-integrator-*` is the original name; `x-avalon-integrator-*` is the
/// generic replacement added by #293 (mirroring #282's `IntegratorCategory`
/// generalization on the server's public API). Both are accepted from any
/// caller indefinitely — see [`header_value`] — but this repo's own
/// outbound code (SDK, CLI) sends only the `integrator` name from now on.
const INTEGRATOR_KEY_ID_HEADER: &str = "x-avalon-integrator-key-id";
const INTEGRATOR_CHALLENGE_ID_HEADER: &str = "x-avalon-integrator-challenge-id";
const INTEGRATOR_SIGNATURE_HEADER: &str = "x-avalon-integrator-signature";

/// Only algorithm `crate::auth::verify_event_signature` (and thus the
/// challenge-response scheme below) can verify. Registration itself rejects
/// anything else up front rather than silently accepting a key it can never
/// later authenticate.
const SUPPORTED_KEY_ALGORITHM: &str = "ed25519";

/// `pub(crate)` so `connections.rs` (#27/#83) can build the same
/// `game:<slug>:self:<verb>` `GlobalId` shape for `game.binding_established`/
/// `game.binding_ended` subjects, rather than reimplementing this format.
pub(crate) fn integrator_ref(slug: &str, verb: &str) -> GlobalId {
    issuer_ref("game", slug, verb)
}

/// Generic form of [`integrator_ref`] — `<namespace>:<slug>:self:<verb>` for any
/// issuer category's namespace (`"game"`/`"app"`/`"service"`, matching
/// `IntegratorCategory::as_str()`). `pub(crate)` so `achievements.rs` (#324)
/// can mint the same shape for `App`/`Service` issuers, not just `Game`.
pub(crate) fn issuer_ref(namespace: &str, slug: &str, verb: &str) -> GlobalId {
    GlobalId::new(namespace, slug, "self", verb)
}

/// The category (`integrator`/`app`/`service`) a registered integrator/app/service was
/// recorded under — what determines its claim vocabulary (#324:
/// `IntegratorCategory::claim_kind`). `pub(crate)` so `achievements.rs` can
/// reject a caller acting under the wrong claim-vocabulary route (an
/// `Issuer::Game` hitting `/integrations/{slug}/milestones`, or vice versa)
/// rather than silently accepting a mismatched label.
pub(crate) async fn fetch_integrator_category(
    state: &AppState,
    integrator_id: Uuid,
) -> Result<IntegratorCategory, AppError> {
    let row = sqlx::query("SELECT category FROM integrators WHERE id = $1")
        .bind(integrator_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::IntegratorNotFound)?;
    let category_raw: String = row.try_get("category")?;
    // The column only ever gets written via `IntegratorCategory::as_str()`
    // (see `register_integrator` above) — an unparseable value here would mean
    // the write side and this read side have drifted, not a real runtime
    // condition to design an error path around. Defaulting to `Game`
    // (rather than panicking a live request) is safe precisely because
    // this can't actually happen in practice.
    Ok(IntegratorCategory::parse(&category_raw).unwrap_or(IntegratorCategory::Game))
}

/// The issuer's current `IntegratorStatus` (issue #33's `Validity` check — see
/// `avalon_protocol::achievements::validity`). Same "defaults to `Active`
/// on an unparseable value rather than panicking a live request" posture
/// as [`fetch_integrator_category`], for the same reason: the column only ever
/// gets written via `IntegratorStatus::as_str()`.
pub(crate) async fn fetch_integrator_status(
    state: &AppState,
    integrator_id: Uuid,
) -> Result<IntegratorStatus, AppError> {
    let row = sqlx::query("SELECT status FROM integrators WHERE id = $1")
        .bind(integrator_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::IntegratorNotFound)?;
    let status_raw: String = row.try_get("status")?;
    Ok(IntegratorStatus::parse(&status_raw).unwrap_or(IntegratorStatus::Active))
}

/// The batched sibling of [`fetch_integrator_category`]/
/// [`fetch_integrator_status`] — one query for every id in `integrator_ids`
/// instead of two queries per id, for a caller (issue #377's
/// `list_my_achievements`) building a response across many attestations at
/// once. An id with no matching row is simply absent from the result map,
/// same "caller decides how to handle a miss" posture
/// [`crate::handlers::list_profiles`] already takes for its own batch read.
pub(crate) async fn fetch_integrator_category_and_status_batch(
    state: &AppState,
    integrator_ids: &[Uuid],
) -> Result<HashMap<Uuid, (IntegratorCategory, IntegratorStatus)>, AppError> {
    if integrator_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query("SELECT id, category, status FROM integrators WHERE id = ANY($1)")
        .bind(integrator_ids)
        .fetch_all(&state.pool)
        .await?;

    let mut result = HashMap::with_capacity(rows.len());
    for row in rows {
        let id: Uuid = row.try_get("id")?;
        let category_raw: String = row.try_get("category")?;
        let status_raw: String = row.try_get("status")?;
        result.insert(
            id,
            (
                IntegratorCategory::parse(&category_raw).unwrap_or(IntegratorCategory::Game),
                IntegratorStatus::parse(&status_raw).unwrap_or(IntegratorStatus::Active),
            ),
        );
    }
    Ok(result)
}

/// Lowercase `[a-z0-9-]`, 2-64 characters. Deliberately rejects rather than
/// normalizes an out-of-charset slug (e.g. uppercase) — silently rewriting
/// it would surprise a caller who submitted something else and would still
/// have to be documented as a distinct behavior from what they asked for.
fn validate_slug(slug: &str) -> Result<(), AppError> {
    let len = slug.chars().count();
    if !(2..=64).contains(&len) {
        return Err(AppError::InvalidIntegratorSlug);
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(AppError::InvalidIntegratorSlug);
    }
    Ok(())
}

#[derive(Deserialize, ToSchema)]
pub struct InitialKeyRequest {
    pub algorithm: String,
    /// Standard-base64-encoded raw public key bytes.
    pub public_key: String,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateIntegratorRequest {
    pub slug: String,
    pub name: String,
    pub owner_name: String,
    #[serde(default)]
    pub requested_capabilities: Vec<String>,
    pub initial_key: InitialKeyRequest,
    /// `integrator` / `app` / `service` (#282); omitted means `integrator`.
    #[serde(default)]
    pub category: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct IntegratorCredentialResponse {
    pub integrator_id: Uuid,
    pub key_id: String,
}

#[derive(Serialize, ToSchema)]
pub struct IntegratorResponse {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub owner_name: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub registered_at: OffsetDateTime,
    pub status: String,
    pub category: String,
    pub requested_capabilities: Vec<String>,
    pub credential: IntegratorCredentialResponse,
}

#[utoipa::path(
    post,
    path = "/integrations",
    tag = "integrators",
    request_body = CreateIntegratorRequest,
    responses((status = 200, body = IntegratorResponse)),
)]
pub async fn register_integrator(
    State(state): State<AppState>,
    Json(body): Json<CreateIntegratorRequest>,
) -> Result<Json<IntegratorResponse>, AppError> {
    validate_slug(&body.slug)?;

    let category = match body.category.as_deref() {
        None => IntegratorCategory::Game,
        Some(raw) => IntegratorCategory::parse(raw).ok_or(AppError::InvalidIntegratorCategory)?,
    };

    if body.initial_key.algorithm != SUPPORTED_KEY_ALGORITHM {
        return Err(AppError::InvalidIntegratorKey);
    }
    let public_key_bytes = BASE64
        .decode(&body.initial_key.public_key)
        .map_err(|_| AppError::InvalidIntegratorKey)?;

    // Normalized through `Capability` so an unrecognized string round-trips
    // rather than erroring (see `crates/protocol/src/permissions.rs`'s own
    // doc comment) — a declaration presented to users, never validated
    // against a closed vocabulary here.
    let requested_capabilities: Vec<String> = body
        .requested_capabilities
        .iter()
        .map(|c| Capability::from(c.as_str()).as_str().to_string())
        .collect();

    let integrator_id = Uuid::new_v4();
    let key_id = Uuid::new_v4();
    let registered_at = OffsetDateTime::now_utc();

    let mut tx = state.pool.begin().await?;

    let inserted = sqlx::query(
        "INSERT INTO integrators (id, slug, name, owner_name, registered_at, status, category) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(integrator_id)
    .bind(&body.slug)
    .bind(&body.name)
    .bind(&body.owner_name)
    .bind(registered_at)
    .bind(IntegratorStatus::Active.as_str())
    .bind(category.as_str())
    .execute(&mut *tx)
    .await;
    if let Err(sqlx::Error::Database(db_err)) = &inserted {
        if db_err.is_unique_violation() {
            return Err(AppError::IntegratorSlugTaken);
        }
    }
    inserted?;

    for capability in &requested_capabilities {
        sqlx::query(
            "INSERT INTO integrator_requested_capabilities (integrator_id, capability) VALUES ($1, $2)",
        )
        .bind(integrator_id)
        .bind(capability)
        .execute(&mut *tx)
        .await?;
    }

    // #80/#84: the key registered here is always `root` — it doubles as
    // the issuer's root key and its first operational key by default (any
    // non-revoked, non-expired key may sign attestations regardless of
    // role; only key-*set* changes require root specifically).
    sqlx::query(
        "INSERT INTO issuer_keys (key_id, integrator_id, algorithm, public_key, role, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(key_id)
    .bind(integrator_id)
    .bind(&body.initial_key.algorithm)
    .bind(&public_key_bytes)
    .bind(KeyRole::Root.as_str())
    .bind(registered_at)
    .execute(&mut *tx)
    .await?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::GameRegistered
            .as_str()
            .to_string(),
        issuer: integrator_ref(&body.slug, "registered"),
        subject: integrator_ref(&body.slug, "registered"),
        // Durable ledger payload key: stays `developer` even though the
        // Rust/API field is now `owner_name` (#290) — see
        // `GameRegisteredPayload::developer`.
        payload: serde_json::to_value(GameRegisteredPayload {
            game_id: integrator_id,
            slug: body.slug.clone(),
            name: body.name.clone(),
            developer: body.owner_name.clone(),
            category: category.as_str().to_string(),
            requested_capabilities: requested_capabilities.clone(),
            initial_key: GameRegisteredKeyPayload {
                key_id,
                algorithm: body.initial_key.algorithm.clone(),
                public_key: BASE64.encode(&public_key_bytes),
            },
        })
        .expect("GameRegisteredPayload should serialize"),
        timestamp: registered_at,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(IntegratorResponse {
        id: integrator_id,
        slug: body.slug,
        name: body.name,
        owner_name: body.owner_name,
        registered_at,
        status: IntegratorStatus::Active.as_str().to_string(),
        category: category.as_str().to_string(),
        requested_capabilities,
        credential: IntegratorCredentialResponse {
            integrator_id,
            key_id: key_id.to_string(),
        },
    }))
}

/// `pub(crate)` so `connections.rs` can resolve a slug to an integrator id without
/// duplicating this lookup.
pub(crate) async fn fetch_integrator_id_by_slug(
    state: &AppState,
    slug: &str,
) -> Result<Uuid, AppError> {
    let row = sqlx::query("SELECT id FROM integrators WHERE slug = $1")
        .bind(slug)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::IntegratorNotFound)?;
    Ok(row.try_get("id")?)
}

#[derive(Serialize, ToSchema)]
pub struct IntegratorPublicResponse {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub owner_name: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub registered_at: OffsetDateTime,
    pub status: String,
    pub category: String,
    pub requested_capabilities: Vec<String>,
}

/// A public read of an integrator's registration — no credential fields, unlike
/// [`IntegratorResponse`] (which only `register_integrator` itself ever returns, to the
/// registrant, once). This is what the Hub's consent view (#27) and
/// `connections.rs`'s `POST /integrations/{slug}/connect` (to validate approved
/// capabilities against what the integrator actually declared) both read; same
/// visibility level `crates/server/src/guilds.rs`'s `get_guild` uses — no
/// auth required, nothing here is sensitive.
#[utoipa::path(
    get,
    path = "/integrations/{slug}",
    tag = "integrators",
    params(("slug" = String, Path)),
    responses((status = 200, body = IntegratorPublicResponse)),
)]
pub async fn get_integrator(
    State(state): State<AppState>,
    axum::extract::Path(slug): axum::extract::Path<String>,
) -> Result<Json<IntegratorPublicResponse>, AppError> {
    let row = sqlx::query(
        "SELECT id, slug, name, owner_name, registered_at, status, category FROM integrators WHERE slug = $1",
    )
    .bind(&slug)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::IntegratorNotFound)?;

    let integrator_id: Uuid = row.try_get("id")?;
    let capability_rows = sqlx::query(
        "SELECT capability FROM integrator_requested_capabilities WHERE integrator_id = $1",
    )
    .bind(integrator_id)
    .fetch_all(&state.pool)
    .await?;
    let requested_capabilities = capability_rows
        .into_iter()
        .map(|r| r.try_get::<String, _>("capability"))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Json(IntegratorPublicResponse {
        id: integrator_id,
        slug: row.try_get("slug")?,
        name: row.try_get("name")?,
        owner_name: row.try_get("owner_name")?,
        registered_at: row.try_get("registered_at")?,
        status: row.try_get("status")?,
        category: row.try_get("category")?,
        requested_capabilities,
    }))
}

/// `sort=` values `GET /integrations` (issue #270) accepts — deliberately just
/// these two, matching #89's "no ranking, no score" invariant: `newest`
/// (default) and `name`, mirroring `guilds::DiscoverSort` minus the
/// membership-derived `most_members` option integrators have no equivalent of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntegratorsListSort {
    Newest,
    Name,
}

impl IntegratorsListSort {
    fn parse(raw: Option<&str>) -> Result<IntegratorsListSort, AppError> {
        Ok(match raw {
            None | Some("newest") => IntegratorsListSort::Newest,
            Some("name") => IntegratorsListSort::Name,
            Some(_) => return Err(AppError::InvalidIntegratorsListQuery),
        })
    }
}

#[derive(Deserialize, utoipa::IntoParams)]
pub struct ListIntegratorsQuery {
    /// Free-text search over `name`/`slug`/`developer` (case-insensitive
    /// substring) — same shape `guilds::DiscoverGuildsQuery::q` uses.
    pub q: Option<String>,
    /// `newest` (default) | `name`.
    pub sort: Option<String>,
    pub limit: Option<i64>,
    /// The last integrator id from the previous page's results — same bare-id
    /// cursor shape `guilds::DiscoverGuildsQuery::cursor` uses.
    pub cursor: Option<Uuid>,
}

#[derive(Serialize, ToSchema)]
pub struct IntegratorSummary {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub owner_name: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub registered_at: OffsetDateTime,
    pub status: String,
    pub category: String,
}

#[derive(Serialize, ToSchema)]
pub struct ListIntegratorsResponse {
    pub integrators: Vec<IntegratorSummary>,
    /// `Some(id)` when another page exists — pass it back as `cursor=` to
    /// fetch it. `None` means this was the last page.
    pub next_cursor: Option<Uuid>,
}

/// Builds the `GET /integrations` query — split out from [`list_integrators`] so the
/// filter/sort/pagination logic can be unit-tested (via
/// [`sqlx::QueryBuilder::sql`]) without a live Postgres connection, same
/// pattern `guilds::build_discover_query` already established for #154.
fn build_integrators_list_query(
    query: &ListIntegratorsQuery,
    sort: IntegratorsListSort,
    limit: i64,
) -> QueryBuilder<Postgres> {
    let mut builder: QueryBuilder<Postgres> = QueryBuilder::new(
        "SELECT id, slug, name, owner_name, registered_at, status, category FROM integrators WHERE 1 = 1",
    );

    if let Some(q) = query.q.as_ref().filter(|s| !s.trim().is_empty()) {
        let like = format!("%{}%", crate::guilds::escape_like(q));
        builder.push(" AND (name ILIKE ");
        builder.push_bind(like.clone());
        builder.push(" OR slug ILIKE ");
        builder.push_bind(like.clone());
        builder.push(" OR owner_name ILIKE ");
        builder.push_bind(like);
        builder.push(")");
    }

    if let Some(cursor_id) = query.cursor {
        match sort {
            IntegratorsListSort::Newest => {
                builder.push(
                    " AND (registered_at, id) < (SELECT registered_at, id FROM integrators WHERE id = ",
                );
                builder.push_bind(cursor_id);
                builder.push(")");
            }
            IntegratorsListSort::Name => {
                builder.push(" AND (name, id) > (SELECT name, id FROM integrators WHERE id = ");
                builder.push_bind(cursor_id);
                builder.push(")");
            }
        }
    }

    match sort {
        IntegratorsListSort::Newest => builder.push(" ORDER BY registered_at DESC, id DESC"),
        IntegratorsListSort::Name => builder.push(" ORDER BY name ASC, id ASC"),
    };

    // Fetch one extra row past the page size, purely to know whether a next
    // page exists — trimmed back off before building the response, same
    // convention `build_discover_query` uses.
    builder.push(" LIMIT ");
    builder.push_bind(limit + 1);

    builder
}

/// `GET /integrations?q=&sort=&limit=&cursor=` (issue #270). Public, unauthenticated
/// — same visibility level [`get_integrator`] already uses. See the module doc
/// comment for the pagination/sort design.
#[utoipa::path(
    get,
    path = "/integrations",
    tag = "integrators",
    params(ListIntegratorsQuery),
    responses((status = 200, body = ListIntegratorsResponse)),
)]
pub async fn list_integrators(
    State(state): State<AppState>,
    Query(query): Query<ListIntegratorsQuery>,
) -> Result<Json<ListIntegratorsResponse>, AppError> {
    let sort = IntegratorsListSort::parse(query.sort.as_deref())?;
    let limit = query
        .limit
        .unwrap_or(DEFAULT_GAMES_LIST_PAGE_SIZE)
        .clamp(1, MAX_GAMES_LIST_PAGE_SIZE);

    let mut builder = build_integrators_list_query(&query, sort, limit);
    let rows = builder.build().fetch_all(&state.pool).await?;
    let has_more = rows.len() as i64 > limit;

    let mut integrators = Vec::with_capacity(rows.len().min(limit as usize));
    for row in rows.iter().take(limit as usize) {
        integrators.push(IntegratorSummary {
            id: row.try_get("id")?,
            slug: row.try_get("slug")?,
            name: row.try_get("name")?,
            owner_name: row.try_get("owner_name")?,
            registered_at: row.try_get("registered_at")?,
            status: row.try_get("status")?,
            category: row.try_get("category")?,
        });
    }
    let next_cursor = if has_more {
        integrators.last().map(|g| g.id)
    } else {
        None
    };

    Ok(Json(ListIntegratorsResponse {
        integrators,
        next_cursor,
    }))
}

#[derive(Serialize, ToSchema)]
pub struct IntegratorChallengeResponse {
    pub challenge_id: Uuid,
    /// Standard-base64-encoded random nonce the integrator must sign with its
    /// registered key and echo back (see [`authenticate_integrator`]).
    pub nonce: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub expires_at: OffsetDateTime,
}

#[utoipa::path(
    post,
    path = "/integrations/{slug}/challenge",
    tag = "integrators",
    params(("slug" = String, Path)),
    responses((status = 200, body = IntegratorChallengeResponse)),
)]
pub async fn create_integrator_challenge(
    State(state): State<AppState>,
    axum::extract::Path(slug): axum::extract::Path<String>,
) -> Result<Json<IntegratorChallengeResponse>, AppError> {
    let integrator_id = fetch_integrator_id_by_slug(&state, &slug).await?;

    let mut nonce = [0u8; GAME_CHALLENGE_NONCE_BYTES];
    rand::rng().fill_bytes(&mut nonce);
    let challenge_id = Uuid::new_v4();
    let expires_at =
        OffsetDateTime::now_utc() + time::Duration::minutes(GAME_CHALLENGE_TTL_MINUTES);

    sqlx::query(
        "INSERT INTO integrator_challenges (id, integrator_id, nonce, expires_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(challenge_id)
    .bind(integrator_id)
    .bind(nonce.as_slice())
    .bind(expires_at)
    .execute(&state.pool)
    .await?;

    Ok(Json(IntegratorChallengeResponse {
        challenge_id,
        nonce: BASE64.encode(nonce),
        expires_at,
    }))
}

/// Reads a header by trying `new_name` first, then `old_name` — either
/// name is accepted from a caller (#293), preferring the generic
/// `integrator` name when both happen to be present.
fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, AppError> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::InvalidIntegratorSignature)
}

fn issuer_key_from_row(row: &sqlx::postgres::PgRow) -> Result<IssuerKey, AppError> {
    let role_raw: String = row.try_get("role")?;
    // Issue #543: `purpose` defaults to `attestation` at the column level
    // (migration `0069`), but this also tolerates a query that hasn't
    // been updated to select it at all — same "never break on an
    // unrecognized/missing value" posture the wire-format default takes.
    let purpose_raw: Option<String> = row.try_get("purpose").ok();
    Ok(IssuerKey {
        key_id: row.try_get("key_id")?,
        algorithm: row.try_get("algorithm")?,
        public_key: row.try_get("public_key")?,
        role: KeyRole::parse(&role_raw).unwrap_or(KeyRole::Operational),
        purpose: purpose_raw
            .and_then(|p| KeyPurpose::parse(&p))
            .unwrap_or(KeyPurpose::Attestation),
        valid_from: row.try_get("created_at")?,
        valid_until: row.try_get("valid_until")?,
        revoked_at: row.try_get("revoked_at")?,
    })
}

/// Every key (any role, any status) this issuer has ever registered —
/// exactly the "issuer's key set" #84's `resolve_valid_signing_key`/
/// `resolve_valid_root_key` search over. `pub(crate)` so #32's attestation
/// issuance can resolve which of an issuer's keys signed a given
/// attestation, at that attestation's own point in time — not just
/// whichever key happens to be valid right now.
pub(crate) async fn fetch_issuer_keys(
    state: &AppState,
    integrator_id: Uuid,
) -> Result<Vec<IssuerKey>, AppError> {
    let rows = sqlx::query(
        "SELECT key_id, algorithm, public_key, role, purpose, created_at, valid_until, revoked_at \
         FROM issuer_keys WHERE integrator_id = $1",
    )
    .bind(integrator_id)
    .fetch_all(&state.pool)
    .await?;
    rows.iter().map(issuer_key_from_row).collect()
}

/// The batched sibling of [`fetch_issuer_keys`] — one query for every id in
/// `integrator_ids` instead of one query per id, same reasoning as
/// [`fetch_integrator_category_and_status_batch`] above.
pub(crate) async fn fetch_issuer_keys_batch(
    state: &AppState,
    integrator_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<IssuerKey>>, AppError> {
    if integrator_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query(
        "SELECT integrator_id, key_id, algorithm, public_key, role, purpose, created_at, valid_until, revoked_at \
         FROM issuer_keys WHERE integrator_id = ANY($1)",
    )
    .bind(integrator_ids)
    .fetch_all(&state.pool)
    .await?;

    let mut result: HashMap<Uuid, Vec<IssuerKey>> = HashMap::new();
    for row in &rows {
        let integrator_id: Uuid = row.try_get("integrator_id")?;
        result
            .entry(integrator_id)
            .or_default()
            .push(issuer_key_from_row(row)?);
    }
    Ok(result)
}

/// `GET /integrations/{slug}/keys` (#90) — public, unauthenticated: an issuer's
/// full key history (any role, any status), the read side of
/// [`add_issuer_key`]/[`revoke_issuer_key`]. Public keys are already public
/// by definition, and #90's design calls for the Hub to show an integrator's "key
/// history and status" on its profile page — nothing here is sensitive the
/// way the integrator's own root-key-authenticated endpoints are. Ordered oldest
/// first so a viewer reads it as a timeline.
#[utoipa::path(
    get,
    path = "/integrations/{slug}/keys",
    tag = "integrators",
    params(("slug" = String, Path)),
    responses((status = 200, body = Vec<IssuerKeyResponse>)),
)]
pub async fn list_issuer_keys(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<Vec<IssuerKeyResponse>>, AppError> {
    let integrator_id = fetch_integrator_id_by_slug(&state, &slug).await?;
    let mut keys = fetch_issuer_keys(&state, integrator_id).await?;
    keys.sort_by_key(|k| k.valid_from);
    Ok(Json(
        keys.into_iter()
            .map(|k| IssuerKeyResponse {
                key_id: k.key_id,
                algorithm: k.algorithm,
                role: k.role.as_str().to_string(),
                purpose: k.purpose.as_str().to_string(),
                valid_from: k.valid_from,
                valid_until: k.valid_until,
                revoked_at: k.revoked_at,
            })
            .collect(),
    ))
}

/// Shared core of [`authenticate_integrator`]/[`authenticate_integrator_root`]: verifies
/// an integrator's server-to-server request via the challenge-response scheme this
/// module's doc comment describes (the milestone-1 stand-in pending #80 —
/// now decided, this scheme's own future is a separate matter #84 doesn't
/// touch). Reads `key_id` / `challenge_id` / a base64 detached Ed25519
/// signature from fixed headers, consumes the matching `integrator_challenges` row
/// with a single `DELETE ... RETURNING` — the same single-use pattern
/// `handlers::register_finish` uses for `webauthn_ceremonies`, so a captured
/// signature can't be replayed against a second request — checks its TTL,
/// then verifies the signature against the stored public key for that
/// `key_id` via [`verify_event_signature`]. Returns the authenticated integrator's
/// id and the full [`IssuerKey`] record that authenticated it, so callers
/// can apply their own role/point-in-time check (#80/#84) — this function
/// itself only proves "this request was signed by whichever key `key_id`
/// names", nothing about whether that key is currently valid or what role
/// it holds.
async fn authenticate_integrator_key(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(Uuid, IssuerKey), AppError> {
    let key_id: Uuid = header_value(headers, INTEGRATOR_KEY_ID_HEADER)?
        .parse()
        .map_err(|_| AppError::InvalidIntegratorSignature)?;
    let challenge_id: Uuid = header_value(headers, INTEGRATOR_CHALLENGE_ID_HEADER)?
        .parse()
        .map_err(|_| AppError::InvalidIntegratorSignature)?;
    let signature_bytes = BASE64
        .decode(header_value(headers, INTEGRATOR_SIGNATURE_HEADER)?)
        .map_err(|_| AppError::InvalidIntegratorSignature)?;

    let challenge_row = sqlx::query(
        "DELETE FROM integrator_challenges WHERE id = $1 RETURNING integrator_id, nonce, expires_at",
    )
    .bind(challenge_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::IntegratorChallengeNotFound)?;
    let expires_at: OffsetDateTime = challenge_row.try_get("expires_at")?;
    if expires_at < OffsetDateTime::now_utc() {
        return Err(AppError::IntegratorChallengeExpired);
    }
    let integrator_id: Uuid = challenge_row.try_get("integrator_id")?;
    let nonce: Vec<u8> = challenge_row.try_get("nonce")?;

    let key_row = sqlx::query(
        "SELECT key_id, algorithm, public_key, role, purpose, created_at, valid_until, revoked_at \
         FROM issuer_keys WHERE key_id = $1 AND integrator_id = $2",
    )
    .bind(key_id)
    .bind(integrator_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::IntegratorKeyNotFound)?;
    let issuer_key = issuer_key_from_row(&key_row)?;

    if !verify_event_signature(&issuer_key.public_key, &nonce, &signature_bytes) {
        return Err(AppError::InvalidIntegratorSignature);
    }

    Ok((integrator_id, issuer_key))
}

/// Authenticates an integrator via any currently-valid key, root or operational
/// (#80/#84 — role only gates key-*set* changes, never ordinary
/// integrator-credentialed calls). Returns the authenticated integrator's id.
/// `pub(crate)` so integrator-authenticated endpoints (#27's capability grants,
/// achievement definitions, etc.) can reuse it.
pub(crate) async fn authenticate_integrator(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Uuid, AppError> {
    let (integrator_id, issuer_key) = authenticate_integrator_key(state, headers).await?;
    if !issuer_key.is_valid_at(OffsetDateTime::now_utc()) {
        return Err(AppError::IntegratorKeyNotFound);
    }
    Ok(integrator_id)
}

/// Authenticates an integrator via a currently-valid **root** key specifically
/// (#80/#84) — what [`add_issuer_key`]/[`revoke_issuer_key`] require, since
/// only a root key may authorize a key-set change. An otherwise-valid
/// operational key fails this with the same [`AppError::IssuerKeyNotRoot`]
/// a caller would see for a role it doesn't hold, not a generic auth
/// failure, so a legitimate integration can tell the two apart while
/// debugging.
pub(crate) async fn authenticate_integrator_root(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Uuid, AppError> {
    let (integrator_id, issuer_key) = authenticate_integrator_key(state, headers).await?;
    let now = OffsetDateTime::now_utc();
    if !issuer_key.is_valid_at(now) {
        return Err(AppError::IntegratorKeyNotFound);
    }
    if !issuer_key.authorizes_key_changes() {
        return Err(AppError::IssuerKeyNotRoot);
    }
    Ok(integrator_id)
}

#[derive(Deserialize, ToSchema)]
pub struct AddIssuerKeyRequest {
    pub algorithm: String,
    /// Standard-base64-encoded raw public key bytes, same shape
    /// [`InitialKeyRequest`] uses at registration.
    pub public_key: String,
    /// `"root"` or `"operational"` — see `avalon_protocol::integrators::KeyRole`.
    pub role: String,
    /// `"attestation"` (the default, omit for existing pre-#543 caller
    /// behavior) or `"shard_settlement"` — see
    /// `avalon_protocol::integrators::KeyPurpose`, issue #543.
    #[serde(default = "default_key_purpose")]
    pub purpose: String,
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[schema(value_type = String, format = "date-time", nullable)]
    pub valid_until: Option<OffsetDateTime>,
}

fn default_key_purpose() -> String {
    KeyPurpose::Attestation.as_str().to_string()
}

#[derive(Serialize, ToSchema)]
pub struct IssuerKeyResponse {
    pub key_id: Uuid,
    pub algorithm: String,
    pub role: String,
    pub purpose: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub valid_from: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[schema(value_type = String, format = "date-time", nullable)]
    pub valid_until: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[schema(value_type = String, format = "date-time", nullable)]
    pub revoked_at: Option<OffsetDateTime>,
}

/// `POST /integrations/{slug}/keys` (#84, implementing #80's decided two-tier key
/// model) — adds a new key to the issuer's key set. Requires the caller to
/// authenticate as the named `slug` with a currently-valid **root** key
/// ([`authenticate_integrator_root`]); an operational key, or a root key
/// belonging to a different integrator, is rejected. Emits `issuer.key_added`.
#[utoipa::path(
    post,
    path = "/integrations/{slug}/keys",
    tag = "integrators",
    params(("slug" = String, Path)),
    request_body = AddIssuerKeyRequest,
    responses((status = 200, body = IssuerKeyResponse)),
)]
pub async fn add_issuer_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(body): Json<AddIssuerKeyRequest>,
) -> Result<Json<IssuerKeyResponse>, AppError> {
    let path_integrator_id = fetch_integrator_id_by_slug(&state, &slug).await?;
    let caller_integrator_id = authenticate_integrator_root(&state, &headers).await?;
    if caller_integrator_id != path_integrator_id {
        return Err(AppError::IssuerKeyForbidden);
    }

    if body.algorithm != SUPPORTED_KEY_ALGORITHM {
        return Err(AppError::InvalidIntegratorKey);
    }
    let role = KeyRole::parse(&body.role).ok_or(AppError::InvalidIssuerKeyRole)?;
    let purpose = KeyPurpose::parse(&body.purpose).ok_or(AppError::InvalidIssuerKeyPurpose)?;
    let public_key_bytes = BASE64
        .decode(&body.public_key)
        .map_err(|_| AppError::InvalidIntegratorKey)?;

    let key_id = Uuid::new_v4();
    let valid_from = OffsetDateTime::now_utc();

    let mut tx = state.pool.begin().await?;

    sqlx::query(
        "INSERT INTO issuer_keys (key_id, integrator_id, algorithm, public_key, role, purpose, valid_until, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(key_id)
    .bind(path_integrator_id)
    .bind(&body.algorithm)
    .bind(&public_key_bytes)
    .bind(role.as_str())
    .bind(purpose.as_str())
    .bind(body.valid_until)
    .bind(valid_from)
    .execute(&mut *tx)
    .await?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::IssuerKeyAdded
            .as_str()
            .to_string(),
        issuer: integrator_ref(&slug, "key_added"),
        subject: integrator_ref(&slug, "key_added"),
        payload: serde_json::to_value(IssuerKeyAddedPayload {
            game_id: path_integrator_id,
            slug: slug.clone(),
            key_id,
            algorithm: body.algorithm.clone(),
            public_key: BASE64.encode(&public_key_bytes),
            role: role.as_str().to_string(),
            purpose: purpose.as_str().to_string(),
            valid_until: body.valid_until,
        })
        .expect("IssuerKeyAddedPayload should serialize"),
        timestamp: valid_from,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(IssuerKeyResponse {
        key_id,
        algorithm: body.algorithm,
        role: role.as_str().to_string(),
        purpose: purpose.as_str().to_string(),
        valid_from,
        valid_until: body.valid_until,
        revoked_at: None,
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct RevokeIssuerKeyRequest {
    pub reason: Option<String>,
}

/// `POST /integrations/{slug}/keys/{key_id}/revoke` (#84) — revokes a key in the
/// issuer's key set (root or operational; a root key can revoke itself, the
/// same "any key genuinely under your control" trust already implied by
/// authenticating as root at all). Same root-key-of-the-named-issuer
/// requirement as [`add_issuer_key`]. Revoking an already-revoked or
/// nonexistent key returns [`AppError::IssuerKeyForbidden`] rather than
/// silently succeeding — same posture `remove_friend`-style "no-op success"
/// endpoints elsewhere in this repo deliberately don't take, since a caller
/// retrying a revoke against a key it no longer controls is exactly the
/// kind of thing worth surfacing, not swallowing. Emits `issuer.key_revoked`.
#[utoipa::path(
    post,
    path = "/integrations/{slug}/keys/{key_id}/revoke",
    tag = "integrators",
    params(("slug" = String, Path), ("key_id" = Uuid, Path)),
    request_body = RevokeIssuerKeyRequest,
    responses((status = 200, body = IssuerKeyResponse)),
)]
pub async fn revoke_issuer_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((slug, key_id)): Path<(String, Uuid)>,
    Json(body): Json<RevokeIssuerKeyRequest>,
) -> Result<Json<IssuerKeyResponse>, AppError> {
    let path_integrator_id = fetch_integrator_id_by_slug(&state, &slug).await?;
    let caller_integrator_id = authenticate_integrator_root(&state, &headers).await?;
    if caller_integrator_id != path_integrator_id {
        return Err(AppError::IssuerKeyForbidden);
    }

    let revoked_at = OffsetDateTime::now_utc();
    let mut tx = state.pool.begin().await?;

    let row = sqlx::query(
        "UPDATE issuer_keys SET revoked_at = $1, revoked_reason = $2 \
         WHERE key_id = $3 AND integrator_id = $4 AND revoked_at IS NULL \
         RETURNING algorithm, role, purpose, created_at, valid_until",
    )
    .bind(revoked_at)
    .bind(&body.reason)
    .bind(key_id)
    .bind(path_integrator_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::IssuerKeyForbidden)?;

    let algorithm: String = row.try_get("algorithm")?;
    let role: String = row.try_get("role")?;
    let purpose: String = row.try_get("purpose")?;
    let valid_from: OffsetDateTime = row.try_get("created_at")?;
    let valid_until: Option<OffsetDateTime> = row.try_get("valid_until")?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: ProtocolEventKindVariant::IssuerKeyRevoked
            .as_str()
            .to_string(),
        issuer: integrator_ref(&slug, "key_revoked"),
        subject: integrator_ref(&slug, "key_revoked"),
        payload: serde_json::to_value(IssuerKeyRevokedPayload {
            game_id: path_integrator_id,
            slug: slug.clone(),
            key_id,
            revoked_at,
            reason: body.reason.clone(),
        })
        .expect("IssuerKeyRevokedPayload should serialize"),
        timestamp: revoked_at,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(IssuerKeyResponse {
        key_id,
        algorithm,
        role,
        purpose,
        valid_from,
        valid_until,
        revoked_at: Some(revoked_at),
    }))
}

#[derive(Serialize, ToSchema)]
pub struct IntegratorWhoamiResponse {
    pub integrator_id: Uuid,
}

/// Exists only to prove [`authenticate_integrator`] works end to end over real
/// HTTP (this ticket's own suggestion) — not a real capability-bearing
/// endpoint; #27 owns those.
#[utoipa::path(
    get,
    path = "/integrations/whoami",
    tag = "integrators",
    responses((status = 200, body = IntegratorWhoamiResponse)),
)]
pub async fn integrator_whoami(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<IntegratorWhoamiResponse>, AppError> {
    let integrator_id = authenticate_integrator(&state, &headers).await?;
    Ok(Json(IntegratorWhoamiResponse { integrator_id }))
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — pure-logic checks only. The
    //! endpoint-level flows (register, slug collision, challenge-response
    //! round-trip) are covered by `crates/server/tests/integrations.rs`, gated
    //! `--ignored`.

    use ed25519_dalek::{Signer, SigningKey};

    use super::*;

    #[test]
    fn validate_slug_accepts_lowercase_alphanumeric_and_hyphen() {
        assert!(validate_slug("ashen-realms").is_ok());
        assert!(validate_slug("a1-2").is_ok());
    }

    #[test]
    fn validate_slug_rejects_uppercase() {
        assert!(validate_slug("Ashen-Realms").is_err());
    }

    #[test]
    fn validate_slug_rejects_disallowed_characters() {
        assert!(validate_slug("ashen_realms").is_err());
        assert!(validate_slug("ashen realms").is_err());
        assert!(validate_slug("ashen.realms").is_err());
    }

    #[test]
    fn validate_slug_rejects_too_short_or_too_long() {
        assert!(validate_slug("a").is_err());
        assert!(validate_slug(&"a".repeat(65)).is_err());
        assert!(validate_slug(&"a".repeat(64)).is_ok());
    }

    #[test]
    fn integrator_ref_namespaces_by_slug_and_verb() {
        let global_id = integrator_ref("ashen-realms", "registered");
        assert_eq!(global_id.as_str(), "game:ashen-realms:self:registered");
    }

    /// Exercises the exact bytes/flow `authenticate_integrator` verifies over,
    /// without needing a database: an integrator signs a nonce with its own key,
    /// and `verify_event_signature` (the helper `authenticate_integrator` reuses
    /// rather than reimplementing) accepts it — and rejects a signature
    /// from any other key or over any other nonce.
    #[test]
    fn a_integrator_signed_nonce_verifies_against_its_own_key_only() {
        let mut csprng = rand::rng();
        let signing_key = SigningKey::generate(&mut csprng);
        let nonce = b"a-challenge-nonce";
        let signature = signing_key.sign(nonce);

        assert!(verify_event_signature(
            signing_key.verifying_key().as_bytes(),
            nonce,
            &signature.to_bytes(),
        ));

        let other_key = SigningKey::generate(&mut csprng);
        assert!(!verify_event_signature(
            other_key.verifying_key().as_bytes(),
            nonce,
            &signature.to_bytes(),
        ));
        assert!(!verify_event_signature(
            signing_key.verifying_key().as_bytes(),
            b"a-different-nonce",
            &signature.to_bytes(),
        ));
    }

    // --- Issue #270: GET /integrations cursor pagination ---------------------
    //
    // Same SQL-string-based assertions `guilds::build_discover_query`'s own
    // tests use — no live Postgres needed, just checking the query shape
    // `QueryBuilder` produces.

    fn empty_list_integrators_query() -> ListIntegratorsQuery {
        ListIntegratorsQuery {
            q: None,
            sort: None,
            limit: None,
            cursor: None,
        }
    }

    #[test]
    fn sort_parse_defaults_to_newest_and_rejects_unknown_values() {
        assert!(matches!(
            IntegratorsListSort::parse(None),
            Ok(IntegratorsListSort::Newest)
        ));
        assert!(matches!(
            IntegratorsListSort::parse(Some("newest")),
            Ok(IntegratorsListSort::Newest)
        ));
        assert!(matches!(
            IntegratorsListSort::parse(Some("name")),
            Ok(IntegratorsListSort::Name)
        ));
        assert!(matches!(
            IntegratorsListSort::parse(Some("most_players")),
            Err(AppError::InvalidIntegratorsListQuery)
        ));
    }

    #[test]
    fn select_list_includes_only_public_fields() {
        let builder = build_integrators_list_query(
            &empty_list_integrators_query(),
            IntegratorsListSort::Newest,
            20,
        );
        let sql = builder.sql();
        let sql = sql.as_str();
        assert!(sql.contains("id, slug, name, owner_name, registered_at, status, category"));
        assert!(sql.contains("FROM integrators"));
    }

    #[test]
    fn text_search_matches_name_slug_and_developer() {
        let mut query = empty_list_integrators_query();
        query.q = Some("ashen".to_string());
        let builder = build_integrators_list_query(&query, IntegratorsListSort::Newest, 20);
        let sql = builder.sql();
        let sql = sql.as_str();
        assert!(sql.contains("name ILIKE"));
        assert!(sql.contains("slug ILIKE"));
        assert!(sql.contains("owner_name ILIKE"));
    }

    #[test]
    fn blank_search_term_is_dropped_rather_than_matching_everything() {
        let mut query = empty_list_integrators_query();
        query.q = Some("   ".to_string());
        let builder = build_integrators_list_query(&query, IntegratorsListSort::Newest, 20);
        assert!(!builder.sql().as_str().contains("ILIKE"));
    }

    #[test]
    fn sort_selects_expected_order_by_clause() {
        let query = empty_list_integrators_query();

        let newest = build_integrators_list_query(&query, IntegratorsListSort::Newest, 20);
        assert!(newest
            .sql()
            .as_str()
            .contains("ORDER BY registered_at DESC, id DESC"));

        let name = build_integrators_list_query(&query, IntegratorsListSort::Name, 20);
        assert!(name.sql().as_str().contains("ORDER BY name ASC, id ASC"));
    }

    #[test]
    fn cursor_adds_keyset_pagination_clause_matching_the_active_sort() {
        let mut query = empty_list_integrators_query();
        query.cursor = Some(Uuid::new_v4());

        let newest = build_integrators_list_query(&query, IntegratorsListSort::Newest, 20);
        assert!(newest.sql().as_str().contains(
            "(registered_at, id) < (SELECT registered_at, id FROM integrators WHERE id ="
        ));

        let name = build_integrators_list_query(&query, IntegratorsListSort::Name, 20);
        assert!(name
            .sql()
            .as_str()
            .contains("(name, id) > (SELECT name, id FROM integrators WHERE id ="));
    }

    #[test]
    fn no_cursor_means_no_keyset_pagination_clause() {
        let query = empty_list_integrators_query();
        let builder = build_integrators_list_query(&query, IntegratorsListSort::Newest, 20);
        assert!(!builder.sql().as_str().contains("WHERE id ="));
    }

    #[test]
    fn limit_fetches_one_extra_row_to_detect_a_next_page() {
        let builder = build_integrators_list_query(
            &empty_list_integrators_query(),
            IntegratorsListSort::Newest,
            20,
        );
        // `push_bind` renders as a placeholder, not the literal value, so
        // this only confirms a LIMIT clause is present — the "+1" behavior
        // itself is exercised by the `#[ignore]`d integration test.
        assert!(builder.sql().as_str().contains(" LIMIT "));
    }
}
