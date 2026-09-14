//! Claim-definition CRUD per issuer (issue #31 for `Game`, generalized to
//! `App`/`Service` by #324/#325) — an issuer defines its achievements or
//! milestones before it can issue them (#32).
//!
//! **Category-driven vocabulary (#324, decided).** One shared mechanism,
//! one shared `achievement_definitions` table (the name predates #324 and
//! is kept — renaming a table that already holds real rows for zero
//! functional gain isn't worth the churn; see #324's own "same underlying
//! record shape" reasoning), one shared row/response shape
//! ([`AchievementDefinitionResponse`], whose field names were already
//! fully generic before this generalization — nothing in it says
//! "achievement"). What varies by the issuer's own registered category
//! (`IntegratorCategory::claim_kind`) is purely the *label*: `Game` issuers
//! keep `"achievement"` — `POST/PATCH/GET /integrations/{slug}/achievements`,
//! `game:<slug>:achievement:<key>`, `achievement.defined`/etc., exactly as
//! #31 shipped, zero churn — while `App`/`Service` issuers get
//! `"milestone"` — `POST/PATCH/GET /integrations/{slug}/milestones`,
//! `app:<slug>:milestone:<key>` or `service:<slug>:milestone:<key>`,
//! `milestone.defined`/etc. [`create_achievement_definition`]/
//! [`update_achievement_definition`]/[`list_achievement_definitions`] and
//! their milestone-route siblings ([`create_milestone_definition`]/etc.)
//! are thin, route-specific entry points over one shared core
//! ([`create_definition`]/[`update_definition`]/[`list_definitions`]) —
//! real shared code, not two parallel near-duplicate modules, per #325's
//! own suggestion.
//!
//! **A route's claim vocabulary is never caller-asserted.** Hitting
//! `/integrations/{slug}/achievements` for an issuer actually registered as
//! `App`/`Service` (or `/integrations/{slug}/milestones` for a `Game`) is
//! rejected ([`AppError::ClaimVocabularyMismatch`]) — the label is derived
//! from the issuer's own real registered category
//! (`integrators::fetch_integrator_category`), checked server-side, not trusted from
//! which URL the caller happened to call.
//!
//! **Namespacing.** A definition's `GlobalId` is
//! `<namespace>:<slug>:<claim_kind>:<key>` (`crates/protocol/src/ids.rs`),
//! minted by [`definition_ref`] the same way `crates/server/src/integrations.rs`'s
//! `integrator_ref`/`issuer_ref` and `guilds.rs`'s `guild_ref` namespace their own
//! events — `key` matches `[a-z0-9_]+` ([`validate_key`]), and the slug is
//! always the caller's own, taken from its registration (#26), never the
//! caller's choice. `id` is immutable once created; nothing in this module
//! ever changes it.
//!
//! **Auth.** All endpoints are integrator/app/service-credential-authenticated
//! (`crate::integrators::authenticate_integrator`, the challenge-response scheme #26
//! established), not a user session — defining a claim is something an
//! issuer does about its own catalogue, not something a user consents
//! to. Unlike issuing (#32, gated behind a capability grant), *defining*
//! needs nothing beyond the issuer proving its own identity. The write
//! endpoints additionally check that the authenticated issuer is the one
//! named by the `{slug}` path segment — an issuer authenticated as itself
//! can never create or change a definition under another issuer's slug
//! (`AppError::AchievementDefinitionForbidden`, 403). The `GET` list
//! endpoints are public and unauthenticated, same visibility level
//! `integrators::get_integrator` and `guilds::get_guild` already use.
//!
//! **Durability.** `achievement_definitions` is a projection; the
//! `<claim_kind>.defined`/`.definition_updated`/`.definition_retired`
//! family is the durable history, written into the outbox in the same
//! transaction as the row insert/update, same pattern
//! `friends.rs`/`guilds.rs`/`integrators.rs` already established for #71.
//! `issuer` is `<namespace>:<slug>:self:<verb>` (mirroring
//! `integrators::issuer_ref`); `subject` is the definition's own `GlobalId` —
//! matching the event-kind catalogue's "issuer → claim id" shape
//! (`docs/architecture/protocol-events.md`).
//!
//! **Update and retirement.** `PATCH .../{key}` updates
//! `name`/`description`/`schema` and bumps `version`, emitting
//! `<claim_kind>.definition_updated`; the id never changes. The same
//! endpoint also supports retiring a definition (`retired: true`) — no new
//! issuances against it (#32 enforces that), but existing attestations are
//! never touched and the row is never deleted, matching the ticket's "no
//! delete endpoint" design. Retiring emits `<claim_kind>.definition_retired`
//! instead of `.definition_updated` (a status change, not a definition
//! change) and does not bump `version`; retiring an already-retired
//! definition is a no-op that emits nothing, and a retired definition can
//! still have its name/description/schema edited in the same call.

use avalon_chain::attestations::{verify_authenticity, Authenticity};
use avalon_protocol::achievements::{AchievementAttestation, Issuer, Signature};
use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::ids::{AttestationId, GlobalId, IdentityId, IntegratorId};
use avalon_protocol::integrators::{resolve_valid_signing_key, IntegratorCategory};
use avalon_protocol::permissions::Capability;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::authz::{authenticate_caller, require_capability, Caller};
use crate::error::AppError;
use crate::handlers::is_http_url;
use crate::integrators::{
    authenticate_integrator, fetch_integrator_category, fetch_integrator_id_by_slug,
    fetch_issuer_keys, issuer_ref,
};
use crate::outbox;
use crate::state::AppState;

/// The built-in icon set shipped with `packages/ui` (issue #332) — a key
/// into `AchievementIconName` on the frontend
/// (`packages/ui/src/components/AvalonAchievementCard.types.ts`), generic
/// enough to cover integrators/apps/services alike. Kept as a small, fixed list
/// here (not a caller-extensible enum) so a bogus `icon` value can never
/// silently render as a blank/broken slot in the Hub.
const BUILTIN_ICONS: &[&str] = &["trophy", "star", "shield", "sword"];

/// The `endpoint` component of this write's idempotency-cache key (issue
/// #47) — shared by `achievements`/`milestones` routes since both go
/// through [`issue_attestation`], the one place an idempotency key is
/// actually honored today.
const ISSUE_ATTESTATION_IDEMPOTENCY_ENDPOINT: &str = "achievements.issue";

/// A definition with neither `icon` nor `icon_url` set still renders
/// *something* (issue #332's invariant) — this is what every reader falls
/// back to.
const DEFAULT_ICON: &str = "trophy";

/// Maximum length for `icon_url`, mirroring `handlers::MAX_AVATAR_URL_LEN`
/// (same class of integrator-hosted-image field, same cap).
const MAX_ICON_URL_LEN: usize = 2048;

/// `None`/absent is always fine (falls back to [`DEFAULT_ICON`] at read
/// time); a non-`None` value must be one of [`BUILTIN_ICONS`].
fn validate_icon(icon: Option<&str>) -> Result<(), AppError> {
    match icon {
        None => Ok(()),
        Some(icon) if BUILTIN_ICONS.contains(&icon) => Ok(()),
        Some(_) => Err(AppError::InvalidAchievementIcon),
    }
}

/// `None`/absent is always fine; a non-`None` value must be an `http`/
/// `https` URL within the length cap — same "the server records, it
/// doesn't vouch for content" posture `handlers::validate_avatar_url`
/// already takes toward integrator-supplied strings.
fn validate_icon_url(icon_url: Option<&str>) -> Result<(), AppError> {
    match icon_url {
        None => Ok(()),
        Some(url) if is_http_url(url, MAX_ICON_URL_LEN) => Ok(()),
        Some(_) => Err(AppError::InvalidAchievementIconUrl),
    }
}

/// `<category.as_str()>:<slug>:<category.claim_kind()>:<key>` — the
/// definition's immutable, globally unique id. `pub(crate)` so a future
/// issuing endpoint (#32) can build the same id to look up the definition
/// an attestation points at, rather than reimplementing this format.
pub(crate) fn definition_ref(category: IntegratorCategory, slug: &str, key: &str) -> GlobalId {
    GlobalId::new(category.as_str(), slug, category.claim_kind(), key)
}

/// Which route a definition-CRUD call came in on — #324's category split.
/// Not caller-asserted: [`authenticate_owning_issuer`] checks the
/// authenticating issuer's *actual* registered category against this
/// before allowing the call through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimRoute {
    Achievements,
    Milestones,
}

impl ClaimRoute {
    fn allows(self, category: IntegratorCategory) -> bool {
        match self {
            ClaimRoute::Achievements => category == IntegratorCategory::Game,
            ClaimRoute::Milestones => {
                matches!(
                    category,
                    IntegratorCategory::App | IntegratorCategory::Service
                )
            }
        }
    }

    /// The capability a user must have granted before this route's
    /// issuing endpoint may act on their behalf (issue #32/#28) — distinct
    /// wire strings per route (`achievements.issue` vs `milestones.issue`,
    /// #324) so a user's consent grant reads correctly for whichever
    /// vocabulary the issuer actually uses.
    fn issue_capability(self) -> Capability {
        match self {
            ClaimRoute::Achievements => Capability::AchievementsIssue,
            ClaimRoute::Milestones => Capability::MilestonesIssue,
        }
    }
}

/// Authenticates the calling issuer, checks it is the one named by `slug`,
/// and checks its actual registered category belongs to `route` — the
/// shared guard every write endpoint (achievement or milestone) uses.
/// Returns the path slug's own `integrator_id` and category (already resolved,
/// so callers don't fetch either twice).
async fn authenticate_owning_issuer(
    state: &AppState,
    headers: &HeaderMap,
    slug: &str,
    route: ClaimRoute,
) -> Result<(Uuid, IntegratorCategory), AppError> {
    let path_integrator_id = fetch_integrator_id_by_slug(state, slug).await?;
    let caller_integrator_id = authenticate_integrator(state, headers).await?;
    if caller_integrator_id != path_integrator_id {
        return Err(AppError::AchievementDefinitionForbidden);
    }
    let category = fetch_integrator_category(state, path_integrator_id).await?;
    if !route.allows(category) {
        return Err(AppError::ClaimVocabularyMismatch);
    }
    Ok((path_integrator_id, category))
}

/// Lowercase `[a-z0-9_]`, 2-128 characters — deliberately rejects rather
/// than normalizes an out-of-charset key, same posture
/// `integrators::validate_slug` documents for slugs.
fn validate_key(key: &str) -> Result<(), AppError> {
    let len = key.chars().count();
    if !(2..=128).contains(&len) {
        return Err(AppError::InvalidAchievementKey);
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(AppError::InvalidAchievementKey);
    }
    Ok(())
}

struct DefinitionRow {
    id: String,
    key: String,
    name: String,
    description: String,
    schema: Option<String>,
    icon: Option<String>,
    icon_url: Option<String>,
    version: i32,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    retired_at: Option<OffsetDateTime>,
}

async fn fetch_definition(
    state: &AppState,
    integrator_id: Uuid,
    key: &str,
) -> Result<DefinitionRow, AppError> {
    let row = sqlx::query(
        "SELECT id, key, name, description, schema, icon, icon_url, version, created_at, \
         updated_at, retired_at \
         FROM achievement_definitions WHERE integrator_id = $1 AND key = $2",
    )
    .bind(integrator_id)
    .bind(key)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::AchievementDefinitionNotFound)?;
    Ok(DefinitionRow {
        id: row.try_get("id")?,
        key: row.try_get("key")?,
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        schema: row.try_get("schema")?,
        icon: row.try_get("icon")?,
        icon_url: row.try_get("icon_url")?,
        version: row.try_get("version")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        retired_at: row.try_get("retired_at")?,
    })
}

#[derive(Serialize)]
pub struct AchievementDefinitionResponse {
    pub id: String,
    pub integrator_id: Uuid,
    pub key: String,
    pub name: String,
    pub description: String,
    pub schema: Option<String>,
    /// Always populated — falls back to [`DEFAULT_ICON`] when the
    /// definition has neither `icon` nor `icon_url` set, so every reader
    /// (the Hub's `AvalonAchievementCard`) always has *something* to
    /// render (issue #332's invariant), never a blank slot.
    pub icon: String,
    /// When present, takes precedence over `icon` on the client — never a
    /// silent fallback to the default just because both happen to be set.
    pub icon_url: Option<String>,
    pub version: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub retired: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub retired_at: Option<OffsetDateTime>,
}

fn definition_response(integrator_id: Uuid, row: DefinitionRow) -> AchievementDefinitionResponse {
    AchievementDefinitionResponse {
        id: row.id,
        integrator_id,
        key: row.key,
        name: row.name,
        description: row.description,
        schema: row.schema,
        icon: row.icon.unwrap_or_else(|| DEFAULT_ICON.to_string()),
        icon_url: row.icon_url,
        version: row.version,
        created_at: row.created_at,
        updated_at: row.updated_at,
        retired: row.retired_at.is_some(),
        retired_at: row.retired_at,
    }
}

#[derive(Deserialize)]
pub struct CreateAchievementDefinitionRequest {
    pub key: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub schema: Option<GlobalId>,
    /// One of [`BUILTIN_ICONS`]; omitted/`null` falls back to
    /// [`DEFAULT_ICON`] at read time (issue #332).
    #[serde(default)]
    pub icon: Option<String>,
    /// An integrator-hosted image URL, taking precedence over `icon` when
    /// present. `http`/`https` only.
    #[serde(default)]
    pub icon_url: Option<String>,
}

/// Shared core of [`create_achievement_definition`]/
/// [`create_milestone_definition`] (#324/#325) — 409 on a duplicate key for
/// this issuer; a different issuer (or the same issuer under a different
/// category — can't happen, category is fixed at registration) defining
/// the same key is a distinct id and always succeeds (namespacing's whole
/// point).
async fn create_definition(
    state: &AppState,
    headers: &HeaderMap,
    slug: &str,
    route: ClaimRoute,
    body: CreateAchievementDefinitionRequest,
) -> Result<Json<AchievementDefinitionResponse>, AppError> {
    let (integrator_id, category) = authenticate_owning_issuer(state, headers, slug, route).await?;
    validate_key(&body.key)?;
    validate_icon(body.icon.as_deref())?;
    validate_icon_url(body.icon_url.as_deref())?;

    let id = definition_ref(category, slug, &body.key);
    let claim_kind = category.claim_kind();
    let schema_str = body.schema.as_ref().map(GlobalId::as_str);
    let now = OffsetDateTime::now_utc();
    const INITIAL_VERSION: i32 = 1;

    let mut tx = state.pool.begin().await?;

    let inserted = sqlx::query(
        "INSERT INTO achievement_definitions \
         (id, integrator_id, key, name, description, schema, icon, icon_url, version, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)",
    )
    .bind(id.as_str())
    .bind(integrator_id)
    .bind(&body.key)
    .bind(&body.name)
    .bind(&body.description)
    .bind(schema_str)
    .bind(&body.icon)
    .bind(&body.icon_url)
    .bind(INITIAL_VERSION)
    .bind(now)
    .execute(&mut *tx)
    .await;
    if let Err(sqlx::Error::Database(db_err)) = &inserted {
        if db_err.is_unique_violation() {
            return Err(AppError::AchievementKeyTaken);
        }
    }
    inserted?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: format!("{claim_kind}.defined"),
        issuer: issuer_ref(category.as_str(), slug, &format!("{claim_kind}_defined")),
        subject: id.clone(),
        payload: serde_json::json!({
            "id": id.as_str(),
            "game_id": integrator_id,
            "slug": slug,
            "key": body.key,
            "name": body.name,
            "description": body.description,
            "schema": schema_str,
            "icon": body.icon,
            "icon_url": body.icon_url,
            "version": INITIAL_VERSION,
        }),
        timestamp: now,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    Ok(Json(definition_response(
        integrator_id,
        DefinitionRow {
            id: id.as_str().to_string(),
            key: body.key,
            name: body.name,
            description: body.description,
            schema: schema_str.map(str::to_string),
            icon: body.icon,
            icon_url: body.icon_url,
            version: INITIAL_VERSION,
            created_at: now,
            updated_at: now,
            retired_at: None,
        },
    )))
}

/// `POST /integrations/{slug}/achievements`.
pub async fn create_achievement_definition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(body): Json<CreateAchievementDefinitionRequest>,
) -> Result<Json<AchievementDefinitionResponse>, AppError> {
    create_definition(&state, &headers, &slug, ClaimRoute::Achievements, body).await
}

/// `POST /integrations/{slug}/milestones` (#324/#325).
pub async fn create_milestone_definition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(body): Json<CreateAchievementDefinitionRequest>,
) -> Result<Json<AchievementDefinitionResponse>, AppError> {
    create_definition(&state, &headers, &slug, ClaimRoute::Milestones, body).await
}

#[derive(Deserialize)]
pub struct UpdateAchievementDefinitionRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub schema: Option<GlobalId>,
    /// One of [`BUILTIN_ICONS`], `Some(None)`-style clearing is not
    /// supported (omit the field to leave it untouched, same "absent means
    /// untouched" convention every other field here already uses).
    #[serde(default)]
    pub icon: Option<String>,
    /// An integrator-hosted image URL; `http`/`https` only.
    #[serde(default)]
    pub icon_url: Option<String>,
    /// Set to `true` to retire the definition (see module doc comment).
    /// Never used to un-retire — retirement is one-way.
    #[serde(default)]
    pub retired: Option<bool>,
}

/// Shared core of [`update_achievement_definition`]/
/// [`update_milestone_definition`] (#324/#325) — updates
/// name/description/schema (bumping `version`) and/or retires the
/// definition. The id never changes.
async fn update_definition(
    state: &AppState,
    headers: &HeaderMap,
    slug: &str,
    key: String,
    route: ClaimRoute,
    body: UpdateAchievementDefinitionRequest,
) -> Result<Json<AchievementDefinitionResponse>, AppError> {
    let (integrator_id, category) = authenticate_owning_issuer(state, headers, slug, route).await?;
    validate_icon(body.icon.as_deref())?;
    validate_icon_url(body.icon_url.as_deref())?;
    let claim_kind = category.claim_kind();
    let existing = fetch_definition(state, integrator_id, &key).await?;

    let new_name = body.name.clone().unwrap_or_else(|| existing.name.clone());
    let new_description = body
        .description
        .clone()
        .unwrap_or_else(|| existing.description.clone());
    let new_schema = body
        .schema
        .as_ref()
        .map(|s| s.as_str().to_string())
        .or_else(|| existing.schema.clone());
    let new_icon = body.icon.clone().or_else(|| existing.icon.clone());
    let new_icon_url = body.icon_url.clone().or_else(|| existing.icon_url.clone());
    let definition_changed = body.name.is_some()
        || body.description.is_some()
        || body.schema.is_some()
        || body.icon.is_some()
        || body.icon_url.is_some();
    let now_retiring = body.retired == Some(true) && existing.retired_at.is_none();

    let now = OffsetDateTime::now_utc();
    let new_version = if definition_changed {
        existing.version + 1
    } else {
        existing.version
    };
    let new_retired_at = if now_retiring {
        Some(now)
    } else {
        existing.retired_at
    };

    let mut tx = state.pool.begin().await?;

    sqlx::query(
        "UPDATE achievement_definitions \
         SET name = $3, description = $4, schema = $5, icon = $6, icon_url = $7, version = $8, \
         updated_at = $9, retired_at = $10 \
         WHERE integrator_id = $1 AND key = $2",
    )
    .bind(integrator_id)
    .bind(&key)
    .bind(&new_name)
    .bind(&new_description)
    .bind(&new_schema)
    .bind(&new_icon)
    .bind(&new_icon_url)
    .bind(new_version)
    .bind(now)
    .bind(new_retired_at)
    .execute(&mut *tx)
    .await?;

    if definition_changed {
        let event = ProtocolEvent {
            id: Uuid::new_v4(),
            kind: format!("{claim_kind}.definition_updated"),
            issuer: issuer_ref(
                category.as_str(),
                slug,
                &format!("{claim_kind}_definition_updated"),
            ),
            subject: definition_ref(category, slug, &key),
            payload: serde_json::json!({
                "id": existing.id,
                "game_id": integrator_id,
                "slug": slug,
                "key": key,
                "name": new_name,
                "description": new_description,
                "schema": new_schema,
                "icon": new_icon,
                "icon_url": new_icon_url,
                "version": new_version,
            }),
            timestamp: now,
            version: 1,
        };
        outbox::enqueue(&mut tx, &event).await?;
    }

    if now_retiring {
        let event = ProtocolEvent {
            id: Uuid::new_v4(),
            kind: format!("{claim_kind}.definition_retired"),
            issuer: issuer_ref(
                category.as_str(),
                slug,
                &format!("{claim_kind}_definition_retired"),
            ),
            subject: definition_ref(category, slug, &key),
            payload: serde_json::json!({
                "id": existing.id,
                "game_id": integrator_id,
                "slug": slug,
                "key": key,
            }),
            timestamp: now,
            version: 1,
        };
        outbox::enqueue(&mut tx, &event).await?;
    }

    tx.commit().await?;

    Ok(Json(definition_response(
        integrator_id,
        DefinitionRow {
            id: existing.id,
            key,
            name: new_name,
            description: new_description,
            schema: new_schema,
            icon: new_icon,
            icon_url: new_icon_url,
            version: new_version,
            created_at: existing.created_at,
            updated_at: now,
            retired_at: new_retired_at,
        },
    )))
}

/// `PATCH /integrations/{slug}/achievements/{key}`.
pub async fn update_achievement_definition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((slug, key)): Path<(String, String)>,
    Json(body): Json<UpdateAchievementDefinitionRequest>,
) -> Result<Json<AchievementDefinitionResponse>, AppError> {
    update_definition(&state, &headers, &slug, key, ClaimRoute::Achievements, body).await
}

/// `PATCH /integrations/{slug}/milestones/{key}` (#324/#325).
pub async fn update_milestone_definition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((slug, key)): Path<(String, String)>,
    Json(body): Json<UpdateAchievementDefinitionRequest>,
) -> Result<Json<AchievementDefinitionResponse>, AppError> {
    update_definition(&state, &headers, &slug, key, ClaimRoute::Milestones, body).await
}

/// Shared core of [`list_achievement_definitions`]/
/// [`list_milestone_definitions`] (#324/#325) — public listing (feeds the
/// registry, #89). No auth required, same visibility level
/// `integrators::get_integrator` and `guilds::get_guild` already use. Includes retired
/// definitions (marked `retired: true`) rather than hiding them — a
/// retired definition's past attestations are still real and still need
/// somewhere to point.
async fn list_definitions(
    state: &AppState,
    slug: &str,
    route: ClaimRoute,
) -> Result<Json<Vec<AchievementDefinitionResponse>>, AppError> {
    let integrator_id = fetch_integrator_id_by_slug(state, slug).await?;
    // Every row under a given `integrator_id` is already the same claim kind by
    // construction — the write-side route check means an issuer's category
    // can never change after registration, so it can never accumulate rows
    // under more than one claim vocabulary. This check exists for the read
    // side specifically: without it, `/integrations/{app-slug}/achievements`
    // would silently serve that app's real milestones back mislabeled as
    // achievements, through the wrong URL's semantics.
    let category = fetch_integrator_category(state, integrator_id).await?;
    if !route.allows(category) {
        return Err(AppError::ClaimVocabularyMismatch);
    }

    let rows = sqlx::query(
        "SELECT id, key, name, description, schema, icon, icon_url, version, created_at, \
         updated_at, retired_at \
         FROM achievement_definitions WHERE integrator_id = $1 ORDER BY created_at",
    )
    .bind(integrator_id)
    .fetch_all(&state.pool)
    .await?;

    let mut definitions = Vec::with_capacity(rows.len());
    for row in rows {
        definitions.push(definition_response(
            integrator_id,
            DefinitionRow {
                id: row.try_get("id")?,
                key: row.try_get("key")?,
                name: row.try_get("name")?,
                description: row.try_get("description")?,
                schema: row.try_get("schema")?,
                icon: row.try_get("icon")?,
                icon_url: row.try_get("icon_url")?,
                version: row.try_get("version")?,
                created_at: row.try_get("created_at")?,
                updated_at: row.try_get("updated_at")?,
                retired_at: row.try_get("retired_at")?,
            },
        ));
    }
    Ok(Json(definitions))
}

/// `GET /integrations/{slug}/achievements`.
pub async fn list_achievement_definitions(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<Vec<AchievementDefinitionResponse>>, AppError> {
    list_definitions(&state, &slug, ClaimRoute::Achievements).await
}

/// `GET /integrations/{slug}/milestones` (#324/#325).
pub async fn list_milestone_definitions(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<Vec<AchievementDefinitionResponse>>, AppError> {
    list_definitions(&state, &slug, ClaimRoute::Milestones).await
}

#[derive(Deserialize)]
pub struct IssueAttestationRequest {
    /// Which of the issuer's own keys signed this attestation — resolved
    /// against that issuer's full key history at the moment of issuance
    /// (#84's `resolve_valid_signing_key`), not assumed to be the key that
    /// authenticated this HTTP request.
    pub key_id: Uuid,
    /// Standard-base64-encoded detached Ed25519 signature over
    /// [`attestation_signing_bytes`].
    pub signature: String,
    /// An optional, unverified pointer to supporting evidence (a replay
    /// id, a screenshot ref, whatever the issuer wants to attach) — carried
    /// through into the emitted event's payload only; not itself part of
    /// what's signed or stored as a column, since it's descriptive
    /// metadata, not something verification depends on.
    #[serde(default)]
    pub evidence: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize)]
pub struct AttestationSignatureResponse {
    pub key_id: Uuid,
    pub algorithm: String,
    pub bytes: String,
}

#[derive(Serialize, Deserialize)]
pub struct AttestationResponse {
    pub id: Uuid,
    pub issuer: String,
    pub subject: Uuid,
    pub achievement: String,
    #[serde(with = "time::serde::rfc3339")]
    pub issued_at: OffsetDateTime,
    pub proof: AttestationSignatureResponse,
}

/// Shared core of [`issue_achievement`]/[`issue_milestone`] (#32,
/// implementing #80/#84's key model and #324's category-driven vocabulary
/// over the same mechanism). See the module doc comment's "Auth" section
/// for the two independent checks every issuance goes through: the calling
/// integrator/app/service's own credential (who is this, on whose behalf), and
/// the *user's* consent grant for the issue capability — neither
/// substitutes for the other, and neither substitutes for the embedded
/// signature check below, which is the one piece of proof that would still
/// hold up even if the HTTP layer's own auth were somehow bypassed.
async fn issue_attestation(
    state: &AppState,
    headers: &HeaderMap,
    slug: &str,
    key: String,
    route: ClaimRoute,
    body: IssueAttestationRequest,
) -> Result<Json<AttestationResponse>, AppError> {
    // Who's calling, and on whose behalf — never a user's own session;
    // only an integrator/app/service issues attestations, per #32's own design.
    let caller = authenticate_caller(state, headers).await?;
    let Caller::Integrator {
        integrator_id,
        identity_id: subject_id,
    } = caller
    else {
        return Err(AppError::Forbidden);
    };

    // The caller must be the exact issuer named by {slug} — same guard
    // every other write endpoint in this module uses.
    let path_integrator_id = fetch_integrator_id_by_slug(state, slug).await?;
    if integrator_id != path_integrator_id {
        return Err(AppError::AchievementDefinitionForbidden);
    }

    // Issue #47: a caller that supplied an `Idempotency-Key` gets exactly
    // the same response replayed on a retry, never a second issuance — see
    // `crate::idempotency`'s own doc comment for why this endpoint is
    // where that first lands.
    let idempotency_key = crate::idempotency::read_idempotency_key(headers);
    if let Some(key) = &idempotency_key {
        if let Some(cached) = crate::idempotency::find_cached::<AttestationResponse>(
            state,
            integrator_id,
            key,
            ISSUE_ATTESTATION_IDEMPOTENCY_ENDPOINT,
        )
        .await?
        {
            return Ok(Json(cached));
        }
    }

    // The issuer's actual registered category must match this route's
    // claim vocabulary (#324) — an App/Service can't issue "achievements"
    // and an Integrator can't issue "milestones".
    let category = fetch_integrator_category(state, integrator_id).await?;
    if !route.allows(category) {
        return Err(AppError::ClaimVocabularyMismatch);
    }

    // The *user*'s own consent: an active binding to this issuer plus an
    // active grant for this route's issue capability (#28's guard, #32's
    // own "integrator caller with achievements.issue for the subject user"
    // requirement).
    require_capability(&caller, route.issue_capability(), state).await?;

    // The definition must exist and not be retired — no new issuances
    // against a retired definition (#31's invariant, still enforced here).
    let definition = fetch_definition(state, integrator_id, &key).await?;
    if definition.retired_at.is_some() {
        return Err(AppError::AttestationDefinitionRetired);
    }

    // The embedded signature: proof that one of the issuer's own keys —
    // not the node operator, not merely "whichever key authenticated this
    // HTTP request" — actually authorized this exact attestation. Verified
    // via the same shared, reusable check (#33's `verify_authenticity`)
    // that a future independent reader (`GET /attestations/{id}`) uses —
    // never duplicated ad hoc per call site. Resolved against the issuer's
    // *entire* key history at the moment of issuance, so a since-rotated
    // (but not-yet-revoked-at-the-time) key still works correctly under
    // #84's point-in-time model.
    let claim_kind = category.claim_kind();
    let issuer_str = format!("{}:{}", category.as_str(), slug);
    let signature_bytes = BASE64
        .decode(&body.signature)
        .map_err(|_| AppError::InvalidAttestationSignature)?;

    let issuer_keys = fetch_issuer_keys(state, integrator_id).await?;
    let now = OffsetDateTime::now_utc();
    let attestation_id = Uuid::new_v4();
    let issuer_enum = match category {
        IntegratorCategory::Game => Issuer::Game(IntegratorId(integrator_id)),
        IntegratorCategory::App => Issuer::App(IntegratorId(integrator_id)),
        IntegratorCategory::Service => Issuer::Service(IntegratorId(integrator_id)),
    };
    let candidate = AchievementAttestation {
        id: AttestationId(attestation_id),
        issuer: issuer_enum,
        subject: IdentityId(subject_id),
        achievement: definition_ref(category, slug, &key),
        issued_at: now,
        proof: Signature {
            key_id: body.key_id.to_string(),
            // The only algorithm registration accepts today (integrators::SUPPORTED_KEY_ALGORITHM);
            // the resolved key's own algorithm is authoritative for storage below.
            algorithm: "ed25519".to_string(),
            bytes: signature_bytes,
        },
    };
    let Authenticity::Authentic { .. } =
        verify_authenticity(&candidate, claim_kind, &issuer_str, &issuer_keys)
    else {
        return Err(AppError::InvalidAttestationSignature);
    };
    let signing_key = resolve_valid_signing_key(&issuer_keys, body.key_id, now)
        .expect("verify_authenticity already resolved this key successfully");

    let mut tx = state.pool.begin().await?;

    sqlx::query(
        "INSERT INTO achievement_attestations \
         (id, integrator_id, issuer, subject, achievement, issued_at, proof_key_id, proof_algorithm, proof_bytes) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(attestation_id)
    .bind(integrator_id)
    .bind(&issuer_str)
    .bind(subject_id)
    .bind(&definition.id)
    .bind(now)
    .bind(body.key_id)
    .bind(&signing_key.algorithm)
    .bind(&candidate.proof.bytes)
    .execute(&mut *tx)
    .await?;

    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: format!("{claim_kind}.issued"),
        issuer: issuer_ref(category.as_str(), slug, &format!("{claim_kind}_issued")),
        subject: issuer_ref(
            "identity",
            &subject_id.to_string(),
            &format!("{claim_kind}_issued"),
        ),
        payload: serde_json::json!({
            "id": attestation_id,
            "issuer": issuer_str,
            "subject": subject_id,
            "achievement": definition.id,
            "evidence": body.evidence,
            "proof": {
                "key_id": body.key_id,
                "algorithm": signing_key.algorithm,
                "bytes": body.signature,
            },
        }),
        timestamp: now,
        version: 1,
    };
    outbox::enqueue(&mut tx, &event).await?;

    tx.commit().await?;

    let response = AttestationResponse {
        id: attestation_id,
        issuer: issuer_str,
        subject: subject_id,
        achievement: definition.id,
        issued_at: now,
        proof: AttestationSignatureResponse {
            key_id: body.key_id,
            algorithm: signing_key.algorithm.clone(),
            bytes: body.signature,
        },
    };

    if let Some(key) = &idempotency_key {
        crate::idempotency::store(
            state,
            integrator_id,
            key,
            ISSUE_ATTESTATION_IDEMPOTENCY_ENDPOINT,
            &response,
        )
        .await?;
    }

    Ok(Json(response))
}

/// `POST /integrations/{slug}/achievements/{key}/issue` (#32).
pub async fn issue_achievement(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((slug, key)): Path<(String, String)>,
    Json(body): Json<IssueAttestationRequest>,
) -> Result<Json<AttestationResponse>, AppError> {
    issue_attestation(&state, &headers, &slug, key, ClaimRoute::Achievements, body).await
}

/// `POST /integrations/{slug}/milestones/{key}/issue` (#32/#324/#325).
pub async fn issue_milestone(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((slug, key)): Path<(String, String)>,
    Json(body): Json<IssueAttestationRequest>,
) -> Result<Json<AttestationResponse>, AppError> {
    issue_attestation(&state, &headers, &slug, key, ClaimRoute::Milestones, body).await
}

/// A pure, DB-free projection of a definition's current state from its own
/// event history — proves `achievement_definitions` is genuinely derived
/// from `achievement.defined`/`.definition_updated`/`.definition_retired`
/// rather than a second source of truth (issue #31's "rebuild" test,
/// mirroring `avalon_indexer::Indexer::rebuild`'s fold-over-events shape,
/// since no concrete per-domain projection exists yet in the still-stub
/// `indexer` crate for this or any other table). `pub(crate)` for this
/// module's own tests.
// Only exercised by this module's own tests today (there is no live caller
// yet — no concrete indexer projection exists in this repo for anything,
// see the doc comment above), same `#![allow(dead_code)]` posture
// `crate::authz` documents for its own not-yet-wired-up infrastructure.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RebuiltDefinition {
    pub name: String,
    pub description: String,
    pub schema: Option<String>,
    pub version: u32,
    pub retired: bool,
}

#[allow(dead_code)]
pub(crate) fn rebuild_definition(events: &[ProtocolEvent]) -> Option<RebuiltDefinition> {
    let mut state: Option<RebuiltDefinition> = None;
    for event in events {
        match event.kind.as_str() {
            "achievement.defined" => {
                state = Some(RebuiltDefinition {
                    name: event.payload["name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    description: event.payload["description"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    schema: event.payload["schema"].as_str().map(str::to_string),
                    version: event.payload["version"].as_u64().unwrap_or(1) as u32,
                    retired: false,
                });
            }
            "achievement.definition_updated" => {
                if let Some(def) = state.as_mut() {
                    def.name = event.payload["name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    def.description = event.payload["description"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    def.schema = event.payload["schema"].as_str().map(str::to_string);
                    def.version = event.payload["version"].as_u64().unwrap_or_default() as u32;
                }
            }
            "achievement.definition_retired" => {
                if let Some(def) = state.as_mut() {
                    def.retired = true;
                }
            }
            _ => {}
        }
    }
    state
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — pure-logic checks only. The
    //! endpoint-level flows (create, duplicate-key 409, cross-slug 403,
    //! update bumps version, list) are covered by
    //! `crates/server/tests/achievements.rs`, gated `--ignored`.

    use super::*;

    #[test]
    fn validate_key_accepts_lowercase_alphanumeric_and_underscore() {
        assert!(validate_key("dragon_slayer").is_ok());
        assert!(validate_key("ab").is_ok());
        assert!(validate_key("a1_2").is_ok());
    }

    #[test]
    fn validate_key_rejects_uppercase_and_hyphens() {
        assert!(validate_key("Dragon_Slayer").is_err());
        assert!(validate_key("dragon-slayer").is_err());
        assert!(validate_key("dragon slayer").is_err());
    }

    #[test]
    fn validate_key_rejects_too_short_or_too_long() {
        assert!(validate_key("a").is_err());
        assert!(validate_key(&"a".repeat(129)).is_err());
        assert!(validate_key(&"a".repeat(128)).is_ok());
    }

    #[test]
    fn definition_ref_namespaces_by_category_slug_and_key() {
        let id = definition_ref(IntegratorCategory::Game, "ashen-realms", "dragon_slayer");
        assert_eq!(id.as_str(), "game:ashen-realms:achievement:dragon_slayer");
    }

    #[test]
    fn two_integrators_defining_the_same_key_produce_distinct_ids() {
        let a = definition_ref(IntegratorCategory::Game, "ashen-realms", "dragon_slayer");
        let b = definition_ref(IntegratorCategory::Game, "worldzero", "dragon_slayer");
        assert_ne!(a, b);
    }

    /// #324/#325: an App/Service issuer's definitions use "milestone", not
    /// "achievement" — same key, same slug, a genuinely different id.
    #[test]
    fn app_and_service_issuers_get_the_milestone_vocabulary_not_achievement() {
        let app_id = definition_ref(IntegratorCategory::App, "wallet-app", "onboarded");
        assert_eq!(app_id.as_str(), "app:wallet-app:milestone:onboarded");

        let service_id = definition_ref(IntegratorCategory::Service, "payments", "onboarded");
        assert_eq!(service_id.as_str(), "service:payments:milestone:onboarded");

        let integrator_id = definition_ref(IntegratorCategory::Game, "wallet-app", "onboarded");
        assert_ne!(
            app_id, integrator_id,
            "different category, same slug/key: still distinct ids"
        );
    }

    #[test]
    fn validate_icon_accepts_none_and_builtin_keys() {
        assert!(validate_icon(None).is_ok());
        assert!(validate_icon(Some("trophy")).is_ok());
        assert!(validate_icon(Some("star")).is_ok());
        assert!(validate_icon(Some("shield")).is_ok());
        assert!(validate_icon(Some("sword")).is_ok());
    }

    #[test]
    fn validate_icon_rejects_unknown_keys() {
        assert!(validate_icon(Some("dragon")).is_err());
        assert!(validate_icon(Some("")).is_err());
    }

    #[test]
    fn validate_icon_url_accepts_none_and_http_urls() {
        assert!(validate_icon_url(None).is_ok());
        assert!(validate_icon_url(Some("https://cdn.example.com/icon.png")).is_ok());
        assert!(validate_icon_url(Some("http://example.com/icon.png")).is_ok());
    }

    #[test]
    fn validate_icon_url_rejects_non_http_schemes() {
        assert!(validate_icon_url(Some("ftp://example.com/icon.png")).is_err());
        assert!(validate_icon_url(Some("javascript:alert(1)")).is_err());
        assert!(validate_icon_url(Some("not a url")).is_err());
    }

    #[test]
    fn claim_route_only_allows_its_own_categories() {
        assert!(ClaimRoute::Achievements.allows(IntegratorCategory::Game));
        assert!(!ClaimRoute::Achievements.allows(IntegratorCategory::App));
        assert!(!ClaimRoute::Achievements.allows(IntegratorCategory::Service));

        assert!(!ClaimRoute::Milestones.allows(IntegratorCategory::Game));
        assert!(ClaimRoute::Milestones.allows(IntegratorCategory::App));
        assert!(ClaimRoute::Milestones.allows(IntegratorCategory::Service));
    }

    fn defined_event(
        name: &str,
        description: &str,
        schema: Option<&str>,
        version: u32,
    ) -> ProtocolEvent {
        ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "achievement.defined".to_string(),
            issuer: issuer_ref("game", "ashen-realms", "achievement_defined"),
            subject: definition_ref(IntegratorCategory::Game, "ashen-realms", "dragon_slayer"),
            payload: serde_json::json!({
                "name": name,
                "description": description,
                "schema": schema,
                "version": version,
            }),
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }
    }

    fn updated_event(
        name: &str,
        description: &str,
        schema: Option<&str>,
        version: u32,
    ) -> ProtocolEvent {
        let mut event = defined_event(name, description, schema, version);
        event.kind = "achievement.definition_updated".to_string();
        event
    }

    fn retired_event() -> ProtocolEvent {
        ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "achievement.definition_retired".to_string(),
            issuer: issuer_ref("game", "ashen-realms", "achievement_definition_retired"),
            subject: definition_ref(IntegratorCategory::Game, "ashen-realms", "dragon_slayer"),
            payload: serde_json::json!({}),
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }
    }

    #[test]
    fn rebuild_from_only_a_defined_event_matches_creation() {
        let events = vec![defined_event("Dragon Slayer", "Slew the dragon", None, 1)];
        let rebuilt = rebuild_definition(&events).unwrap();
        assert_eq!(rebuilt.name, "Dragon Slayer");
        assert_eq!(rebuilt.description, "Slew the dragon");
        assert_eq!(rebuilt.schema, None);
        assert_eq!(rebuilt.version, 1);
        assert!(!rebuilt.retired);
    }

    #[test]
    fn rebuild_applies_updates_in_order_and_bumps_version() {
        let events = vec![
            defined_event("Dragon Slayer", "Slew the dragon", None, 1),
            updated_event(
                "Dragon Slayer",
                "Slew the Dragon Lord",
                Some("game:ashen-realms:achievement:schema:v1"),
                2,
            ),
        ];
        let rebuilt = rebuild_definition(&events).unwrap();
        assert_eq!(rebuilt.description, "Slew the Dragon Lord");
        assert_eq!(
            rebuilt.schema.as_deref(),
            Some("game:ashen-realms:achievement:schema:v1")
        );
        assert_eq!(rebuilt.version, 2);
    }

    #[test]
    fn rebuild_reflects_retirement_without_touching_other_fields() {
        let events = vec![
            defined_event("Dragon Slayer", "Slew the dragon", None, 1),
            retired_event(),
        ];
        let rebuilt = rebuild_definition(&events).unwrap();
        assert!(rebuilt.retired);
        assert_eq!(rebuilt.name, "Dragon Slayer");
        assert_eq!(rebuilt.version, 1);
    }

    #[test]
    fn rebuild_with_no_events_yields_nothing() {
        assert!(rebuild_definition(&[]).is_none());
    }
}
