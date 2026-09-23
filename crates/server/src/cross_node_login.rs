//! Cross-node login pending-request lifecycle and verification — the
//! server-side counterpart of
//! `avalon_protocol::cross_node_login::CrossNodeLoginGrant`. See that
//! module's doc comment for the wire shape and the security posture this
//! preserves.
//!
//! Structurally close to `crate::device_pairing`'s create/poll/approve
//! shape, but genuinely cross-node: approval never requires a live session
//! on *this* (the requesting) node at all, unlike same-node pairing
//! — the grant is signed and submitted from wherever the identity's own
//! signing key lives, which may be a browser tab that has never talked to
//! this node before.
//!
//! **Same-device fast path**: if the client attempting login already holds
//! the identity's signing key locally, it can skip `start`/`poll` entirely
//! and call [`submit`] directly with a grant it minted itself — the
//! `start`+poll dance below exists only for the cross-device case (an
//! unfamiliar browser, a console, a friend's machine).
//!
//! **Cross-shard verification fallback**: a fresh
//! `identity_signing_keys` lookup only ever finds a key this node already
//! has locally (authored or mirrored) — without a fallback, cross-node
//! login could never complete for an identity whose signing-key
//! projection isn't already replicated to the node being logged into,
//! which defeats a real part of what cross-node login exists to unlock. When the
//! local lookup misses, [`verify_grant`] falls back to the locator
//! (`crate::identity_locator::resolve`) plus cross-shard
//! fetch-and-verify (`crate::cross_shard_fetch::fetch_verified_entries`)
//! — see [`resolve_signing_key_cross_shard`]'s own doc comment for the
//! shard-id simplification this relies on.

use std::collections::HashMap;

use avalon_indexer::projections::identity_signing_keys;
use avalon_protocol::cross_node_login::CrossNodeLoginGrant;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use ed25519_dalek::VerifyingKey;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::auth::{generate_session_token, verify_event_signature};
use crate::error::AppError;
use crate::state::AppState;
use utoipa::ToSchema;

/// Identity-level signing keys (Layer 1, per
/// `docs/architecture/identity-aggregate-view.md`'s two-layer model)
/// typically live on one shared shard — using `"core"` here is a
/// documented, honest simplification, not a silent
/// assumption: the per-integrator `issuer_keys` trust mechanism
/// never resolves anything for `"core"` (it has no owning integrator by
/// construction), so [`core_shard_verify_keys`] supplies the real trust
/// anchor instead.
const IDENTITY_SIGNING_KEY_SHARD_ID: &str = "core";

const REQUEST_TTL_MINUTES: i64 = 10;
/// Same lifetime `device_pairing::approve_pairing` mints — a session minted
/// through cross-node login is an ordinary session in every respect.
const SESSION_LIFETIME_DAYS: i64 = 30;
const POLL_MIN_INTERVAL_SECONDS: i64 = 5;
/// Same unambiguous-glyph alphabet `device_pairing::USER_CODE_ALPHABET` uses.
const USER_CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const USER_CODE_LEN: usize = 8;
/// Same allowance `crate::continuation::CLOCK_SKEW_ALLOWANCE` documents.
const CLOCK_SKEW_ALLOWANCE: Duration = Duration::seconds(5);

fn generate_user_code() -> String {
    let mut rng = rand::rng();
    (0..USER_CODE_LEN)
        .map(|_| USER_CODE_ALPHABET[rng.random_range(0..USER_CODE_ALPHABET.len())] as char)
        .collect()
}

fn bearer_token(headers: &HeaderMap) -> Result<&str, AppError> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(AppError::Unauthorized)
}

/// This node's own advertised base URL — what a submitted grant's
/// `destination_base_url` is checked against. A node with no configured
/// `own_base_url` can't participate in cross-node login at all (there'd be
/// nothing for a remote approver to bind their approval to).
fn own_base_url(state: &AppState) -> Result<&str, AppError> {
    state.own_base_url.as_deref().ok_or(AppError::Unauthorized)
}

#[derive(Serialize, ToSchema)]
pub struct StartCrossNodeLoginResponse {
    pub request_code: String,
    pub user_code: String,
    pub requesting_context: String,
    pub expires_in: i64,
    pub poll_interval: i64,
}

/// `POST /auth/cross-node/start` — unauthenticated, called on the
/// requesting node. Mints an opaque `request_code` (known only to this
/// client and this server) and a short human-typeable `user_code` (shown
/// as a QR code / typed on the approving device), and stores a pending row
/// binding this node's own `base_url` into what the approver will
/// eventually sign over.
#[utoipa::path(
    post,
    path = "/auth/cross-node/start",
    tag = "identity",
    responses((status = 200, body = StartCrossNodeLoginResponse)),
)]
pub async fn start(
    State(state): State<AppState>,
) -> Result<Json<StartCrossNodeLoginResponse>, AppError> {
    const MAX_ATTEMPTS: u32 = 20;
    let base_url = own_base_url(&state)?.to_string();
    let requesting_context = base_url.clone();
    let now = OffsetDateTime::now_utc();
    let expires_at = now + Duration::minutes(REQUEST_TTL_MINUTES);

    for _ in 0..MAX_ATTEMPTS {
        let request_code = generate_session_token();
        let user_code = generate_user_code();

        let inserted = sqlx::query(
            r#"
            INSERT INTO cross_node_login_requests
                (request_code, user_code, status, requesting_base_url, expires_at)
            VALUES ($1, $2, 'pending', $3, $4)
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(&request_code)
        .bind(&user_code)
        .bind(&base_url)
        .bind(expires_at)
        .execute(&state.pool)
        .await?;

        if inserted.rows_affected() == 1 {
            return Ok(Json(StartCrossNodeLoginResponse {
                request_code,
                user_code,
                requesting_context,
                expires_in: REQUEST_TTL_MINUTES * 60,
                poll_interval: POLL_MIN_INTERVAL_SECONDS,
            }));
        }
    }
    Err(AppError::CrossNodeLoginRequestCodeGenerationFailed)
}

#[derive(Deserialize, utoipa::IntoParams)]
pub struct LookupQuery {
    pub user_code: String,
}

#[derive(Serialize, ToSchema)]
pub struct LookupCrossNodeLoginResponse {
    /// One of `pending`, `denied`, `expired`, `approved` — an approval
    /// screen only ever meaningfully acts on `pending`; the others let it
    /// show a clear "this code was already used/expired" state instead of
    /// a generic not-found.
    pub status: String,
    pub requesting_context: String,
    pub expires_in: i64,
    /// Whether this node — the one the identity is being asked to log
    /// into — resolves to a real, registered integrator (or a known
    /// network anchor for the default shard). See
    /// [`resolve_requester_verification`]'s own doc comment for exactly
    /// what "verified" means here. Rendered as a visual
    /// distinction, never a hard gate — an unverified requester still
    /// gets a prompt, just a clearly flagged one.
    pub integrator_verified: bool,
    /// A real registered display name, only ever present when
    /// `integrator_verified` is `true` — `null` otherwise, so the
    /// approval screen has no ambiguous "empty string vs. never checked"
    /// state to handle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// Resolves whether this node — reached exactly the way the approver's
/// `lookup` call reaches it, since this handler's own `state` always
/// describes the node it's running on — is a "verified" requester for
/// cross-node login purposes, and a real display name to show in place of
/// the raw `base_url` when it is. Two paths, deliberately reusing the
/// existing shard-trust mechanism rather than a second registry:
///
/// - **An owned shard** (`own_shard_id` shaped `"{namespace}:{owner}"`,
///   `game`/`app`/`service`): verified iff `owner` resolves to a real
///   integrator that currently holds an unrevoked `shard_settlement`-purpose
///   issuer key for exactly this shard — the exact same join
///   `crate::cross_shard::resolve_shard_verify_keys_from_db` already trusts
///   for STH signature verification, not a second, weaker check.
/// - **The default, unowned shard** (`"core"`, or anything else with no
///   `owner`): there's no integrator to check — verified iff this node's
///   own `own_base_url` is one of this network's real seed nodes
///   (`docs/trusted-networks.json`, via `avalon_protocol::network_trust::bundled_trust_anchors`)
///   — a shared network shard has no integrator, but a canonical anchor
///   node is still a real, checkable fact.
async fn resolve_requester_verification(state: &AppState) -> (bool, Option<String>) {
    if let Some((namespace, owner)) = state.own_shard_id.split_once(':') {
        if matches!(namespace, "game" | "app" | "service") {
            let row = sqlx::query(
                "SELECT i.name FROM integrators i \
                 JOIN issuer_keys ik ON ik.integrator_id = i.id \
                 WHERE i.slug = $1 AND i.category = $2 \
                   AND ik.purpose = 'shard_settlement' AND ik.revoked_at IS NULL \
                 LIMIT 1",
            )
            .bind(owner)
            .bind(namespace)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten();
            if let Some(row) = row {
                if let Ok(name) = row.try_get::<String, _>("name") {
                    return (true, Some(name));
                }
            }
        }
        return (false, None);
    }

    let Some(base_url) = state.own_base_url.as_deref() else {
        return (false, None);
    };
    let is_anchor = is_verified_seed_node(
        base_url,
        state.chain.network_id(),
        avalon_protocol::network_trust::bundled_trust_anchors(),
    );
    (is_anchor, is_anchor.then(|| "Avalon network".to_string()))
}

/// Pure resolution logic behind [`resolve_requester_verification`]'s
/// unowned-shard path, split out for direct unit testing — same "pure
/// function behind the real-data-reading wrapper" pattern
/// `crate::nodes::resolve_bootstrap_peers` already establishes, needed for
/// the same reason: this sandbox's own checked-in
/// `docs/trusted-networks.json` entry has an empty `seed_nodes` list (a
/// lone dev node with no anchor peer yet), so a live test against the real
/// bundled file can only ever exercise the "not an anchor" branch — a
/// controlled anchor list is the only way to exercise the "is an anchor"
/// branch at all.
fn is_verified_seed_node(
    own_base_url: &str,
    network_id: &str,
    anchors: &[avalon_protocol::network_trust::TrustAnchorEntry],
) -> bool {
    anchors
        .iter()
        .find(|entry| entry.network_id == network_id)
        .map(|entry| {
            entry
                .seed_nodes
                .iter()
                .any(|seed| seed.trim_end_matches('/') == own_base_url)
        })
        .unwrap_or(false)
}

/// `GET /auth/cross-node/lookup?user_code=...` — unauthenticated: the
/// Hub/mobile-hub approval screen has to
/// show real context before a human decides whether to approve, but `submit`/`deny` only
/// ever take a `user_code` with no read path to go with it. Deliberately
/// returns nothing beyond what's needed to render the prompt — never
/// `request_code` (the polling device's own bearer credential, not the
/// approver's business).
#[utoipa::path(
    get,
    path = "/auth/cross-node/lookup",
    tag = "identity",
    params(LookupQuery),
    responses((status = 200, body = LookupCrossNodeLoginResponse)),
)]
pub async fn lookup(
    State(state): State<AppState>,
    Query(query): Query<LookupQuery>,
) -> Result<Json<LookupCrossNodeLoginResponse>, AppError> {
    let now = OffsetDateTime::now_utc();
    let row = sqlx::query(
        "SELECT status, requesting_base_url, expires_at FROM cross_node_login_requests WHERE user_code = $1",
    )
    .bind(&query.user_code)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::CrossNodeLoginRequestNotFound)?;

    let mut status: String = row.try_get("status")?;
    let requesting_base_url: String = row.try_get("requesting_base_url")?;
    let expires_at: OffsetDateTime = row.try_get("expires_at")?;

    // Same lazy-expiry posture `poll`'s own handler already takes: nothing
    // proactively flips a stale `pending` row to `expired` on a schedule,
    // so a read has to reconcile it itself rather than trust the stored
    // status blindly.
    if status == "pending" && expires_at < now {
        status = "expired".to_string();
    }

    let (integrator_verified, display_name) = resolve_requester_verification(&state).await;

    Ok(Json(LookupCrossNodeLoginResponse {
        status,
        requesting_context: requesting_base_url,
        expires_in: (expires_at - now).whole_seconds().max(0),
        integrator_verified,
        display_name,
    }))
}

#[derive(Serialize, ToSchema)]
pub struct PollCrossNodeLoginResponse {
    /// One of `pending`, `slow_down`, `denied`, `expired`, `approved`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = "date-time")]
    pub expires_at: Option<OffsetDateTime>,
}

fn pending_status(status: &str) -> PollCrossNodeLoginResponse {
    PollCrossNodeLoginResponse {
        status: status.to_string(),
        token: None,
        expires_at: None,
    }
}

/// `POST /auth/cross-node/poll` — unauthenticated; the bearer token here is
/// the opaque `request_code`, not a session. Same single-use-on-approved
/// shape as `device_pairing::poll_pairing`.
#[utoipa::path(
    post,
    path = "/auth/cross-node/poll",
    tag = "identity",
    responses((status = 200, body = PollCrossNodeLoginResponse)),
)]
pub async fn poll(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PollCrossNodeLoginResponse>, AppError> {
    let request_code = bearer_token(&headers)?;
    let now = OffsetDateTime::now_utc();

    let row = sqlx::query(
        "SELECT status, expires_at, last_polled_at FROM cross_node_login_requests WHERE request_code = $1",
    )
    .bind(request_code)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::Unauthorized)?;

    let status: String = row.try_get("status")?;
    let expires_at: OffsetDateTime = row.try_get("expires_at")?;
    let last_polled_at: Option<OffsetDateTime> = row.try_get("last_polled_at")?;

    if status == "pending" && expires_at < now {
        sqlx::query(
            "UPDATE cross_node_login_requests SET status = 'expired' WHERE request_code = $1 AND status = 'pending'",
        )
        .bind(request_code)
        .execute(&state.pool)
        .await?;
        return Ok(Json(pending_status("expired")));
    }

    match status.as_str() {
        "expired" => Ok(Json(pending_status("expired"))),
        "denied" => Ok(Json(pending_status("denied"))),
        "pending" => {
            sqlx::query(
                "UPDATE cross_node_login_requests SET last_polled_at = $2 WHERE request_code = $1",
            )
            .bind(request_code)
            .bind(now)
            .execute(&state.pool)
            .await?;
            if let Some(last_polled_at) = last_polled_at {
                if now - last_polled_at < Duration::seconds(POLL_MIN_INTERVAL_SECONDS) {
                    return Ok(Json(pending_status("slow_down")));
                }
            }
            Ok(Json(pending_status("pending")))
        }
        "approved" => {
            let consumed = sqlx::query(
                r#"
                UPDATE cross_node_login_requests
                SET status = 'expired'
                WHERE request_code = $1 AND status = 'approved'
                RETURNING session_token
                "#,
            )
            .bind(request_code)
            .fetch_optional(&state.pool)
            .await?;

            let Some(consumed) = consumed else {
                return Ok(Json(pending_status("expired")));
            };
            let session_token: Option<String> = consumed.try_get("session_token")?;
            let Some(session_token) = session_token else {
                return Ok(Json(pending_status("expired")));
            };

            let session_row = sqlx::query("SELECT expires_at FROM sessions WHERE token = $1")
                .bind(&session_token)
                .fetch_optional(&state.pool)
                .await?;
            let Some(session_row) = session_row else {
                return Ok(Json(pending_status("expired")));
            };
            let session_expires_at: OffsetDateTime = session_row.try_get("expires_at")?;

            Ok(Json(PollCrossNodeLoginResponse {
                status: "approved".to_string(),
                token: Some(session_token),
                expires_at: Some(session_expires_at),
            }))
        }
        other => {
            debug_assert!(
                false,
                "unexpected cross_node_login_requests.status: {other}"
            );
            Ok(Json(pending_status("expired")))
        }
    }
}

#[derive(Deserialize, ToSchema)]
pub struct SubmitGrantRequest {
    /// Present for the cross-device flow: which pending `start`ed request
    /// this grant resolves. `None` for the same-device fast path, which
    /// mints a session directly with no pending row at all.
    #[serde(default)]
    pub user_code: Option<String>,
    pub grant: CrossNodeLoginGrant,
}

#[derive(Serialize, ToSchema)]
pub struct SubmitGrantResponse {
    /// Present only on the same-device fast path (`user_code: None`) — the
    /// cross-device path's session token is picked up via `poll`, same as
    /// `device_pairing::approve_pairing`, not returned here directly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = "date-time")]
    pub expires_at: Option<OffsetDateTime>,
}

/// Resolves the verify key for the cross-shard fetch of
/// `"core"`-shard data. The per-integrator `issuer_keys` mechanism
/// never resolves anything for `"core"`, so the trust anchor here is this
/// network's own pinned key from `docs/trusted-networks.json`
/// (`avalon_protocol::network_trust::bundled_trust_anchors`) — the exact same source
/// `resolve_requester_verification` already reuses for its
/// own `"core"`-shard trust path. Empty (not an error) when this network
/// has no bundled entry, or its `verify_key` doesn't parse — the cross-shard
/// fetch simply reports every `"core"`-shard STH as unverifiable in that
/// case, same as a genuinely unresolvable shard.
fn core_shard_verify_keys(network_id: &str) -> HashMap<String, VerifyingKey> {
    let mut keys = HashMap::new();
    if let Some(anchor) = avalon_protocol::network_trust::bundled_trust_anchors()
        .iter()
        .find(|entry| entry.network_id == network_id)
    {
        if let Some(key) = hex::decode(&anchor.verify_key)
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .and_then(|array| VerifyingKey::from_bytes(&array).ok())
        {
            keys.insert(IDENTITY_SIGNING_KEY_SHARD_ID.to_string(), key);
        }
    }
    keys
}

/// The real gap this closes: falls back to the locator plus
/// cross-shard fetch-and-verify when `signing_key_id` isn't in this
/// node's own local `identity_signing_keys` at all. Checks every candidate
/// location the locator returns, in order, stopping at the first that
/// yields a real, currently-unrevoked matching key — `None` once every
/// candidate has been tried (or the locator found none at all). Revocation
/// is checked per candidate against that same node's own
/// `identity.signing_key_revoked` history, the cross-shard equivalent of
/// local lookup's own `revoked_at IS NULL` condition.
async fn resolve_signing_key_cross_shard(
    state: &AppState,
    identity_id: Uuid,
    signing_key_id: Uuid,
) -> Option<Vec<u8>> {
    let locations = crate::identity_locator::resolve(state, identity_id).await;
    if locations.is_empty() {
        return None;
    }

    let verify_keys = core_shard_verify_keys(state.chain.network_id());
    let signing_key_id_str = signing_key_id.to_string();
    let added_subject = format!("identity:{identity_id}:self:signing_key_added");
    let revoked_subject = format!("identity:{identity_id}:self:signing_key_revoked");

    for base_url in locations {
        let Ok(added) = crate::cross_shard_fetch::fetch_verified_entries(
            &state.pool,
            state.chain.network_id(),
            IDENTITY_SIGNING_KEY_SHARD_ID,
            &base_url,
            &added_subject,
            &verify_keys,
        )
        .await
        else {
            continue;
        };
        let Some(matching) = added.iter().find(|entry| {
            entry.payload.get("signing_key_id").and_then(|v| v.as_str())
                == Some(signing_key_id_str.as_str())
        }) else {
            continue;
        };

        let revoked = crate::cross_shard_fetch::fetch_verified_entries(
            &state.pool,
            state.chain.network_id(),
            IDENTITY_SIGNING_KEY_SHARD_ID,
            &base_url,
            &revoked_subject,
            &verify_keys,
        )
        .await
        .unwrap_or_default();
        let is_revoked = revoked.iter().any(|entry| {
            entry.payload.get("signing_key_id").and_then(|v| v.as_str())
                == Some(signing_key_id_str.as_str())
        });
        if is_revoked {
            return None;
        }

        let Some(public_key_b64) = matching.payload.get("public_key").and_then(|v| v.as_str())
        else {
            continue;
        };
        let Ok(public_key) =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, public_key_b64)
        else {
            continue;
        };
        return Some(public_key);
    }
    None
}

/// Companion to [`resolve_signing_key_cross_shard`]: a verified signing key
/// alone isn't enough to actually mint a session here — `sessions.identity_id`
/// has a real `REFERENCES identities(id)` foreign key, and `GET /me` needs a
/// `profiles` row (`display_name`) to return anything at all. Best-effort —
/// every failure just leaves this node without a local stub, which only
/// matters for a *second* future call, not this one (`submit`'s caller
/// already has everything it needs from `resolve_signing_key_cross_shard`
/// alone). Deliberately never surfaces an error: a login that already
/// verified via a real signature and inclusion proof must not fail just
/// because, say, this node happens to already have an unrelated local
/// identity with the same `display_name` (the one real collision case here
/// — cross-shard `display_name` uniqueness isn't and can't be enforced
/// globally by a single node's unique index).
async fn provision_local_identity_stub(state: &AppState, identity_id: Uuid) {
    let already_local = sqlx::query("SELECT 1 FROM identities WHERE id = $1")
        .bind(identity_id)
        .fetch_optional(&state.pool)
        .await;
    if !matches!(already_local, Ok(None)) {
        return;
    }

    let locations = crate::identity_locator::resolve(state, identity_id).await;
    let verify_keys = core_shard_verify_keys(state.chain.network_id());
    let created_subject = format!("identity:{identity_id}:self:created");

    for base_url in locations {
        let Ok(created) = crate::cross_shard_fetch::fetch_verified_entries(
            &state.pool,
            state.chain.network_id(),
            IDENTITY_SIGNING_KEY_SHARD_ID,
            &base_url,
            &created_subject,
            &verify_keys,
        )
        .await
        else {
            continue;
        };
        let Some(display_name) = created
            .first()
            .and_then(|entry| entry.payload.get("display_name"))
            .and_then(|v| v.as_str())
        else {
            continue;
        };

        let Ok(mut tx) = state.pool.begin().await else {
            return;
        };
        if sqlx::query("INSERT INTO identities (id) VALUES ($1) ON CONFLICT DO NOTHING")
            .bind(identity_id)
            .execute(&mut *tx)
            .await
            .is_err()
        {
            return;
        }
        // `profiles_display_name_lower_idx` may reject this on a genuine
        // cross-shard name collision — left uncommitted in that case,
        // which is fine (see doc comment above).
        if sqlx::query(
            "INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2) \
             ON CONFLICT (identity_id) DO NOTHING",
        )
        .bind(identity_id)
        .bind(display_name)
        .execute(&mut *tx)
        .await
        .is_err()
        {
            return;
        }
        let _ = tx.commit().await;
        return;
    }
}

/// Verifies `grant` against `identity_signing_keys` (falling back to
/// [`resolve_signing_key_cross_shard`] when this node has no local copy —
/// see this module's own doc comment), this node's own `base_url`, and the
/// anti-replay nonce table, returning the identity it authenticates. Every
/// failure is [`AppError::Unauthorized`], same undifferentiated posture
/// `crate::continuation::verify`'s own doc comment already establishes for
/// the same reason (never reveal *why* a bearer credential didn't verify).
async fn verify_grant(state: &AppState, grant: &CrossNodeLoginGrant) -> Result<Uuid, AppError> {
    let now = OffsetDateTime::now_utc();
    if grant.expires_at < now || grant.issued_at > now + CLOCK_SKEW_ALLOWANCE {
        return Err(AppError::Unauthorized);
    }
    if grant.expires_at - grant.issued_at
        > Duration::seconds(avalon_protocol::cross_node_login::DEFAULT_TTL_SECONDS)
    {
        return Err(AppError::Unauthorized);
    }

    // Destination binding: a grant approved
    // for a different node must never verify here, even with a perfectly
    // valid signature and nonce.
    if grant.destination_base_url != own_base_url(state)? {
        return Err(AppError::Unauthorized);
    }

    let public_key =
        match identity_signing_keys::find_active_by_id(&state.pool, grant.signing_key_id).await? {
            Some(key) => {
                if key.identity_id != grant.identity_id {
                    return Err(AppError::Unauthorized);
                }
                key.public_key
            }
            // Not local — falls back to cross-shard resolution. A cross-shard-fetched entry
            // is already scoped to `grant.identity_id` by construction (it's
            // fetched from that exact identity's own `identity:{id}:...`
            // subject), so there's no separate identity-id cross-check to
            // repeat here the way the local path needs one.
            None => {
                let key =
                    resolve_signing_key_cross_shard(state, grant.identity_id, grant.signing_key_id)
                        .await
                        .ok_or(AppError::Unauthorized)?;
                // Needed before `submit` can insert into `sessions` (its
                // `identity_id` FK) or `GET /me` can return anything — see
                // this function's own doc comment.
                provision_local_identity_stub(state, grant.identity_id).await;
                key
            }
        };

    let signature_bytes = hex::decode(&grant.signature).map_err(|_| AppError::Unauthorized)?;
    if !verify_event_signature(&public_key, &grant.signing_bytes(), &signature_bytes) {
        return Err(AppError::Unauthorized);
    }

    sqlx::query("DELETE FROM consumed_cross_node_login_nonces WHERE expires_at < now()")
        .execute(&state.pool)
        .await?;
    let inserted = sqlx::query(
        "INSERT INTO consumed_cross_node_login_nonces (nonce, expires_at) VALUES ($1, $2) \
         ON CONFLICT DO NOTHING",
    )
    .bind(grant.nonce)
    .bind(grant.expires_at)
    .execute(&state.pool)
    .await?;
    if inserted.rows_affected() == 0 {
        return Err(AppError::Unauthorized);
    }

    Ok(grant.identity_id)
}

/// `POST /auth/cross-node/submit` — unauthenticated (the grant itself is
/// the credential). Mints an ordinary session, same mechanism
/// `device_pairing::approve_pairing` uses. With `user_code` present,
/// attaches the session to that pending request for the waiting client to
/// pick up via `poll`; with `user_code` absent (same-device fast path),
/// returns the session token directly.
#[utoipa::path(
    post,
    path = "/auth/cross-node/submit",
    tag = "identity",
    request_body = SubmitGrantRequest,
    responses((status = 200, body = SubmitGrantResponse)),
)]
pub async fn submit(
    State(state): State<AppState>,
    Json(body): Json<SubmitGrantRequest>,
) -> Result<Json<SubmitGrantResponse>, AppError> {
    let identity_id = verify_grant(&state, &body.grant).await?;

    let token = generate_session_token();
    let session_expires_at = OffsetDateTime::now_utc() + Duration::days(SESSION_LIFETIME_DAYS);

    let mut tx = state.pool.begin().await?;
    sqlx::query("INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)")
        .bind(&token)
        .bind(identity_id)
        .bind(session_expires_at)
        .execute(&mut *tx)
        .await?;

    let Some(user_code) = body.user_code else {
        tx.commit().await?;
        return Ok(Json(SubmitGrantResponse {
            token: Some(token),
            expires_at: Some(session_expires_at),
        }));
    };

    let resolved = sqlx::query(
        r#"
        UPDATE cross_node_login_requests
        SET status = 'approved', identity_id = $2, session_token = $3
        WHERE user_code = $1 AND status = 'pending' AND expires_at > now()
        "#,
    )
    .bind(&user_code)
    .bind(identity_id)
    .bind(&token)
    .execute(&mut *tx)
    .await?;
    if resolved.rows_affected() == 0 {
        // Resolved (approved/denied/expired) by a concurrent request, or
        // the code never matched a pending row at all — either way this
        // freshly-minted session must not be left dangling unattached.
        return Err(AppError::CrossNodeLoginRequestNotFound);
    }

    tx.commit().await?;
    Ok(Json(SubmitGrantResponse {
        token: None,
        expires_at: None,
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct UserCodeRequest {
    pub user_code: String,
}

#[derive(Serialize, ToSchema)]
pub struct DenyResponse {
    pub status: String,
}

/// `POST /auth/cross-node/deny` — deliberately unauthenticated, unlike
/// `device_pairing::deny_pairing`. That module's approver always already
/// holds a session *on the same node* the pairing was started on, so
/// requiring it there is free; here the approver's session (if any) lives
/// wherever the identity's signing key does, which is routinely a
/// *different* node than the one this request was started on — requiring
/// local auth would make denial impossible for the exact case cross-node
/// login exists to handle. Safe to leave unauthenticated regardless: a
/// denial grants nothing, so the worst case of a guessed `user_code` (8
/// chars from a 32-symbol alphabet) is griefing one's own pending request,
/// not a security bypass.
#[utoipa::path(
    post,
    path = "/auth/cross-node/deny",
    tag = "identity",
    request_body = UserCodeRequest,
    responses((status = 200, body = DenyResponse)),
)]
pub async fn deny(
    State(state): State<AppState>,
    Json(body): Json<UserCodeRequest>,
) -> Result<Json<DenyResponse>, AppError> {
    let resolved = sqlx::query(
        r#"
        UPDATE cross_node_login_requests
        SET status = 'denied'
        WHERE user_code = $1 AND status = 'pending' AND expires_at > now()
        "#,
    )
    .bind(&body.user_code)
    .execute(&state.pool)
    .await?;
    if resolved.rows_affected() == 0 {
        return Err(AppError::CrossNodeLoginRequestNotFound);
    }

    Ok(Json(DenyResponse {
        status: "denied".to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_code_uses_only_the_unambiguous_alphabet() {
        for _ in 0..200 {
            let code = generate_user_code();
            assert_eq!(code.len(), USER_CODE_LEN);
            for c in code.chars() {
                assert!(USER_CODE_ALPHABET.contains(&(c as u8)));
                assert!(!"0O1IL".contains(c));
            }
        }
    }

    fn anchor(
        network_id: &str,
        seed_nodes: Vec<String>,
    ) -> avalon_protocol::network_trust::TrustAnchorEntry {
        avalon_protocol::network_trust::TrustAnchorEntry {
            label: network_id.to_string(),
            network_id: network_id.to_string(),
            verify_key: "deadbeef".to_string(),
            signing_key_id: "k1".to_string(),
            server_url: None,
            environment: avalon_protocol::network_trust::NetworkEnvironment::Dev,
            seed_nodes,
            notes: None,
        }
    }

    #[test]
    fn a_node_matching_a_real_seed_entry_is_a_verified_anchor() {
        let anchors = vec![anchor(
            "avalon-mainnet-1",
            vec!["https://anchor-a.example".to_string()],
        )];
        assert!(is_verified_seed_node(
            "https://anchor-a.example",
            "avalon-mainnet-1",
            &anchors,
        ));
    }

    #[test]
    fn a_trailing_slash_on_either_side_still_matches() {
        let anchors = vec![anchor(
            "avalon-mainnet-1",
            vec!["https://anchor-a.example/".to_string()],
        )];
        assert!(is_verified_seed_node(
            "https://anchor-a.example",
            "avalon-mainnet-1",
            &anchors,
        ));
    }

    #[test]
    fn a_node_not_in_the_seed_list_is_not_an_anchor() {
        let anchors = vec![anchor(
            "avalon-mainnet-1",
            vec!["https://anchor-a.example".to_string()],
        )];
        assert!(!is_verified_seed_node(
            "https://some-other-node.example",
            "avalon-mainnet-1",
            &anchors,
        ));
    }

    #[test]
    fn a_network_with_no_matching_anchors_entry_is_never_an_anchor() {
        let anchors = vec![anchor(
            "some-other-network",
            vec!["https://x.example".to_string()],
        )];
        assert!(!is_verified_seed_node(
            "https://x.example",
            "avalon-mainnet-1",
            &anchors,
        ));
    }

    #[test]
    fn an_empty_seed_list_is_never_an_anchor() {
        let anchors = vec![anchor("avalon-mainnet-1", vec![])];
        assert!(!is_verified_seed_node(
            "https://anchor-a.example",
            "avalon-mainnet-1",
            &anchors,
        ));
    }
}
