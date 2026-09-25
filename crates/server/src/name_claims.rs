//! `POST /shards/{self_certifying_id}/name-claims`: a shard operator submits
//! a signed [`NameBindingClaim`] and this node fetches and checks its domain
//! proof (see `avalon_protocol::domain_proof`), recording the result if it
//! checks out. `GET /shards/name/{name}` and
//! `GET /shards/{self_certifying_id}/name-claims` are the corresponding read
//! paths.
//!
//! This is a parallel, optional naming layer for self-certifying
//! (`node:<key-hash>`) shard ids — it never touches, and is never consulted
//! by, the existing registry-based `game:<slug>`/`issuer.key_added`
//! resolution path (`crate::integrators`, `avalon_protocol::shard::
//! shard_authority`). A self-certifying shard with no verified name claim
//! at all is unaffected and fully functional; this module only ever adds a
//! human-readable name on top.
//!
//! No central authority is consulted anywhere in this module: verifying a
//! claim's signature is pure local logic, and verifying its domain proof
//! only ever contacts the claimed domain itself.

use std::net::SocketAddr;
use std::sync::OnceLock;
use std::time::Duration;

use avalon_protocol::domain_proof::{contested_name_winner, verify_domain_proof, WELL_KNOWN_PATH};
use avalon_protocol::shard_identity::{verify_name_binding_claim, NameBindingClaim};
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::outbound_policy::OutboundPolicy;
use crate::state::AppState;
use crate::topology_limits::{client_ip, EndpointLimits, TopologyError};
use crate::topology_probe::policy_error;

/// Largest well-known proof body this node will read.
const MAX_PROOF_BYTES: usize = 4 * 1024;
/// Longest name accepted at all — matches the DNS full-name limit, well
/// past anything a real domain needs.
const MAX_NAME_LEN: usize = 253;
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const WELL_KNOWN_PROOF_METHOD: &str = "well-known";

fn limits() -> &'static EndpointLimits {
    static LIMITS: OnceLock<EndpointLimits> = OnceLock::new();
    LIMITS.get_or_init(|| {
        EndpointLimits::from_env(
            "AVALON_NAME_CLAIM_RATE_LIMIT_PER_MINUTE",
            5,
            "AVALON_NAME_CLAIM_MAX_CONCURRENT",
            4,
        )
    })
}

/// Whether `name` is shaped like a domain this node could plausibly fetch a
/// well-known proof from — not a full RFC 1035 validator, just enough to
/// reject anything that obviously can't carry a domain proof (no scheme, no
/// path, no whitespace, at least one label separator).
fn is_domain_shaped(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name.contains('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && !name.starts_with('.')
        && !name.starts_with('-')
        && !name.ends_with('.')
        && !name.ends_with('-')
}

/// Wire shape of a `NameBindingClaim` — the server's own DTO (rather than
/// deriving `ToSchema` on the protocol type directly) so the OpenAPI schema
/// stays owned by `crates/server`, matching every other request DTO in this
/// module family (e.g. `crate::integrators::CreateIntegratorRequest`).
#[derive(Debug, Deserialize, ToSchema)]
pub struct NameClaimRequest {
    pub self_certifying_id: String,
    pub public_key: String,
    pub name: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub created_at: OffsetDateTime,
    pub signature: String,
}

impl From<NameClaimRequest> for NameBindingClaim {
    fn from(request: NameClaimRequest) -> Self {
        NameBindingClaim {
            self_certifying_id: request.self_certifying_id,
            public_key: request.public_key,
            name: request.name,
            created_at: request.created_at,
            signature: request.signature,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NameClaimResponse {
    pub name: String,
    pub self_certifying_id: String,
    pub proof_method: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub verified_at: OffsetDateTime,
}

struct NameClaimRow {
    name: String,
    self_certifying_id: String,
    public_key: String,
    claim_created_at: OffsetDateTime,
    signature: String,
    proof_method: String,
    verified_at: OffsetDateTime,
}

impl NameClaimRow {
    fn as_claim(&self) -> NameBindingClaim {
        NameBindingClaim {
            self_certifying_id: self.self_certifying_id.clone(),
            public_key: self.public_key.clone(),
            name: self.name.clone(),
            created_at: self.claim_created_at,
            signature: self.signature.clone(),
        }
    }

    fn as_response(&self) -> NameClaimResponse {
        NameClaimResponse {
            name: self.name.clone(),
            self_certifying_id: self.self_certifying_id.clone(),
            proof_method: self.proof_method.clone(),
            verified_at: self.verified_at,
        }
    }
}

fn db_error(e: sqlx::Error) -> TopologyError {
    TopologyError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "storage_error",
        format!("name claim storage error: {e}"),
    )
}

async fn fetch_row(pool: &sqlx::PgPool, name: &str) -> Result<Option<NameClaimRow>, TopologyError> {
    let row = sqlx::query(
        "SELECT name, self_certifying_id, public_key, claim_created_at, signature, \
         proof_method, verified_at FROM name_claims WHERE name = $1",
    )
    .bind(name)
    .fetch_optional(pool)
    .await
    .map_err(db_error)?;
    Ok(row.map(|row| NameClaimRow {
        name: row.get("name"),
        self_certifying_id: row.get("self_certifying_id"),
        public_key: row.get("public_key"),
        claim_created_at: row.get("claim_created_at"),
        signature: row.get("signature"),
        proof_method: row.get("proof_method"),
        verified_at: row.get("verified_at"),
    }))
}

async fn upsert_row(
    pool: &sqlx::PgPool,
    claim: &NameBindingClaim,
    verified_at: OffsetDateTime,
) -> Result<(), TopologyError> {
    sqlx::query(
        "INSERT INTO name_claims \
         (name, self_certifying_id, public_key, claim_created_at, signature, proof_method, verified_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (name) DO UPDATE SET \
         self_certifying_id = excluded.self_certifying_id, \
         public_key = excluded.public_key, \
         claim_created_at = excluded.claim_created_at, \
         signature = excluded.signature, \
         proof_method = excluded.proof_method, \
         verified_at = excluded.verified_at",
    )
    .bind(&claim.name)
    .bind(&claim.self_certifying_id)
    .bind(&claim.public_key)
    .bind(claim.created_at)
    .bind(&claim.signature)
    .bind(WELL_KNOWN_PROOF_METHOD)
    .bind(verified_at)
    .execute(pool)
    .await
    .map_err(db_error)?;
    Ok(())
}

/// Fetches the well-known proof body from `base_url` — `policy` is taken
/// explicitly (rather than read from the environment internally) so tests
/// can point this at a mock server with a permissive policy instead of a
/// real domain, with no process-global env mutation involved.
async fn fetch_well_known_proof(
    policy: &OutboundPolicy,
    base_url: &str,
) -> Result<String, TopologyError> {
    let checked = policy
        .check_base_url(base_url)
        .await
        .map_err(policy_error)?;
    let client = checked.client(FETCH_TIMEOUT);
    let url = format!("{}{WELL_KNOWN_PATH}", checked.base_url);
    let response = client.get(&url).send().await.map_err(|_| {
        TopologyError::new(
            StatusCode::BAD_GATEWAY,
            "domain_proof_unreachable",
            "could not fetch the well-known domain proof",
        )
    })?;
    if !response.status().is_success() {
        return Err(TopologyError::new(
            StatusCode::BAD_GATEWAY,
            "domain_proof_unreachable",
            "well-known domain proof endpoint did not respond successfully",
        ));
    }
    let bytes = response.bytes().await.map_err(|_| {
        TopologyError::new(
            StatusCode::BAD_GATEWAY,
            "domain_proof_unreachable",
            "could not read the well-known domain proof body",
        )
    })?;
    if bytes.len() > MAX_PROOF_BYTES {
        return Err(TopologyError::new(
            StatusCode::BAD_GATEWAY,
            "domain_proof_too_large",
            "well-known domain proof body exceeds the size limit",
        ));
    }
    Ok(String::from_utf8_lossy(&bytes).trim().to_string())
}

/// Fetches `name`'s well-known domain proof over HTTPS.
async fn fetch_domain_proof(name: &str) -> Result<String, TopologyError> {
    fetch_well_known_proof(&OutboundPolicy::from_env(), &format!("https://{name}")).await
}

/// Submits a signed name-binding claim and, if its domain proof checks out,
/// records it as this name's verified binding — replacing a previously
/// verified claim for the same name only when the new one wins the
/// deterministic contest (`avalon_protocol::domain_proof::
/// contested_name_winner`).
#[utoipa::path(
    post,
    path = "/shards/{self_certifying_id}/name-claims",
    tag = "shard-identity",
    params(("self_certifying_id" = String, Path)),
    request_body = NameClaimRequest,
    responses(
        (status = 200, body = NameClaimResponse),
        (status = 400, description = "malformed claim, id mismatch, or a name with no domain shape"),
        (status = 409, description = "name already verified for a different, contest-winning claim"),
        (status = 422, description = "domain proof did not match the claim"),
        (status = 429, description = "Rate limit or in-flight cap hit; see Retry-After"),
    ),
)]
pub async fn submit_name_claim(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(self_certifying_id): Path<String>,
    Json(body): Json<NameClaimRequest>,
) -> Result<Json<NameClaimResponse>, TopologyError> {
    let claim: NameBindingClaim = body.into();
    let limits = limits();
    limits.admit(client_ip(peer, &headers))?;

    if claim.self_certifying_id != self_certifying_id {
        return Err(TopologyError::new(
            StatusCode::BAD_REQUEST,
            "self_certifying_id_mismatch",
            "claim self_certifying_id does not match the path",
        ));
    }
    if !verify_name_binding_claim(&claim) {
        return Err(TopologyError::new(
            StatusCode::BAD_REQUEST,
            "invalid_claim",
            "claim does not verify against its own key and signature",
        ));
    }
    if !is_domain_shaped(&claim.name) {
        return Err(TopologyError::new(
            StatusCode::BAD_REQUEST,
            "name_not_domain_shaped",
            "name must be a domain that can carry a well-known proof",
        ));
    }

    let _permit = limits.in_flight.try_enter()?;
    let fetched = fetch_domain_proof(&claim.name).await?;
    if !verify_domain_proof(&claim, &fetched) {
        return Err(TopologyError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "domain_proof_mismatch",
            "the fetched domain proof does not match this claim",
        ));
    }

    if let Some(existing) = fetch_row(&state.pool, &claim.name).await? {
        if existing.self_certifying_id != claim.self_certifying_id {
            let existing_claim = existing.as_claim();
            if contested_name_winner(&existing_claim, &claim).self_certifying_id
                != claim.self_certifying_id
            {
                return Err(TopologyError::new(
                    StatusCode::CONFLICT,
                    "name_contested",
                    "this name is already verified for a different shard",
                ));
            }
        }
    }

    let verified_at = OffsetDateTime::now_utc();
    upsert_row(&state.pool, &claim, verified_at).await?;
    Ok(Json(NameClaimResponse {
        name: claim.name,
        self_certifying_id: claim.self_certifying_id,
        proof_method: WELL_KNOWN_PROOF_METHOD.to_string(),
        verified_at,
    }))
}

/// Resolves a verified name to its self-certifying shard id.
#[utoipa::path(
    get,
    path = "/shards/name/{name}",
    tag = "shard-identity",
    params(("name" = String, Path)),
    responses(
        (status = 200, body = NameClaimResponse),
        (status = 404, description = "no verified claim for this name"),
    ),
)]
pub async fn resolve_name(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<NameClaimResponse>, TopologyError> {
    fetch_row(&state.pool, &name)
        .await?
        .map(|row| Json(row.as_response()))
        .ok_or_else(|| {
            TopologyError::new(
                StatusCode::NOT_FOUND,
                "name_not_found",
                "no verified claim for this name",
            )
        })
}

/// Lists every verified name currently bound to `self_certifying_id`.
#[utoipa::path(
    get,
    path = "/shards/{self_certifying_id}/name-claims",
    tag = "shard-identity",
    params(("self_certifying_id" = String, Path)),
    responses((status = 200, body = Vec<NameClaimResponse>)),
)]
pub async fn list_names_for_shard(
    State(state): State<AppState>,
    Path(self_certifying_id): Path<String>,
) -> Result<Json<Vec<NameClaimResponse>>, TopologyError> {
    let rows = sqlx::query(
        "SELECT name, self_certifying_id, public_key, claim_created_at, signature, \
         proof_method, verified_at FROM name_claims WHERE self_certifying_id = $1 ORDER BY name",
    )
    .bind(&self_certifying_id)
    .fetch_all(&state.pool)
    .await
    .map_err(db_error)?;
    let names = rows
        .into_iter()
        .map(|row| {
            NameClaimRow {
                name: row.get("name"),
                self_certifying_id: row.get("self_certifying_id"),
                public_key: row.get("public_key"),
                claim_created_at: row.get("claim_created_at"),
                signature: row.get("signature"),
                proof_method: row.get("proof_method"),
                verified_at: row.get("verified_at"),
            }
            .as_response()
        })
        .collect();
    Ok(Json(names))
}

#[cfg(test)]
mod tests {
    use super::*;
    use avalon_protocol::domain_proof::expected_domain_proof;
    use avalon_protocol::shard_identity::sign_name_binding_claim;
    use ed25519_dalek::SigningKey;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn signed_claim(name: &str) -> (SigningKey, NameBindingClaim) {
        let key = SigningKey::generate(&mut rand::rng());
        let claim = sign_name_binding_claim(&key, name, OffsetDateTime::UNIX_EPOCH);
        (key, claim)
    }

    #[test]
    fn domain_shape_accepts_ordinary_domains_and_rejects_the_rest() {
        assert!(is_domain_shaped("wow-demo.example"));
        assert!(is_domain_shaped("sub.wow-demo.example"));
        assert!(!is_domain_shaped(""));
        assert!(!is_domain_shaped("no-dot-at-all"));
        assert!(!is_domain_shaped("https://wow-demo.example"));
        assert!(!is_domain_shaped("wow demo.example"));
        assert!(!is_domain_shaped(".example.com"));
        assert!(!is_domain_shaped("example.com."));
        assert!(!is_domain_shaped(&"a.".repeat(200)));
    }

    #[tokio::test]
    async fn well_known_proof_is_fetched_and_matched() {
        let policy = OutboundPolicy::new(true);
        let server = MockServer::start().await;
        let (_key, claim) = signed_claim("wow-demo.example");
        let proof = expected_domain_proof(&claim);

        Mock::given(method("GET"))
            .and(path(WELL_KNOWN_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_string(proof))
            .mount(&server)
            .await;

        let fetched = fetch_well_known_proof(&policy, &server.uri())
            .await
            .unwrap();
        assert!(verify_domain_proof(&claim, &fetched));
    }

    #[tokio::test]
    async fn a_mismatched_published_proof_is_rejected() {
        let policy = OutboundPolicy::new(true);
        let server = MockServer::start().await;
        let (_key, claim) = signed_claim("wow-demo.example");

        Mock::given(method("GET"))
            .and(path(WELL_KNOWN_PATH))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("avalon-name-proof-v1:deadbeef"),
            )
            .mount(&server)
            .await;

        let fetched = fetch_well_known_proof(&policy, &server.uri())
            .await
            .unwrap();
        assert!(!verify_domain_proof(&claim, &fetched));
    }

    #[test]
    fn contested_names_pick_the_earlier_claim() {
        let key_a = SigningKey::generate(&mut rand::rng());
        let key_b = SigningKey::generate(&mut rand::rng());
        let earlier =
            sign_name_binding_claim(&key_a, "wow-demo.example", OffsetDateTime::UNIX_EPOCH);
        let later = sign_name_binding_claim(
            &key_b,
            "wow-demo.example",
            OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
        );
        let winner = contested_name_winner(&earlier, &later);
        assert_eq!(winner.self_certifying_id, earlier.self_certifying_id);
    }

    #[test]
    fn rate_limiter_throttles_one_source_without_affecting_another() {
        let limits = EndpointLimits::new(1, 4);
        let ip_a: std::net::IpAddr = "203.0.113.1".parse().unwrap();
        let ip_b: std::net::IpAddr = "203.0.113.2".parse().unwrap();
        assert!(limits.admit(ip_a).is_ok());
        assert!(limits.admit(ip_a).is_err());
        assert!(limits.admit(ip_b).is_ok());
    }

    async fn live_test_pool() -> sqlx::PgPool {
        avalon_devenv::load();
        let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        sqlx::postgres::PgPoolOptions::new()
            .connect(&database_url)
            .await
            .expect("failed to connect to Postgres — is it reachable?")
    }

    /// Real Postgres, real `name_claims` table (migration 0074): a fresh
    /// claim inserts, is resolvable by name, and a later claim for a
    /// *different* self-certifying id that loses the contest is rejected
    /// without disturbing the stored winner — the exact "verify, store,
    /// resolve" path `submit_name_claim` drives, exercised directly against
    /// the storage helpers rather than over HTTP.
    #[tokio::test]
    #[ignore]
    async fn a_claim_round_trips_through_real_postgres_and_resists_a_losing_contest() {
        let pool = live_test_pool().await;
        let name = format!("live-test-{}.example", uuid::Uuid::new_v4());

        let winner_key = SigningKey::generate(&mut rand::rng());
        let winner_claim = sign_name_binding_claim(&winner_key, &name, OffsetDateTime::UNIX_EPOCH);
        assert!(fetch_row(&pool, &name).await.unwrap().is_none());

        let verified_at = OffsetDateTime::now_utc();
        upsert_row(&pool, &winner_claim, verified_at).await.unwrap();

        let stored = fetch_row(&pool, &name).await.unwrap().unwrap();
        assert_eq!(stored.self_certifying_id, winner_claim.self_certifying_id);
        assert_eq!(stored.as_claim(), winner_claim);

        // A later, later-timestamped claim for a different key loses the
        // deterministic contest against the already-stored, earlier claim.
        let later_key = SigningKey::generate(&mut rand::rng());
        let later_claim = sign_name_binding_claim(
            &later_key,
            &name,
            OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(60),
        );
        let existing_claim = fetch_row(&pool, &name).await.unwrap().unwrap().as_claim();
        assert_eq!(
            contested_name_winner(&existing_claim, &later_claim).self_certifying_id,
            winner_claim.self_certifying_id,
        );

        // Cleanup: leave the shared database exactly as this test found it.
        sqlx::query("DELETE FROM name_claims WHERE name = $1")
            .bind(&name)
            .execute(&pool)
            .await
            .unwrap();
    }
}
