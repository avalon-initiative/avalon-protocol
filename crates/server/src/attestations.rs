//! Attestation reads and revocation (issues #33 and #85, implementing ADR
//! #76's "authentic, valid, recognized are three separate questions"
//! model and #81's decided revocation mechanics). `GET
//! /attestations/{id}` is a public, unauthenticated read — same visibility
//! level `integrators::get_integrator`/`achievements::list_achievement_definitions`
//! already use — returning an attestation with its computed authenticity
//! ([`avalon_chain::attestations::verify_authenticity`]) and validity
//! (`avalon_protocol::achievements::validity`, now revocation-aware).
//!
//! **Recognition is deliberately absent from this response.** Whether a
//! *specific* consumer recognizes a claim is that consumer's own policy
//! evaluation (`avalon_protocol::achievements::recognize`), never a
//! boolean this endpoint computes — the same claim is `Authentic`/`Valid`
//! for every observer, but "recognized" only makes sense relative to one
//! consumer's own `TrustRelationship`. Publishing/serving an integrator's own
//! declared recognition policy (the ticket's `PUT
//! /integrations/{slug}/recognition`) is deferred, not built in this pass — see
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

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{Postgres, QueryBuilder, Row};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use avalon_chain::attestations::{verify_authenticity, verify_signature, Authenticity};
use avalon_protocol::achievements::validity as compute_validity;
use avalon_protocol::achievements::{attestation_status_at, revocation_signing_bytes};
use avalon_protocol::ids::AttestationId;
use avalon_protocol::integrators::{IntegratorCategory, IntegratorStatus, IssuerKey};
use avalon_protocol::revocation::RevocationReasonCode;

use avalon_protocol::event_payloads::ClaimRevokedPayload;
use avalon_protocol::events::{ProtocolEvent, ProtocolEventKindVariant};

use crate::achievements::claim_kind_variant;

use crate::error::AppError;
use crate::integrators::{
    authenticate_integrator, fetch_integrator_category, fetch_integrator_category_and_status_batch,
    fetch_integrator_status, fetch_issuer_keys, fetch_issuer_keys_batch, issuer_ref,
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

#[derive(Serialize, ToSchema)]
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

#[derive(Serialize, ToSchema)]
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

#[derive(Serialize, ToSchema)]
pub struct AttestationProofResponse {
    pub key_id: String,
    pub algorithm: String,
}

/// One entry in an attestation's history — `"issued"` always, plus
/// `"revoked"` if a revocation entry exists (#85). Reinstatement/
/// supersession entries would append here too, once either exists.
#[derive(Serialize, ToSchema)]
pub struct AttestationHistoryEntry {
    pub event: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub at: OffsetDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct AttestationReadResponse {
    pub id: Uuid,
    pub issuer: String,
    pub subject: Uuid,
    pub achievement: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub issued_at: OffsetDateTime,
    pub proof: AttestationProofResponse,
    pub authenticity: AuthenticityResponse,
    pub validity: ValidityResponse,
    pub history: Vec<AttestationHistoryEntry>,
    // Deliberately no `recognition` field — see module doc comment.
}

/// `GET /attestations/{id}` (#33) — public, unauthenticated.
#[utoipa::path(
    get,
    path = "/attestations/{id}",
    tag = "achievements",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = AttestationReadResponse)),
)]
pub async fn get_attestation(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<AttestationReadResponse>, AppError> {
    let row = sqlx::query(
        "SELECT id, integrator_id, issuer, subject, achievement, issued_at, \
                proof_key_id, proof_algorithm, proof_bytes \
         FROM achievement_attestations WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::AttestationNotFound)?;

    Ok(Json(build_attestation_response(&state, row).await?))
}

/// Everything [`assemble_attestation_response`] needs from an
/// `achievement_attestations` row, decoupled from how it was fetched — the
/// same decomposition `authz.rs`'s `BindingFacts` uses to keep the
/// assembly logic pure and testable without a database.
struct AttestationRowData {
    id: Uuid,
    integrator_id: Uuid,
    issuer: String,
    subject: Uuid,
    achievement: String,
    issued_at: OffsetDateTime,
    proof_key_id: Uuid,
    proof_algorithm: String,
    proof_bytes: Vec<u8>,
}

fn parse_attestation_row(row: &PgRow) -> Result<AttestationRowData, AppError> {
    Ok(AttestationRowData {
        id: row.try_get("id")?,
        integrator_id: row.try_get("integrator_id")?,
        issuer: row.try_get("issuer")?,
        subject: row.try_get("subject")?,
        achievement: row.try_get("achievement")?,
        issued_at: row.try_get("issued_at")?,
        proof_key_id: row.try_get("proof_key_id")?,
        proof_algorithm: row.try_get("proof_algorithm")?,
        proof_bytes: row.try_get("proof_bytes")?,
    })
}

#[derive(Clone)]
struct RevocationData {
    revoked_at: OffsetDateTime,
    reason_code: RevocationReasonCode,
    reason: String,
}

fn revocation_data_from_row(row: &PgRow) -> Result<RevocationData, AppError> {
    let reason_code: String = row.try_get("reason_code")?;
    Ok(RevocationData {
        revoked_at: row.try_get("revoked_at")?,
        reason_code: RevocationReasonCode::from(reason_code),
        reason: row.try_get("reason")?,
    })
}

/// The pure assembly step every attestation read (single or batched) goes
/// through once its inputs are in hand — no I/O here, so this is the piece
/// a future test can exercise directly without a database.
fn assemble_attestation_response(
    data: AttestationRowData,
    category: IntegratorCategory,
    status: IntegratorStatus,
    issuer_keys: &[IssuerKey],
    revocation: Option<RevocationData>,
) -> Result<AttestationReadResponse, AppError> {
    let attestation = avalon_protocol::achievements::AchievementAttestation {
        id: avalon_protocol::ids::AttestationId(data.id),
        issuer: match category {
            IntegratorCategory::Game => avalon_protocol::achievements::Issuer::Game(
                avalon_protocol::ids::IntegratorId(data.integrator_id),
            ),
            IntegratorCategory::App => avalon_protocol::achievements::Issuer::App(
                avalon_protocol::ids::IntegratorId(data.integrator_id),
            ),
            IntegratorCategory::Service => avalon_protocol::achievements::Issuer::Service(
                avalon_protocol::ids::IntegratorId(data.integrator_id),
            ),
        },
        subject: avalon_protocol::ids::IdentityId(data.subject),
        achievement: global_id_from_str(&data.achievement).ok_or(AppError::AttestationNotFound)?,
        issued_at: data.issued_at,
        proof: avalon_protocol::achievements::Signature {
            key_id: data.proof_key_id.to_string(),
            algorithm: data.proof_algorithm.clone(),
            bytes: data.proof_bytes,
        },
    };

    let authenticity = verify_authenticity(
        &attestation,
        category.claim_kind(),
        &data.issuer,
        issuer_keys,
    );

    let revoked_at = revocation.as_ref().map(|r| r.revoked_at);
    let now = OffsetDateTime::now_utc();
    let attestation_status = attestation_status_at(revoked_at, now);
    let validity = compute_validity(status, attestation_status);

    let mut history = vec![AttestationHistoryEntry {
        event: "issued".to_string(),
        at: data.issued_at,
        reason_code: None,
        reason: None,
    }];
    if let Some(r) = revocation {
        history.push(AttestationHistoryEntry {
            event: "revoked".to_string(),
            at: r.revoked_at,
            reason_code: Some(r.reason_code.to_string()),
            reason: Some(r.reason),
        });
    }

    Ok(AttestationReadResponse {
        id: data.id,
        issuer: data.issuer,
        subject: data.subject,
        achievement: data.achievement,
        issued_at: data.issued_at,
        proof: AttestationProofResponse {
            key_id: data.proof_key_id.to_string(),
            algorithm: data.proof_algorithm,
        },
        authenticity: authenticity.into(),
        validity: validity.into(),
        history,
    })
}

async fn build_attestation_response(
    state: &AppState,
    row: PgRow,
) -> Result<AttestationReadResponse, AppError> {
    let data = parse_attestation_row(&row)?;
    let category = fetch_integrator_category(state, data.integrator_id).await?;
    let status = fetch_integrator_status(state, data.integrator_id).await?;
    let issuer_keys = fetch_issuer_keys(state, data.integrator_id).await?;

    let revocation_row = sqlx::query(
        "SELECT revoked_at, reason_code, reason FROM attestation_revocations \
         WHERE attestation_id = $1",
    )
    .bind(data.id)
    .fetch_optional(&state.pool)
    .await?;
    let revocation = revocation_row
        .as_ref()
        .map(revocation_data_from_row)
        .transpose()?;

    assemble_attestation_response(data, category, status, &issuer_keys, revocation)
}

const DEFAULT_ACHIEVEMENTS_PAGE_SIZE: i64 = 50;
const MAX_ACHIEVEMENTS_PAGE_SIZE: i64 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimKindFilter {
    Achievement,
    Milestone,
}

impl ClaimKindFilter {
    fn parse(raw: Option<&str>) -> Result<Option<ClaimKindFilter>, AppError> {
        Ok(match raw {
            None => None,
            Some("achievement") => Some(ClaimKindFilter::Achievement),
            Some("milestone") => Some(ClaimKindFilter::Milestone),
            Some(_) => return Err(AppError::InvalidAchievementsListQuery),
        })
    }
}

#[derive(Deserialize, utoipa::IntoParams)]
pub struct ListMyAchievementsQuery {
    /// Restrict to one issuer, by its `integrators.id` — the actual
    /// indexed foreign key `achievement_attestations.integrator_id` names,
    /// rather than parsing the `issuer` column's `"<category>:<slug>"`
    /// wire string back apart.
    pub integrator_id: Option<Uuid>,
    /// `"achievement"` (Game-category issuers) or `"milestone"` (App/
    /// Service), matching `IntegratorCategory::claim_kind()`.
    pub claim_kind: Option<String>,
    /// Cursor: an attestation id already seen by the caller. Results are
    /// the next page strictly older than it (`issued_at` desc, `id` as
    /// tiebreak) — same `before`/`limit` shape
    /// `guild_messages::ListMessagesQuery` and
    /// `integrators::ListIntegratorsQuery` already established.
    pub before: Option<Uuid>,
    pub limit: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct ListMyAchievementsResponse {
    pub achievements: Vec<AttestationReadResponse>,
    /// `Some(id)` when another page exists — pass it back as `before=` to
    /// fetch it. `None` means this was the last page.
    pub next_cursor: Option<Uuid>,
}

/// Builds the `GET /me/achievements` query — split out from
/// [`list_my_achievements`] so the filter/pagination shape can be
/// unit-tested (via [`sqlx::QueryBuilder::sql`]) without a live Postgres
/// connection, same pattern `integrators::build_integrators_list_query`
/// already established. Only joins `integrators` when `claim_kind` is
/// actually filtered on — the common, unfiltered case stays the same
/// single-table scan it always was.
fn build_my_achievements_query(
    subject: Uuid,
    integrator_id: Option<Uuid>,
    claim_kind: Option<ClaimKindFilter>,
    before: Option<Uuid>,
    limit: i64,
) -> QueryBuilder<Postgres> {
    let needs_join = claim_kind.is_some();
    let mut builder: QueryBuilder<Postgres> = QueryBuilder::new(if needs_join {
        "SELECT a.id, a.integrator_id, a.issuer, a.subject, a.achievement, a.issued_at, \
                a.proof_key_id, a.proof_algorithm, a.proof_bytes \
         FROM achievement_attestations a JOIN integrators i ON i.id = a.integrator_id \
         WHERE a.subject = "
    } else {
        "SELECT id, integrator_id, issuer, subject, achievement, issued_at, \
                proof_key_id, proof_algorithm, proof_bytes \
         FROM achievement_attestations WHERE subject = "
    });
    builder.push_bind(subject);

    let col = if needs_join { "a." } else { "" };

    if let Some(integrator_id) = integrator_id {
        builder.push(format!(" AND {col}integrator_id = "));
        builder.push_bind(integrator_id);
    }

    match claim_kind {
        Some(ClaimKindFilter::Achievement) => {
            builder.push(" AND i.category = ");
            builder.push_bind("game".to_string());
        }
        Some(ClaimKindFilter::Milestone) => {
            builder.push(" AND i.category IN (");
            builder.push_bind("app".to_string());
            builder.push(", ");
            builder.push_bind("service".to_string());
            builder.push(")");
        }
        None => {}
    }

    if let Some(cursor_id) = before {
        builder.push(format!(
            " AND ({col}issued_at, {col}id) < (SELECT issued_at, id \
              FROM achievement_attestations WHERE id = "
        ));
        builder.push_bind(cursor_id);
        builder.push(" AND subject = ");
        builder.push_bind(subject);
        builder.push(")");
    }

    builder.push(format!(
        " ORDER BY {col}issued_at DESC, {col}id DESC LIMIT "
    ));
    // Fetch one extra row past the page size, purely to know whether a
    // next page exists — same convention
    // `integrators::build_integrators_list_query` uses.
    builder.push_bind(limit + 1);

    builder
}

/// `GET /me/achievements?integrator_id=&claim_kind=&before=&limit=` (#34,
/// paginated/filtered per #377) — bearer-authenticated as the reading
/// identity, returning that identity's own attestation history (every
/// issuer, active and revoked alike): the identity reading its own
/// record, not a per-consumer trust question, so no additional
/// authorization beyond "this is genuinely you" is needed — matching
/// `GET /attestations/{id}`'s own "authenticity/validity are facts, never
/// gated behind a specific issuer's permission" posture. Also the read
/// path `crates/sdk/src/achievements.rs::Session::achievements` (#34) and
/// the Hub's achievements view (#35) are designed against.
///
/// The N+1 `build_attestation_response` had (one integrator-category, one
/// integrator-status, one issuer-keys, one revocation query — *per row*)
/// is fixed here by batching all four lookups across the whole page: a
/// fixed four queries regardless of how many attestations are on the
/// page, not `1 + 4*page_size`.
#[utoipa::path(
    get,
    path = "/me/achievements",
    tag = "achievements",
    params(ListMyAchievementsQuery),
    responses((status = 200, body = ListMyAchievementsResponse)),
)]
pub async fn list_my_achievements(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListMyAchievementsQuery>,
) -> Result<Json<ListMyAchievementsResponse>, AppError> {
    let identity_id = crate::handlers::authenticate(&state, &headers).await?;
    let claim_kind = ClaimKindFilter::parse(query.claim_kind.as_deref())?;
    let limit = query
        .limit
        .unwrap_or(DEFAULT_ACHIEVEMENTS_PAGE_SIZE)
        .clamp(1, MAX_ACHIEVEMENTS_PAGE_SIZE);

    let mut builder = build_my_achievements_query(
        identity_id,
        query.integrator_id,
        claim_kind,
        query.before,
        limit,
    );
    let rows = builder.build().fetch_all(&state.pool).await?;
    let has_more = rows.len() as i64 > limit;

    let page: Vec<AttestationRowData> = rows
        .iter()
        .take(limit as usize)
        .map(parse_attestation_row)
        .collect::<Result<_, _>>()?;

    let integrator_ids: Vec<Uuid> = {
        let mut ids: Vec<Uuid> = page.iter().map(|d| d.integrator_id).collect();
        ids.sort();
        ids.dedup();
        ids
    };
    let attestation_ids: Vec<Uuid> = page.iter().map(|d| d.id).collect();

    let category_status =
        fetch_integrator_category_and_status_batch(&state, &integrator_ids).await?;
    let issuer_keys_by_integrator = fetch_issuer_keys_batch(&state, &integrator_ids).await?;
    let revocations = fetch_revocations_batch(&state, &attestation_ids).await?;

    // Cursor must walk the real underlying row order regardless of #534
    // filtering below — computed from the page as actually fetched, not
    // from whatever survives the visibility filter, so a page containing
    // only hidden-reason revocations still advances `before=` correctly
    // instead of re-fetching the same page forever.
    let last_fetched_id = page.last().map(|d| d.id);

    let mut achievements = Vec::with_capacity(page.len());
    for data in page {
        // A row here always has a matching `integrators` row — the
        // foreign key enforces it — so the `Game`/`Active` fallback is
        // unreachable in practice, same "can't actually happen" posture
        // `fetch_integrator_category`'s own default takes.
        let (category, status) = category_status
            .get(&data.integrator_id)
            .copied()
            .unwrap_or((IntegratorCategory::Game, IntegratorStatus::Active));
        let empty_keys: Vec<IssuerKey> = Vec::new();
        let issuer_keys = issuer_keys_by_integrator
            .get(&data.integrator_id)
            .unwrap_or(&empty_keys);
        let revocation = revocations.get(&data.id).cloned();
        // Issue #534: a revocation coded with a reason that "hides after
        // revocation" (e.g. a developer mistake, never a real fact about
        // the subject) drops the claim from this current-state listing
        // entirely — raw ledger history is unaffected either way, this
        // only changes what this one projection-style read surfaces.
        let hidden = revocation
            .as_ref()
            .is_some_and(|r| r.reason_code.hides_after_revocation());
        let response =
            assemble_attestation_response(data, category, status, issuer_keys, revocation)?;
        if !hidden {
            achievements.push(response);
        }
    }

    let next_cursor = if has_more { last_fetched_id } else { None };

    Ok(Json(ListMyAchievementsResponse {
        achievements,
        next_cursor,
    }))
}

/// The batched sibling of the per-attestation revocation lookup
/// `build_attestation_response` still does — one query for every id in
/// `attestation_ids` instead of one query per id, same reasoning as
/// `integrators::fetch_issuer_keys_batch`.
async fn fetch_revocations_batch(
    state: &AppState,
    attestation_ids: &[Uuid],
) -> Result<HashMap<Uuid, RevocationData>, AppError> {
    if attestation_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query(
        "SELECT attestation_id, revoked_at, reason_code, reason FROM attestation_revocations \
         WHERE attestation_id = ANY($1)",
    )
    .bind(attestation_ids)
    .fetch_all(&state.pool)
    .await?;

    let mut result = HashMap::with_capacity(rows.len());
    for row in &rows {
        let attestation_id: Uuid = row.try_get("attestation_id")?;
        result.insert(attestation_id, revocation_data_from_row(row)?);
    }
    Ok(result)
}

#[derive(Deserialize, ToSchema)]
pub struct RevokeAttestationRequest {
    pub key_id: Uuid,
    /// Standard-base64-encoded detached Ed25519 signature over
    /// [`revocation_signing_bytes`].
    pub signature: String,
    /// Issue #534: a real, extensible vocabulary (`RevocationReasonCode`),
    /// not a free-text string — see that type's own doc comment. Still
    /// deserializes from a plain JSON string, so no wire-format change
    /// for existing callers; an unrecognized code decodes to `Other`
    /// rather than a request error. Serializes/deserializes as a plain
    /// string via hand-written `serde` impls, so it has no `ToSchema` of
    /// its own — represented here as `String`.
    #[schema(value_type = String)]
    pub reason_code: RevocationReasonCode,
    pub reason: String,
}

#[derive(Serialize, ToSchema)]
pub struct RevocationResponse {
    pub attestation_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub revoked_at: OffsetDateTime,
    #[schema(value_type = String)]
    pub reason_code: RevocationReasonCode,
    pub reason: String,
}

/// `POST /attestations/{id}/revoke` (#85). Only the attestation's original
/// issuer may revoke it — authenticated via the same challenge-response
/// scheme every issuer-credentialed endpoint uses, plus (like issuance) an
/// independently-checked embedded signature over
/// [`revocation_signing_bytes`], so the revocation record itself carries
/// cryptographic proof of who authorized it, not just an HTTP-layer claim.
#[utoipa::path(
    post,
    path = "/attestations/{id}/revoke",
    tag = "achievements",
    params(("id" = Uuid, Path)),
    request_body = RevokeAttestationRequest,
    responses((status = 200, body = RevocationResponse)),
)]
pub async fn revoke_attestation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<RevokeAttestationRequest>,
) -> Result<Json<RevocationResponse>, AppError> {
    let row = sqlx::query(
        "SELECT integrator_id, issuer, achievement FROM achievement_attestations WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::AttestationNotFound)?;
    let integrator_id: Uuid = row.try_get("integrator_id")?;
    let issuer: String = row.try_get("issuer")?;

    // Only the attestation's own issuer — never a different issuer, never
    // the node operator, never the subject.
    let caller_integrator_id = authenticate_integrator(&state, &headers).await?;
    if caller_integrator_id != integrator_id {
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

    let category = fetch_integrator_category(&state, integrator_id).await?;
    let claim_kind = category.claim_kind();
    let signature_bytes = BASE64
        .decode(&body.signature)
        .map_err(|_| AppError::InvalidAttestationSignature)?;
    let signing_bytes = revocation_signing_bytes(
        claim_kind,
        &issuer,
        AttestationId(id),
        body.reason_code.as_str(),
    );

    let issuer_keys = fetch_issuer_keys(&state, integrator_id).await?;
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
    .bind(body.reason_code.as_str())
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
        kind: claim_kind_variant(
            claim_kind,
            ProtocolEventKindVariant::AchievementRevoked,
            ProtocolEventKindVariant::MilestoneRevoked,
        )
        .as_str()
        .to_string(),
        issuer: issuer_ref(category.as_str(), slug, &format!("{claim_kind}_revoked")),
        subject: issuer_ref(
            "attestation",
            &id.to_string(),
            &format!("{claim_kind}_revoked"),
        ),
        payload: serde_json::to_value(ClaimRevokedPayload {
            id: revocation_id,
            attestation_id: id,
            issuer: issuer.clone(),
            reason_code: body.reason_code.clone(),
            reason: body.reason.clone(),
        })
        .expect("ClaimRevokedPayload should serialize"),
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

#[cfg(test)]
mod tests {
    //! Pure-logic checks for #377's filter/pagination query shape and
    //! `claim_kind` parsing — no live Postgres needed, same pattern
    //! `integrators::build_integrators_list_query`'s own tests already
    //! established.

    use super::*;

    #[test]
    fn claim_kind_parses_achievement_and_milestone() {
        assert_eq!(
            ClaimKindFilter::parse(Some("achievement")).unwrap(),
            Some(ClaimKindFilter::Achievement)
        );
        assert_eq!(
            ClaimKindFilter::parse(Some("milestone")).unwrap(),
            Some(ClaimKindFilter::Milestone)
        );
        assert_eq!(ClaimKindFilter::parse(None).unwrap(), None);
    }

    #[test]
    fn claim_kind_rejects_anything_else() {
        assert!(matches!(
            ClaimKindFilter::parse(Some("quest")),
            Err(AppError::InvalidAchievementsListQuery)
        ));
    }

    #[test]
    fn no_filters_stays_a_single_table_scan() {
        let builder = build_my_achievements_query(Uuid::new_v4(), None, None, None, 50);
        let sql = builder.sql();
        let sql = sql.as_str();
        assert!(sql.contains("FROM achievement_attestations WHERE subject ="));
        assert!(!sql.contains("JOIN integrators"));
    }

    #[test]
    fn integrator_filter_adds_a_column_check_without_joining() {
        let builder =
            build_my_achievements_query(Uuid::new_v4(), Some(Uuid::new_v4()), None, None, 50);
        let sql = builder.sql();
        let sql = sql.as_str();
        assert!(sql.contains("AND integrator_id ="));
        assert!(!sql.contains("JOIN integrators"));
    }

    #[test]
    fn claim_kind_filter_joins_integrators_and_checks_category() {
        let achievement = build_my_achievements_query(
            Uuid::new_v4(),
            None,
            Some(ClaimKindFilter::Achievement),
            None,
            50,
        );
        let sql = achievement.sql();
        let sql = sql.as_str();
        assert!(sql.contains("JOIN integrators i ON i.id = a.integrator_id"));
        assert!(sql.contains("AND i.category ="));

        let milestone = build_my_achievements_query(
            Uuid::new_v4(),
            None,
            Some(ClaimKindFilter::Milestone),
            None,
            50,
        );
        assert!(milestone.sql().as_str().contains("AND i.category IN ("));
    }

    #[test]
    fn cursor_adds_keyset_pagination_clause() {
        let builder =
            build_my_achievements_query(Uuid::new_v4(), None, None, Some(Uuid::new_v4()), 50);
        assert!(builder.sql().as_str().contains(
            "AND (issued_at, id) < (SELECT issued_at, id \
              FROM achievement_attestations WHERE id ="
        ));
    }

    #[test]
    fn no_cursor_means_no_keyset_pagination_clause() {
        let builder = build_my_achievements_query(Uuid::new_v4(), None, None, None, 50);
        assert!(!builder.sql().as_str().contains("WHERE id ="));
    }

    #[test]
    fn limit_fetches_one_extra_row_to_detect_a_next_page() {
        let builder = build_my_achievements_query(Uuid::new_v4(), None, None, None, 50);
        assert!(builder.sql().as_str().contains(" LIMIT "));
    }
}
