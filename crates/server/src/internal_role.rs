//! Operator-internal, node-to-node RPC — the foundational
//! piece of Node Role Separation. Once a Gateway process
//! and its Indexer/Realtime/Settlement processes are genuinely separate,
//! the Gateway's call sites (today's direct in-process
//! trait calls, e.g. `state.indexer.apply(...)`) need a way to reach a
//! role that isn't running in the same process anymore. This module is
//! that mechanism, built once here so each role doesn't invent its
//! own wire format.
//!
//! **Explicitly distinct from `crate::settlement`'s `/ledger/*` mirror-sync
//! protocol.** That protocol is multi-operator and
//! trust-minimized: independent Settlement nodes, run by different
//! operators, verifying and mirroring one log with no assumption that the
//! other side is honest. This protocol is the opposite: one operator's
//! own processes, inside their own deployment, talking to each other.
//! There is no independent verification on either side — a caller here
//! trusts a response exactly as much as it would trust an in-process
//! function call to the same role, because from a deployment-topology
//! point of view that's genuinely what this is standing in for. Never
//! conflate the two: this protocol's auth (below) must never be reused
//! for, or accepted by, anything under `/ledger/*`, and vice versa.
//!
//! **Transport: HTTP+JSON**, matching every other endpoint in this
//! codebase (`/ledger/*`, `/nodes/*`, the whole request-facing API) —
//! chosen for consistency and debuggability over inventing a second
//! transport (gRPC, etc.) for just this one internal case. Nothing about
//! this traffic pattern (request/response, JSON-serializable domain
//! types, low enough volume per role for now) presents a concrete
//! performance problem HTTP+JSON can't handle; revisit only if a real one
//! is measured, not speculatively.
//!
//! **Auth: a third shared-secret bearer token, `AVALON_INTERNAL_ROLE_KEY`
//! (`AppState::internal_role_key`)** — same shape as
//! `crate::settlement::require_settlement_submit_key` and
//! `crate::admin::check_admin_token`, and deliberately its own env var
//! rather than reusing either. Each of the three covers a different trust
//! domain (see `crate::admin`'s own module doc comment for the settlement-
//! vs-admin split, and `AppState::internal_role_key`'s doc comment for
//! this one): `AVALON_SETTLEMENT_SUBMIT_KEY` may be shared across two
//! nodes one operator runs specifically so they can submit to each
//! other's ledger; `AVALON_ADMIN_TOKEN` is narrower still, meant to be
//! held by only the one process's own operator console. This key sits
//! between them — shared across one operator's own Gateway/Indexer/
//! Realtime/Settlement processes, but never handed to, or accepted from,
//! anything outside that one deployment. Unset means every request under
//! `/internal/*` is refused, never silently open — the same posture the
//! other two keys already establish.
//!
//! **Pattern proven here: the Indexer role.** `avalon_indexer::Indexer`
//! is the cleanest first target — already a small, well-
//! defined trait (`apply`, `rebuild`) in a crate (`avalon-indexer`) that
//! depends only on `avalon-protocol`, not `avalon-server`. This module
//! adds:
//! - Server side: [`apply_indexer_event`]/[`rebuild_indexer`], thin HTTP
//!   handlers that do exactly what a local `Indexer::apply`/`rebuild`
//!   call would, bearer-gated by [`require_internal_role_key`].
//! - Client side: [`RemoteIndexer`], a real `Indexer` implementation
//!   backed by HTTP calls to those handlers — so any code written against
//!   `dyn Indexer`/`impl Indexer` can't tell whether it's talking
//!   in-process or over the network. **This is the actual proof the
//!   pattern works**, not just a design on paper.
//!
//! Wiring `RemoteIndexer` into `avalon-server`'s own startup path (so a
//! Gateway-only deployment actually uses it instead of a local
//! `PostgresIndexer`) is explicitly #662's job, not this ticket's —
//! today's handlers call `PostgresIndexer::apply_in_tx` inside a shared
//! Postgres transaction with their own app-data writes (see
//! `AppState::indexer`'s doc comment), a coupling #662 has to unpick
//! first. This ticket proves the remote-implementation half of the
//! pattern in isolation, live, over HTTP, between two real processes —
//! see `crates/server/tests/internal_role_protocol.rs`.

use std::time::Duration;

use avalon_indexer::{IndexError, Indexer};
use avalon_protocol::events::ProtocolEvent;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::state::AppState;

/// Shared bearer-token check every `/internal/*` handler in this module
/// uses — same shape as `crate::settlement::require_settlement_submit_key`
/// and `crate::admin::check_admin_token`. See this module's own doc
/// comment for why this is its own, third, env-scoped key rather than
/// reusing either of those.
fn require_internal_role_key(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    let expected = state
        .internal_role_key
        .as_deref()
        .ok_or(AppError::Unauthorized)?;
    let provided = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(AppError::Unauthorized)?;
    if provided != expected {
        return Err(AppError::Unauthorized);
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Server side: this node acting as the Indexer role for a remote caller.
// ---------------------------------------------------------------------

/// `POST /internal/indexer/apply` — the network-facing twin of
/// [`avalon_indexer::Indexer::apply`]. Runs the exact same call this
/// node's own in-process callers use (`state.indexer.apply`, which opens
/// its own transaction — see `avalon_indexer::postgres::PostgresIndexer`'s
/// doc comment for why that's a different entry point from
/// `apply_in_tx`), so a remote caller gets identical behavior to a local
/// one. Bearer-gated; see [`require_internal_role_key`].
pub async fn apply_indexer_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(event): Json<ProtocolEvent>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_internal_role_key(&state, &headers)?;
    state.indexer.apply(&event).await?;
    Ok(Json(serde_json::json!({})))
}

#[derive(Deserialize)]
pub struct RebuildIndexerRequest {
    pub events: Vec<ProtocolEvent>,
}

/// `POST /internal/indexer/rebuild` — the network-facing twin of
/// [`avalon_indexer::Indexer::rebuild`]. A second, distinct operation
/// (not just `apply` looped remotely) so the remote implementation below
/// is proven against more than one real trait method. Bearer-gated; see
/// [`require_internal_role_key`].
pub async fn rebuild_indexer(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RebuildIndexerRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_internal_role_key(&state, &headers)?;
    state.indexer.rebuild(&body.events).await?;
    Ok(Json(serde_json::json!({})))
}

// ---------------------------------------------------------------------
// Client side: this node reaching an Indexer role running elsewhere.
// ---------------------------------------------------------------------

/// A bounded timeout for every request `RemoteIndexer` makes — without
/// this, a remote role that's hung (not down, not refusing, just never
/// answering — e.g. a network partition mid-response) would make a caller
/// hang too, exactly the failure mode issue #661 calls out as
/// unacceptable ("not a hang"). Chosen generously for a same-deployment,
/// same-datacenter call (this is never meant to cross the public
/// internet — see this module's own doc comment) while still being far
/// short of a caller-visible stall.
const REMOTE_INDEXER_TIMEOUT: Duration = Duration::from_secs(10);

/// A real `Indexer`, backed by HTTP calls to another node's
/// [`apply_indexer_event`]/[`rebuild_indexer`] endpoints instead of a
/// local Postgres pool — the concrete, live-verified instance of the
/// remote-role pattern. Any code holding an `impl Indexer` (or, once
/// this becomes a `dyn Indexer`) can use this exactly as it would
/// `avalon_indexer::postgres::PostgresIndexer`, without knowing the
/// difference.
#[derive(Clone)]
pub struct RemoteIndexer {
    client: reqwest::Client,
    /// The remote Indexer role's base URL, no trailing slash.
    base_url: String,
    /// `AVALON_INTERNAL_ROLE_KEY`, sent as this node's own credential on
    /// every request. `None` behaves exactly like an unset key on the
    /// server side of `require_internal_role_key` — every call gets
    /// refused (surfaced here as [`IndexError::RemoteUnreachable`], since
    /// from this node's point of view an authless 401 is just as much "I
    /// can't get a real answer from this role" as a connection failure
    /// is) — never silently sent as an empty/absent header hoping the
    /// remote treats that as open.
    role_key: Option<String>,
}

impl RemoteIndexer {
    pub fn new(base_url: impl Into<String>, role_key: Option<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(REMOTE_INDEXER_TIMEOUT)
                .build()
                .expect("failed to build the RemoteIndexer HTTP client"),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            role_key,
        }
    }

    /// `AVALON_INDEXER_REMOTE_URL`/`AVALON_INTERNAL_ROLE_KEY`. `Ok(None)`
    /// when the URL isn't set, meaning this node has no remote Indexer role
    /// configured at all — `main.rs`'s own call site is what turns
    /// that into a hard startup failure when the Indexer role is also
    /// excluded locally, exactly the same shape
    /// `nodes::realtime_mode_from_env` already establishes for
    /// `AVALON_REALTIME_URL`. `Err` when the URL is set but not
    /// well-formed — issue #665: previously this only checked
    /// non-emptiness, so a malformed `AVALON_INDEXER_REMOTE_URL` would
    /// silently build a `RemoteIndexer` whose every request then failed
    /// with a confusing runtime error instead of a clear one at startup;
    /// now shares `crate::backing_services::normalize_and_validate_url`
    /// with the other two backing-service vars, so all three fail the same
    /// way on a malformed value.
    pub fn from_env() -> Result<Option<Self>, String> {
        let Some(raw) = std::env::var("AVALON_INDEXER_REMOTE_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
        else {
            return Ok(None);
        };
        let base_url =
            crate::backing_services::normalize_and_validate_url("AVALON_INDEXER_REMOTE_URL", &raw)?;
        let role_key = std::env::var("AVALON_INTERNAL_ROLE_KEY")
            .ok()
            .filter(|s| !s.is_empty());
        Ok(Some(Self::new(base_url, role_key)))
    }

    /// This remote Indexer role's own base URL — read by `main.rs`'s issue
    /// #665 reachability check, so it doesn't need to re-derive or re-parse
    /// `AVALON_INDEXER_REMOTE_URL` a second time.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .request(method, format!("{}{}", self.base_url, path));
        if let Some(key) = &self.role_key {
            req = req.bearer_auth(key);
        }
        req
    }
}

#[async_trait::async_trait]
impl Indexer for RemoteIndexer {
    async fn apply(&self, event: &ProtocolEvent) -> Result<(), IndexError> {
        let response = self
            .request(reqwest::Method::POST, "/internal/indexer/apply")
            .json(event)
            .send()
            .await
            .map_err(|e| {
                IndexError::RemoteUnreachable(format!(
                    "indexer apply request to {} failed: {e}",
                    self.base_url
                ))
            })?;
        if !response.status().is_success() {
            let status = response.status();
            return Err(IndexError::RemoteUnreachable(format!(
                "indexer apply rejected by {}: {status}",
                self.base_url
            )));
        }
        Ok(())
    }

    async fn rebuild(&self, events: &[ProtocolEvent]) -> Result<(), IndexError> {
        let response = self
            .request(reqwest::Method::POST, "/internal/indexer/rebuild")
            .json(&RebuildIndexerRequest {
                events: events.to_vec(),
            })
            .send()
            .await
            .map_err(|e| {
                IndexError::RemoteUnreachable(format!(
                    "indexer rebuild request to {} failed: {e}",
                    self.base_url
                ))
            })?;
        if !response.status().is_success() {
            let status = response.status();
            return Err(IndexError::RemoteUnreachable(format!(
                "indexer rebuild rejected by {}: {status}",
                self.base_url
            )));
        }
        Ok(())
    }
}

// `RebuildIndexerRequest` needs `Serialize` on the client side (built and
// sent by `RemoteIndexer::rebuild`) as well as `Deserialize` on the
// server side (received by `rebuild_indexer`) — one shared DTO, not two,
// so the wire shape can't drift between the two ends of this protocol.
impl Serialize for RebuildIndexerRequest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        struct Wire<'a> {
            events: &'a [ProtocolEvent],
        }
        Wire {
            events: &self.events,
        }
        .serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use avalon_protocol::ids::GlobalId;
    use time::OffsetDateTime;
    use uuid::Uuid;
    use wiremock::matchers::{bearer_token, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A minimal, realistic `ProtocolEvent` — the exact shape doesn't
    /// matter for these tests (the mock server never inspects the body),
    /// only that it round-trips through `serde_json` the same way a real
    /// one would. Same shape `crates/indexer/src/projections/profiles.rs`'s
    /// own fixtures use.
    fn sample_event() -> ProtocolEvent {
        let identity_id = Uuid::new_v4();
        ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "identity.created".to_string(),
            issuer: GlobalId::new("identity", &identity_id.to_string(), "self", "created"),
            subject: GlobalId::new("identity", &identity_id.to_string(), "self", "created"),
            payload: serde_json::json!({
                "identity_id": identity_id,
                "display_name": "test-remote-indexer",
            }),
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
            identity_chain: None,
        }
    }

    /// `RemoteIndexer::apply` against a mock server that accepts the
    /// bearer token and returns success — proves the client sends the
    /// right method/path/auth/body shape.
    #[tokio::test]
    async fn apply_succeeds_against_a_well_behaved_mock_remote() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/indexer/apply"))
            .and(bearer_token("test-role-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let remote = RemoteIndexer::new(server.uri(), Some("test-role-key".to_string()));
        remote
            .apply(&sample_event())
            .await
            .expect("apply against a well-behaved mock should succeed");
    }

    /// A wrong/missing bearer token is rejected by the mock the same way
    /// `require_internal_role_key` would reject it for real — the client
    /// must surface that as `IndexError::RemoteUnreachable`, not panic or
    /// silently succeed.
    #[tokio::test]
    async fn apply_reports_remote_unreachable_when_the_role_key_is_wrong() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/indexer/apply"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let remote = RemoteIndexer::new(server.uri(), Some("wrong-key".to_string()));
        let err = remote
            .apply(&sample_event())
            .await
            .expect_err("a 401 from the remote must surface as an error");
        assert!(matches!(err, IndexError::RemoteUnreachable(_)));
    }

    /// Pointed at a port nothing is listening on — the connection-refused
    /// case, the clearest "unreachable" failure mode issue #661 asks for.
    #[tokio::test]
    async fn apply_reports_remote_unreachable_when_nothing_is_listening() {
        let remote = RemoteIndexer::new(
            "http://127.0.0.1:1".to_string(),
            Some("test-role-key".to_string()),
        );
        let err = remote
            .apply(&sample_event())
            .await
            .expect_err("a connection failure must surface as an error, not a panic");
        assert!(matches!(err, IndexError::RemoteUnreachable(_)));
    }

    /// `rebuild` gets the same treatment as `apply` — a second real trait
    /// method proven against the mock, not just the default `apply`-looped
    /// behavior `avalon_indexer::Indexer::rebuild`'s default impl would
    /// give for free.
    #[tokio::test]
    async fn rebuild_succeeds_against_a_well_behaved_mock_remote() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/indexer/rebuild"))
            .and(bearer_token("test-role-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let remote = RemoteIndexer::new(server.uri(), Some("test-role-key".to_string()));
        remote
            .rebuild(&[sample_event(), sample_event()])
            .await
            .expect("rebuild against a well-behaved mock should succeed");
    }
}
