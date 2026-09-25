//! `POST /nodes/probe`: this node measures the round trip to a peer it already
//! knows and reports timings only.
//!
//! The target must be in this node's peer table or its unverified gossip
//! pool, on this node's network, and every address it resolves to must pass
//! [`crate::outbound_policy`]. The endpoint sends at most [`MAX_SAMPLES`]
//! sequential `GET /nodes/status` requests, follows no redirects, and never
//! returns anything the target sent back. Timings are this node's own
//! observations. A successful probe of an unverified-pool target promotes it
//! into the peer table — see `crate::nodes::promote_on_contact`.

use std::net::SocketAddr;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::outbound_policy::{OutboundPolicy, PolicyError};
use crate::overlay_routing::canonical_base_url;
use crate::state::AppState;
use crate::topology_limits::{client_ip, EndpointLimits, TopologyError};

/// Most samples one call may take.
pub const MAX_SAMPLES: u8 = 3;
/// Per-sample request timeout.
pub const SAMPLE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Deserialize, ToSchema)]
pub struct ProbeRequest {
    /// Base URL of a node in this node's peer table.
    pub target: String,
    /// Sequential samples to take, 1 to 3 (default 1).
    #[serde(default)]
    pub samples: Option<u8>,
}

#[derive(Debug, Serialize, ToSchema, PartialEq)]
pub struct ProbeResponse {
    pub target: String,
    /// True when every requested sample completed.
    pub ok: bool,
    /// Round trip of each completed sample in milliseconds, in order. The
    /// first sample includes connection setup.
    pub samples_ms: Vec<f64>,
    pub min_ms: Option<f64>,
    pub median_ms: Option<f64>,
    /// `timeout`, `unreachable` or `bad_status` when a sample failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn limits() -> &'static EndpointLimits {
    static LIMITS: OnceLock<EndpointLimits> = OnceLock::new();
    LIMITS.get_or_init(|| {
        EndpointLimits::from_env(
            "AVALON_PROBE_RATE_LIMIT_PER_MINUTE",
            30,
            "AVALON_PROBE_MAX_CONCURRENT",
            8,
        )
    })
}

/// Validates a requested sample count.
pub fn resolve_samples(requested: Option<u8>) -> Result<u8, TopologyError> {
    match requested.unwrap_or(1) {
        n @ 1..=MAX_SAMPLES => Ok(n),
        _ => Err(TopologyError::new(
            StatusCode::BAD_REQUEST,
            "invalid_samples",
            format!("samples must be between 1 and {MAX_SAMPLES}"),
        )),
    }
}

pub fn median(sorted: &[f64]) -> Option<f64> {
    let n = sorted.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(sorted[n / 2]),
        _ => Some((sorted[n / 2 - 1] + sorted[n / 2]) / 2.0),
    }
}

/// Takes `samples` sequential timings against `target` (already resolved
/// through the policy).
pub async fn measure(
    target: &crate::outbound_policy::CheckedTarget,
    samples: u8,
    timeout: Duration,
) -> ProbeResponse {
    let client = target.client(timeout);
    let url = format!("{}/nodes/status", target.base_url);
    let mut samples_ms = Vec::new();
    let mut error = None;
    for _ in 0..samples {
        let started = Instant::now();
        match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => {
                samples_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            Ok(_) => {
                error = Some("bad_status");
                break;
            }
            Err(e) => {
                error = Some(if e.is_timeout() {
                    "timeout"
                } else {
                    "unreachable"
                });
                break;
            }
        }
    }
    let mut sorted = samples_ms.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    ProbeResponse {
        target: target.base_url.clone(),
        ok: error.is_none(),
        min_ms: sorted.first().copied(),
        median_ms: median(&sorted),
        samples_ms,
        error: error.map(str::to_string),
    }
}

/// Maps a policy refusal to the API error.
pub fn policy_error(e: PolicyError) -> TopologyError {
    match e {
        PolicyError::Resolve => TopologyError::new(
            StatusCode::BAD_GATEWAY,
            "target_unresolvable",
            "target host did not resolve",
        ),
        _ => TopologyError::new(
            StatusCode::FORBIDDEN,
            "target_not_allowed",
            "target address is not allowed by this node's outbound policy",
        ),
    }
}

/// Measure the round trip from this node to a known peer.
///
/// Timings only; nothing the target returns is passed on. The target must be
/// in this node's peer table.
#[utoipa::path(
    post,
    path = "/nodes/probe",
    tag = "nodes",
    request_body = ProbeRequest,
    responses(
        (status = 200, body = ProbeResponse),
        (status = 400, description = "Invalid sample count or target"),
        (status = 403, description = "Target address refused by outbound policy"),
        (status = 404, description = "unknown_target: not in this node's peer table"),
        (status = 429, description = "Rate limit or in-flight cap hit; see Retry-After"),
    ),
)]
pub async fn probe(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<ProbeRequest>,
) -> Result<Json<ProbeResponse>, TopologyError> {
    let limits = limits();
    limits.admit(client_ip(peer, &headers))?;
    let samples = resolve_samples(body.samples)?;

    let wanted = canonical_base_url(&body.target);
    let known = state
        .peers
        .list_all()
        .into_iter()
        .chain(state.peers.list_unverified())
        .find(|p| canonical_base_url(&p.base_url) == wanted)
        .ok_or_else(|| {
            TopologyError::new(
                StatusCode::NOT_FOUND,
                "unknown_target",
                "target is not in this node's peer table",
            )
        })?;
    if known.network_id != state.chain.network_id() {
        return Err(TopologyError::new(
            StatusCode::NOT_FOUND,
            "unknown_target",
            "target is not in this node's peer table",
        ));
    }

    let _permit = limits.in_flight.try_enter()?;
    let checked = OutboundPolicy::from_env()
        .check_base_url(&known.base_url)
        .await
        .map_err(policy_error)?;
    let result = measure(&checked, samples, SAMPLE_TIMEOUT).await;
    if result.ok {
        crate::nodes::promote_on_contact(
            &state.peers,
            crate::peer_admission::admission(),
            &known.base_url,
        );
    }
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn samples_default_to_one_and_cap_at_three() {
        assert_eq!(resolve_samples(None).unwrap(), 1);
        assert_eq!(resolve_samples(Some(3)).unwrap(), 3);
        assert!(resolve_samples(Some(0)).is_err());
        assert!(resolve_samples(Some(4)).is_err());
    }

    #[test]
    fn median_handles_odd_even_and_empty() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[5.0]), Some(5.0));
        assert_eq!(median(&[1.0, 2.0, 9.0]), Some(2.0));
        assert_eq!(median(&[1.0, 3.0]), Some(2.0));
    }

    async fn target_for(server: &MockServer) -> crate::outbound_policy::CheckedTarget {
        OutboundPolicy::new(true)
            .check_base_url(&server.uri())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn measures_sequential_samples_and_returns_timings_only() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/nodes/status"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("secret body")
                    .set_delay(Duration::from_millis(20)),
            )
            .expect(3)
            .mount(&server)
            .await;
        let r = measure(&target_for(&server).await, 3, Duration::from_secs(2)).await;
        assert!(r.ok);
        assert_eq!(r.samples_ms.len(), 3);
        assert!(r.samples_ms.iter().all(|ms| *ms >= 20.0));
        assert!(r.min_ms.unwrap() <= r.median_ms.unwrap());
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("secret body"));
    }

    #[tokio::test]
    async fn redirects_are_not_followed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/nodes/status"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", "http://127.0.0.1:1/x"),
            )
            .mount(&server)
            .await;
        let r = measure(&target_for(&server).await, 1, Duration::from_secs(2)).await;
        assert!(!r.ok);
        assert_eq!(r.error.as_deref(), Some("bad_status"));
        assert!(r.samples_ms.is_empty());
    }

    #[tokio::test]
    async fn slow_target_reports_timeout() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(500)))
            .mount(&server)
            .await;
        let r = measure(&target_for(&server).await, 2, Duration::from_millis(100)).await;
        assert!(!r.ok);
        assert_eq!(r.error.as_deref(), Some("timeout"));
    }

    #[tokio::test]
    async fn closed_port_reports_unreachable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let t = OutboundPolicy::new(true)
            .check_base_url(&format!("http://{addr}"))
            .await
            .unwrap();
        let r = measure(&t, 1, Duration::from_secs(1)).await;
        assert_eq!(r.error.as_deref(), Some("unreachable"));
    }

    #[test]
    fn policy_refusals_map_to_forbidden() {
        let e = policy_error(PolicyError::Forbidden("169.254.169.254".parse().unwrap()));
        assert_eq!(e.status, StatusCode::FORBIDDEN);
        assert_eq!(e.code, "target_not_allowed");
    }
}
