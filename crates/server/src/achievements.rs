//! Claim-definition CRUD per issuer (issue #31 for `Game`, generalized to
//! `App`/`Service` by #324/#325) — an issuer defines its achievements or
//! milestones before it can issue them (#32). See
//! `docs/architecture/achievements-and-attestations.md`'s "Today in the
//! repo" and "Namespacing" sections for the category-driven claim
//! vocabulary, auth model, and update/retirement semantics.

use avalon_chain::attestations::{verify_authenticity, verify_signature, Authenticity};
use avalon_protocol::achievements::{
    bulk_attestation_signing_bytes, AchievementAttestation, Issuer, Signature,
};
use avalon_protocol::event_payloads::{
    ClaimDefinedPayload, ClaimDefinitionRetiredPayload, ClaimDefinitionUpdatedPayload,
    ClaimIssuedPayload, ClaimProofPayload,
};
use avalon_protocol::events::{ProtocolEvent, ProtocolEventKindVariant};
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
use crate::issuer_registration::ensure_issuer_registered;
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

/// Cache key for bulk issuance's own idempotency entries (#495) — deliberately
/// distinct from [`ISSUE_ATTESTATION_IDEMPOTENCY_ENDPOINT`] so a bulk call and
/// a single-claim call can never collide on the same idempotency key by
/// accident.
const BULK_ISSUE_ATTESTATION_IDEMPOTENCY_ENDPOINT: &str = "achievements.bulk_issue";

/// A bulk call's own claim-count cap (#495) — generous enough for "a
/// veteran player's full in-game achievement history" (this ticket's own
/// motivating case), small enough that a single request body/transaction
/// can't grow unbounded. An oversized or empty list is
/// [`AppError::InvalidBulkAttestationRequest`], never silently truncated.
pub(crate) const MAX_BULK_CLAIMS: usize = 1000;

/// Default cap on how many attestations one issuer may write about one
/// subject within [`DEFAULT_WRITE_QUOTA_WINDOW_HOURS`] (#365, #306's
/// abuse-floor piece) — overridable via `AVALON_ACHIEVEMENT_WRITE_QUOTA`
/// so live tests can exercise the rejection path without actually writing
/// hundreds of attestations. Deliberately generous: a legitimate bulk
/// issuance ([`MAX_BULK_CLAIMS`]) can still land in one window; this is a
/// floor against runaway/abusive volume, never a throughput target.
const DEFAULT_WRITE_QUOTA: i64 = 500;
const DEFAULT_WRITE_QUOTA_WINDOW_HOURS: i64 = 1;

fn write_quota_from_env() -> i64 {
    std::env::var("AVALON_ACHIEVEMENT_WRITE_QUOTA")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_WRITE_QUOTA)
}

fn write_quota_window_hours_from_env() -> i64 {
    std::env::var("AVALON_ACHIEVEMENT_WRITE_QUOTA_WINDOW_HOURS")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|hours| *hours > 0)
        .unwrap_or(DEFAULT_WRITE_QUOTA_WINDOW_HOURS)
}

/// Volume-only abuse floor (#365) — never evaluates *what* is being
/// attested to, only how many writes one issuer has made about one
/// subject recently. `additional` is however many writes this call would
/// add (1 for a single issuance, `body.claims.len()` for a bulk call) so a
/// bulk call that would *cross* the quota is rejected as a whole, not
/// partially applied.
pub(crate) fn guard_write_quota(recent_count: i64, additional: i64) -> Result<(), AppError> {
    if recent_count + additional > write_quota_from_env() {
        return Err(AppError::AttestationWriteQuotaExceeded);
    }
    Ok(())
}

/// How many attestations `issuer_str` has written about `subject_id`
/// within the current quota window — same rolling-window-count shape as
/// `recovery::recent_request_count`. Scoped to `achievement_attestations`
/// specifically: the quota is about durable ledger-backed writes, not
/// chat/presence, which never reach this table at all.
async fn recent_issuer_subject_write_count(
    state: &AppState,
    issuer_str: &str,
    subject_id: Uuid,
) -> Result<i64, AppError> {
    let window_start =
        OffsetDateTime::now_utc() - time::Duration::hours(write_quota_window_hours_from_env());
    let row = sqlx::query(
        "SELECT COUNT(*) AS count FROM achievement_attestations \
         WHERE issuer = $1 AND subject = $2 AND issued_at >= $3",
    )
    .bind(issuer_str)
    .bind(subject_id)
    .bind(window_start)
    .fetch_one(&state.pool)
    .await?;
    Ok(row.try_get("count")?)
}

/// A definition with neither `icon` nor `icon_url` set still renders
/// *something* (issue #332's invariant) — this is what every reader falls
/// back to.
const DEFAULT_ICON: &str = "trophy";

/// Maximum length for `icon_url`, mirroring `handlers::MAX_AVATAR_URL_LEN`
/// (same class of integrator-hosted-image field, same cap).
const MAX_ICON_URL_LEN: usize = 2048;

/// Picks the right typed kind for a dynamically-chosen claim vocabulary
/// (issue #82) — `claim_kind` is `"achievement"` or `"milestone"`
/// (`IntegratorCategory::claim_kind`), never caller-chosen. The two kinds
/// share one payload schema per row (see
/// `docs/architecture/protocol-events-catalogue.md`); only the *kind
/// string* differs, matching #324/#325's own "which vocabulary, never a
/// payload difference" split.
pub(crate) fn claim_kind_variant(
    claim_kind: &str,
    achievement: ProtocolEventKindVariant,
    milestone: ProtocolEventKindVariant,
) -> ProtocolEventKindVariant {
    if claim_kind == "achievement" {
        achievement
    } else {
        milestone
    }
}

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
        kind: claim_kind_variant(
            claim_kind,
            ProtocolEventKindVariant::AchievementDefined,
            ProtocolEventKindVariant::MilestoneDefined,
        )
        .as_str()
        .to_string(),
        issuer: issuer_ref(category.as_str(), slug, &format!("{claim_kind}_defined")),
        subject: id.clone(),
        payload: serde_json::to_value(ClaimDefinedPayload {
            id: id.as_str().to_string(),
            game_id: integrator_id,
            slug: slug.to_string(),
            key: body.key.clone(),
            name: body.name.clone(),
            description: body.description.clone(),
            schema: schema_str.map(str::to_string),
            icon: body.icon.clone(),
            icon_url: body.icon_url.clone(),
            version: INITIAL_VERSION,
        })
        .expect("ClaimDefinedPayload should serialize"),
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
            kind: claim_kind_variant(
                claim_kind,
                ProtocolEventKindVariant::AchievementDefinitionUpdated,
                ProtocolEventKindVariant::MilestoneDefinitionUpdated,
            )
            .as_str()
            .to_string(),
            issuer: issuer_ref(
                category.as_str(),
                slug,
                &format!("{claim_kind}_definition_updated"),
            ),
            subject: definition_ref(category, slug, &key),
            payload: serde_json::to_value(ClaimDefinitionUpdatedPayload {
                id: existing.id.clone(),
                game_id: integrator_id,
                slug: slug.to_string(),
                key: key.clone(),
                name: new_name.clone(),
                description: new_description.clone(),
                schema: new_schema.clone(),
                icon: new_icon.clone(),
                icon_url: new_icon_url.clone(),
                version: new_version,
            })
            .expect("ClaimDefinitionUpdatedPayload should serialize"),
            timestamp: now,
            version: 1,
        };
        outbox::enqueue(&mut tx, &event).await?;
    }

    if now_retiring {
        let event = ProtocolEvent {
            id: Uuid::new_v4(),
            kind: claim_kind_variant(
                claim_kind,
                ProtocolEventKindVariant::AchievementDefinitionRetired,
                ProtocolEventKindVariant::MilestoneDefinitionRetired,
            )
            .as_str()
            .to_string(),
            issuer: issuer_ref(
                category.as_str(),
                slug,
                &format!("{claim_kind}_definition_retired"),
            ),
            subject: definition_ref(category, slug, &key),
            payload: serde_json::to_value(ClaimDefinitionRetiredPayload {
                id: existing.id.clone(),
                game_id: integrator_id,
                slug: slug.to_string(),
                key: key.clone(),
            })
            .expect("ClaimDefinitionRetiredPayload should serialize"),
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

    // #365: the write-time abuse floor, checked before the (more
    // expensive) signature verification below — same ordering
    // `recovery::start_request` uses for its own rate limit.
    let recent_writes = recent_issuer_subject_write_count(state, &issuer_str, subject_id).await?;
    guard_write_quota(recent_writes, 1)?;

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

    // #481: authenticity (just verified above) and network-admission are
    // two independent checks — a signature valid against the issuer's own
    // key history still isn't enough to write here unless this exact key
    // has also been admitted on this network (explicitly, or implicitly
    // via dev/int auto-registration on this first valid write). Runs
    // inside this same transaction so an auto-registration and the write
    // it admits commit atomically together.
    ensure_issuer_registered(
        &mut tx,
        state.chain.network_id(),
        &signing_key.public_key,
        &issuer_str,
    )
    .await?;

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
        kind: claim_kind_variant(
            claim_kind,
            ProtocolEventKindVariant::AchievementIssued,
            ProtocolEventKindVariant::MilestoneIssued,
        )
        .as_str()
        .to_string(),
        issuer: issuer_ref(category.as_str(), slug, &format!("{claim_kind}_issued")),
        subject: issuer_ref(
            "identity",
            &subject_id.to_string(),
            &format!("{claim_kind}_issued"),
        ),
        payload: serde_json::to_value(ClaimIssuedPayload {
            id: attestation_id,
            issuer: issuer_str.clone(),
            subject: subject_id,
            achievement: definition.id.clone(),
            evidence: body.evidence.clone(),
            proof: ClaimProofPayload {
                key_id: body.key_id,
                algorithm: signing_key.algorithm.clone(),
                bytes: body.signature.clone(),
            },
        })
        .expect("ClaimIssuedPayload should serialize"),
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

/// One claim in a [`BulkIssueAttestationRequest`] — just enough to look up
/// its definition; the proof covering the whole ordered list lives once,
/// at the request's top level (see that struct's own doc comment).
#[derive(Deserialize)]
pub struct BulkClaimRequest {
    pub key: String,
    #[serde(default)]
    pub evidence: Option<serde_json::Value>,
}

/// `POST /integrations/{slug}/achievements/bulk-issue` /
/// `.../milestones/bulk-issue` (issue #495, implementing #492's decided
/// shape). One challenge-response proof that this integrator's key is
/// making the call, plus **one** signature over
/// [`bulk_attestation_signing_bytes`] of the whole ordered `claims` list —
/// never a per-claim signature. Every claim still becomes its own ordinary
/// attestation server-side, through the exact same write path
/// [`issue_attestation`] uses per-item; this endpoint is purely an
/// API/transport-layer convenience over that, per #492's own invariant.
#[derive(Deserialize)]
pub struct BulkIssueAttestationRequest {
    /// Which of the issuer's own keys signed the whole ordered list.
    pub key_id: Uuid,
    /// Standard-base64-encoded detached Ed25519 signature over
    /// [`bulk_attestation_signing_bytes`] of `claims`, in order.
    pub signature: String,
    pub claims: Vec<BulkClaimRequest>,
}

/// One claim's own outcome — a bulk call is never all-or-nothing (#495's
/// own invariant): a claim referencing an unknown or retired definition
/// fails on its own, every other claim in the same call still succeeds.
#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum BulkClaimResult {
    Issued {
        key: String,
        attestation: AttestationResponse,
    },
    Failed {
        key: String,
        /// A stable machine-readable code, matching `AppError::code`'s own
        /// convention elsewhere in this codebase — never just the free-text
        /// `error` string alone.
        code: String,
        error: String,
    },
}

#[derive(Serialize, Deserialize)]
pub struct BulkIssueAttestationResponse {
    /// Same order as the request's `claims` — a caller matches results
    /// back to what it submitted by position, not by searching for `key`
    /// (which isn't itself guaranteed unique within one call).
    pub results: Vec<BulkClaimResult>,
}

/// Shared core of the two bulk-issuance handlers below — mirrors
/// [`issue_attestation`]'s own auth/capability checks exactly (one
/// integrator credential, one subject, one capability check for the whole
/// call, since every claim in a bulk call shares the same subject and
/// issuer), but resolves the *list* of achievement refs up front (pure, no
/// DB) so the one signature can be verified once against all of them
/// before any per-claim database work happens.
async fn bulk_issue_attestation(
    state: &AppState,
    headers: &HeaderMap,
    slug: &str,
    route: ClaimRoute,
    body: BulkIssueAttestationRequest,
) -> Result<Json<BulkIssueAttestationResponse>, AppError> {
    let caller = authenticate_caller(state, headers).await?;
    let Caller::Integrator {
        integrator_id,
        identity_id: subject_id,
    } = caller
    else {
        return Err(AppError::Forbidden);
    };

    let path_integrator_id = fetch_integrator_id_by_slug(state, slug).await?;
    if integrator_id != path_integrator_id {
        return Err(AppError::AchievementDefinitionForbidden);
    }

    if body.claims.is_empty() || body.claims.len() > MAX_BULK_CLAIMS {
        return Err(AppError::InvalidBulkAttestationRequest);
    }

    let idempotency_key = crate::idempotency::read_idempotency_key(headers);
    if let Some(key) = &idempotency_key {
        if let Some(cached) = crate::idempotency::find_cached::<BulkIssueAttestationResponse>(
            state,
            integrator_id,
            key,
            BULK_ISSUE_ATTESTATION_IDEMPOTENCY_ENDPOINT,
        )
        .await?
        {
            return Ok(Json(cached));
        }
    }

    let category = fetch_integrator_category(state, integrator_id).await?;
    if !route.allows(category) {
        return Err(AppError::ClaimVocabularyMismatch);
    }

    // The user's own consent, checked once for the whole call — every
    // claim in a bulk call shares the same subject/integrator/capability,
    // matching `issue_attestation`'s own single-claim check.
    require_capability(&caller, route.issue_capability(), state).await?;

    let claim_kind = category.claim_kind();
    let issuer_str = format!("{}:{}", category.as_str(), slug);

    // #365: the whole batch counts against the quota as one unit — a
    // bulk call that would cross the limit is rejected outright, never
    // partially applied claim-by-claim.
    let recent_writes = recent_issuer_subject_write_count(state, &issuer_str, subject_id).await?;
    guard_write_quota(recent_writes, body.claims.len() as i64)?;

    // Every claim's full achievement ref, in order — pure, no DB lookup —
    // exactly what the caller must have signed over, independent of
    // whether each referenced definition actually exists yet.
    let achievement_refs: Vec<String> = body
        .claims
        .iter()
        .map(|claim| {
            definition_ref(category, slug, &claim.key)
                .as_str()
                .to_string()
        })
        .collect();

    let signature_bytes = BASE64
        .decode(&body.signature)
        .map_err(|_| AppError::InvalidAttestationSignature)?;
    let issuer_keys = fetch_issuer_keys(state, integrator_id).await?;
    let now = OffsetDateTime::now_utc();

    let signing_bytes = bulk_attestation_signing_bytes(
        claim_kind,
        &issuer_str,
        IdentityId(subject_id),
        &achievement_refs,
    );
    let Authenticity::Authentic { .. } = verify_signature(
        &body.key_id.to_string(),
        &signing_bytes,
        &signature_bytes,
        now,
        &issuer_keys,
    ) else {
        return Err(AppError::InvalidAttestationSignature);
    };
    let signing_key = resolve_valid_signing_key(&issuer_keys, body.key_id, now)
        .expect("verify_signature already resolved this key successfully");

    let mut tx = state.pool.begin().await?;

    // Same belt-and-suspenders network-admission check `issue_attestation`
    // makes, once for the whole call — every claim writes under the same
    // issuer key, so there's nothing to admit per-claim.
    ensure_issuer_registered(
        &mut tx,
        state.chain.network_id(),
        &signing_key.public_key,
        &issuer_str,
    )
    .await?;

    let mut results = Vec::with_capacity(body.claims.len());
    for (claim, achievement_ref) in body.claims.iter().zip(achievement_refs.iter()) {
        // Per-claim definition existence/retirement — a per-item failure,
        // never aborting the rest of the batch (#495's own invariant). Any
        // other error (a real database failure) still propagates as a
        // whole-call failure, same as it would for a single issuance.
        let definition = match fetch_definition(state, integrator_id, &claim.key).await {
            Ok(row) => row,
            Err(AppError::AchievementDefinitionNotFound) => {
                results.push(BulkClaimResult::Failed {
                    key: claim.key.clone(),
                    code: "ACHIEVEMENT_DEFINITION_NOT_FOUND".to_string(),
                    error: "achievement definition not found".to_string(),
                });
                continue;
            }
            Err(other) => return Err(other),
        };
        if definition.retired_at.is_some() {
            results.push(BulkClaimResult::Failed {
                key: claim.key.clone(),
                code: "ATTESTATION_DEFINITION_RETIRED".to_string(),
                error: "achievement definition is retired".to_string(),
            });
            continue;
        }

        let attestation_id = Uuid::new_v4();
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
        .bind(&signature_bytes)
        .execute(&mut *tx)
        .await?;

        let event = ProtocolEvent {
            id: Uuid::new_v4(),
            kind: claim_kind_variant(
                claim_kind,
                ProtocolEventKindVariant::AchievementIssued,
                ProtocolEventKindVariant::MilestoneIssued,
            )
            .as_str()
            .to_string(),
            issuer: issuer_ref(category.as_str(), slug, &format!("{claim_kind}_issued")),
            subject: issuer_ref(
                "identity",
                &subject_id.to_string(),
                &format!("{claim_kind}_issued"),
            ),
            payload: serde_json::to_value(ClaimIssuedPayload {
                id: attestation_id,
                issuer: issuer_str.clone(),
                subject: subject_id,
                achievement: definition.id.clone(),
                evidence: claim.evidence.clone(),
                proof: ClaimProofPayload {
                    key_id: body.key_id,
                    algorithm: signing_key.algorithm.clone(),
                    bytes: body.signature.clone(),
                },
            })
            .expect("ClaimIssuedPayload should serialize"),
            timestamp: now,
            version: 1,
        };
        outbox::enqueue(&mut tx, &event).await?;

        results.push(BulkClaimResult::Issued {
            key: claim.key.clone(),
            attestation: AttestationResponse {
                id: attestation_id,
                issuer: issuer_str.clone(),
                subject: subject_id,
                achievement: achievement_ref.clone(),
                issued_at: now,
                proof: AttestationSignatureResponse {
                    key_id: body.key_id,
                    algorithm: signing_key.algorithm.clone(),
                    bytes: body.signature.clone(),
                },
            },
        });
    }

    tx.commit().await?;

    let response = BulkIssueAttestationResponse { results };

    if let Some(key) = &idempotency_key {
        crate::idempotency::store(
            state,
            integrator_id,
            key,
            BULK_ISSUE_ATTESTATION_IDEMPOTENCY_ENDPOINT,
            &response,
        )
        .await?;
    }

    Ok(Json(response))
}

/// `POST /integrations/{slug}/achievements/bulk-issue` (#495).
pub async fn bulk_issue_achievements(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(body): Json<BulkIssueAttestationRequest>,
) -> Result<Json<BulkIssueAttestationResponse>, AppError> {
    bulk_issue_attestation(&state, &headers, &slug, ClaimRoute::Achievements, body).await
}

/// `POST /integrations/{slug}/milestones/bulk-issue` (#495).
pub async fn bulk_issue_milestones(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(body): Json<BulkIssueAttestationRequest>,
) -> Result<Json<BulkIssueAttestationResponse>, AppError> {
    bulk_issue_attestation(&state, &headers, &slug, ClaimRoute::Milestones, body).await
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

    #[test]
    fn guard_write_quota_allows_up_to_the_limit() {
        let limit = write_quota_from_env();
        assert!(guard_write_quota(0, 1).is_ok());
        assert!(guard_write_quota(limit - 1, 1).is_ok());
        assert!(guard_write_quota(0, limit).is_ok());
    }

    #[test]
    fn guard_write_quota_rejects_crossing_the_limit() {
        let limit = write_quota_from_env();
        assert!(matches!(
            guard_write_quota(limit, 1),
            Err(AppError::AttestationWriteQuotaExceeded)
        ));
        assert!(matches!(
            guard_write_quota(0, limit + 1),
            Err(AppError::AttestationWriteQuotaExceeded)
        ));
    }
}
