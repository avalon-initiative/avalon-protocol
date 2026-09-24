//! `POST /nodes/trace`: a traceroute across the overlay.
//!
//! The receiving node applies [`crate::overlay_routing::next_hop`] toward the
//! target and, unless it is the target, forwards the same request to the
//! chosen active neighbor and waits. Each node prepends its own hop entry to
//! the path that comes back, so the first node returns the whole path.
//!
//! One forward per hop, no fan-out. Forwards go only to the neighbor
//! `next_hop` chose, through [`crate::outbound_policy`]. Every field of an
//! incoming request is untrusted and clamped: hop budget, time budget, visited
//! list and identifiers. Durations are measured by each node on its own
//! clock; no timestamps cross nodes. All hop data is self-reported by the
//! nodes on the path and is advisory.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::outbound_policy::OutboundPolicy;
use crate::overlay_routing::{canonical_base_url, next_hop, NextHop, NoRouteReason, OverlayNode};
use crate::state::AppState;
use crate::topology_limits::{client_ip, EndpointLimits, TopologyError};

/// Hard cap on forwards for one trace.
pub const MAX_TTL: u8 = 16;
pub const DEFAULT_TTL: u8 = 12;
/// Total time budget when the caller gives none, and its upper clamp.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(10);
pub const MAX_BUDGET: Duration = Duration::from_secs(15);
/// Time each hop keeps back from the budget it hands downstream, so a
/// downstream timeout can still be reported before this hop's own deadline.
pub const DOWNSTREAM_MARGIN: Duration = Duration::from_millis(100);
/// Below this remaining budget a hop stops instead of forwarding.
pub const MIN_FORWARD_BUDGET: Duration = Duration::from_millis(50);
const MAX_VISITED: usize = 32;
const MAX_URL_LEN: usize = 2048;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_FIELD_LEN: usize = 256;
const MAX_ROLES: usize = 8;

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct TraceRequest {
    /// Base URL of the node to trace to.
    pub target: String,
    /// Forwards still allowed, 0 to 16 (default 12). Clients use 1 to 16;
    /// a hop that receives 0 does not forward.
    #[serde(default)]
    pub ttl: Option<u8>,
    #[serde(default)]
    pub trace_id: Option<Uuid>,
    /// Set by forwarding nodes: base URLs already on the path.
    #[serde(default)]
    pub visited: Option<Vec<String>>,
    /// Set by forwarding nodes: remaining time budget in milliseconds.
    #[serde(default)]
    pub budget_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Ttl,
    NoRoute,
    Loop,
    Timeout,
    TargetUnreachable,
}

/// One node on the path, as that node reports itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TraceHop {
    pub index: u32,
    pub base_url: String,
    pub roles: Vec<String>,
    pub protocol_version: String,
    /// Time this node spent before forwarding (or in total, at the last hop),
    /// on its own clock.
    pub processing_ms: f64,
    /// Leg to the next hop: this node's round trip to it minus the time the
    /// next hop reports for itself, so it approximates network transit. When
    /// the next hop did not answer, the time this node waited. Absent at the
    /// last hop.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub to_next_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TraceResponse {
    pub trace_id: Uuid,
    pub target: String,
    pub reached: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stopped_reason: Option<StopReason>,
    /// Why routing stopped, when `stopped_reason` is `no_route` or
    /// `target_unreachable`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub detail: Option<String>,
    /// Time from receipt to response at the node that answered, on its own clock.
    pub total_ms: f64,
    /// Ordered path, first node to last, all self-reported.
    pub hops: Vec<TraceHop>,
}

/// A request after clamping.
#[derive(Debug, Clone)]
pub struct NormalizedTrace {
    pub target: String,
    pub ttl: u8,
    pub trace_id: Uuid,
    pub visited: Vec<String>,
    pub budget: Duration,
}

pub fn normalize(req: TraceRequest) -> Result<NormalizedTrace, TopologyError> {
    if req.target.len() > MAX_URL_LEN || OutboundPolicy::parse_base_url(&req.target).is_err() {
        return Err(TopologyError::new(
            StatusCode::BAD_REQUEST,
            "invalid_target",
            "target must be an http(s) base URL",
        ));
    }
    let visited: Vec<String> = req
        .visited
        .unwrap_or_default()
        .into_iter()
        .filter(|v| v.len() <= MAX_URL_LEN)
        .map(|v| canonical_base_url(&v))
        .take(MAX_VISITED)
        .collect();
    let budget = req
        .budget_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_BUDGET)
        .min(MAX_BUDGET);
    Ok(NormalizedTrace {
        target: canonical_base_url(&req.target),
        ttl: req.ttl.unwrap_or(DEFAULT_TTL).min(MAX_TTL),
        trace_id: req.trace_id.unwrap_or_else(Uuid::new_v4),
        visited,
        budget,
    })
}

/// What one forward attempt produced.
#[derive(Debug)]
pub enum ForwardOutcome {
    Response(TraceResponse),
    Timeout,
    Unreachable(&'static str),
}

/// Sends a trace to a neighbor. Implemented over HTTP in production.
pub trait Forwarder {
    fn forward(
        &self,
        neighbor: &OverlayNode,
        request: &TraceRequest,
        timeout: Duration,
    ) -> impl std::future::Future<Output = ForwardOutcome> + Send;
}

/// What a node knows about itself and its neighbors for one trace.
pub struct TraceContext {
    pub me: OverlayNode,
    pub roles: Vec<String>,
    pub protocol_version: String,
    pub target: OverlayNode,
    pub neighbors: Vec<OverlayNode>,
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn no_route_detail(r: &NoRouteReason) -> &'static str {
    match r {
        NoRouteReason::TargetIsSelf => "target_is_self",
        NoRouteReason::ForeignNetwork => "foreign_network",
        NoRouteReason::NoNeighbors => "no_neighbors",
        NoRouteReason::AllVisited => "all_visited",
        NoRouteReason::NoProgress => "no_progress",
    }
}

/// Runs this node's part of a trace. `started` is when the request arrived.
pub async fn run_trace<F: Forwarder>(
    ctx: &TraceContext,
    forwarder: &F,
    req: &NormalizedTrace,
    started: Instant,
) -> TraceResponse {
    let my_url = canonical_base_url(&ctx.me.base_url);
    let hop = |processing: Duration, to_next: Option<Duration>| TraceHop {
        index: 0,
        base_url: ctx.me.base_url.clone(),
        roles: ctx.roles.clone(),
        protocol_version: ctx.protocol_version.clone(),
        processing_ms: ms(processing),
        to_next_ms: to_next.map(ms),
    };
    let finish = |reached: bool,
                  reason: Option<StopReason>,
                  detail: Option<String>,
                  mut hops: Vec<TraceHop>| {
        for (i, h) in hops.iter_mut().enumerate() {
            h.index = i as u32;
        }
        TraceResponse {
            trace_id: req.trace_id,
            target: req.target.clone(),
            reached,
            stopped_reason: reason,
            detail,
            total_ms: ms(started.elapsed()),
            hops,
        }
    };

    if req.visited.contains(&my_url) {
        return finish(
            false,
            Some(StopReason::Loop),
            None,
            vec![hop(started.elapsed(), None)],
        );
    }
    if canonical_base_url(&ctx.target.base_url) == my_url {
        return finish(true, None, None, vec![hop(started.elapsed(), None)]);
    }
    if req.ttl == 0 {
        return finish(
            false,
            Some(StopReason::Ttl),
            None,
            vec![hop(started.elapsed(), None)],
        );
    }

    let mut visited: HashSet<String> = req.visited.iter().cloned().collect();
    visited.insert(my_url);
    let next = match next_hop(&ctx.me, &ctx.target, &ctx.neighbors, &visited) {
        NextHop::Direct(n) | NextHop::Forward(n) => n,
        NextHop::NoRoute(reason) => {
            return finish(
                false,
                Some(StopReason::NoRoute),
                Some(no_route_detail(&reason).to_string()),
                vec![hop(started.elapsed(), None)],
            );
        }
    };

    let remaining = req.budget.saturating_sub(started.elapsed());
    let downstream = remaining.saturating_sub(DOWNSTREAM_MARGIN);
    if downstream < MIN_FORWARD_BUDGET {
        return finish(
            false,
            Some(StopReason::Timeout),
            None,
            vec![hop(started.elapsed(), None)],
        );
    }
    let mut visited_out: Vec<String> = visited.into_iter().collect();
    visited_out.sort();
    let forward_request = TraceRequest {
        target: req.target.clone(),
        ttl: Some(req.ttl - 1),
        trace_id: Some(req.trace_id),
        visited: Some(visited_out),
        budget_ms: Some(downstream.as_millis() as u64),
    };

    let processing = started.elapsed();
    let sent = Instant::now();
    let outcome = forwarder.forward(&next, &forward_request, remaining).await;
    let waited = sent.elapsed();
    match outcome {
        ForwardOutcome::Response(down) => {
            let leg = waited.saturating_sub(Duration::from_secs_f64(down.total_ms / 1000.0));
            let mut hops = vec![hop(processing, Some(leg))];
            hops.extend(down.hops);
            finish(down.reached, down.stopped_reason, down.detail, hops)
        }
        ForwardOutcome::Timeout => finish(
            false,
            Some(StopReason::Timeout),
            None,
            vec![hop(processing, Some(waited))],
        ),
        ForwardOutcome::Unreachable(why) => finish(
            false,
            Some(StopReason::TargetUnreachable),
            Some(why.to_string()),
            vec![hop(processing, Some(waited))],
        ),
    }
}

fn clip(s: &str) -> String {
    s.chars().take(MAX_FIELD_LEN).collect()
}

fn finite(v: f64) -> f64 {
    if v.is_finite() && v >= 0.0 {
        v
    } else {
        0.0
    }
}

/// Bounds a downstream response, which is untrusted: same trace id, a
/// plausible hop count, clipped strings, finite non-negative durations.
pub fn sanitize(mut resp: TraceResponse, expected: Uuid, max_hops: usize) -> Option<TraceResponse> {
    if resp.trace_id != expected || resp.hops.is_empty() || resp.hops.len() > max_hops {
        return None;
    }
    resp.target = clip(&resp.target);
    resp.detail = resp.detail.map(|d| clip(&d));
    resp.total_ms = finite(resp.total_ms);
    for h in &mut resp.hops {
        h.base_url = clip(&h.base_url);
        h.protocol_version = clip(&h.protocol_version);
        h.roles.truncate(MAX_ROLES);
        h.roles = h.roles.iter().map(|r| clip(r)).collect();
        h.processing_ms = finite(h.processing_ms);
        h.to_next_ms = h.to_next_ms.map(finite);
    }
    Some(resp)
}

/// Forwards over HTTP to a neighbor after the outbound policy check.
pub struct HttpForwarder {
    pub policy: OutboundPolicy,
}

impl Forwarder for HttpForwarder {
    fn forward(
        &self,
        neighbor: &OverlayNode,
        request: &TraceRequest,
        timeout: Duration,
    ) -> impl std::future::Future<Output = ForwardOutcome> + Send {
        let policy = self.policy;
        let url = neighbor.base_url.clone();
        let body = serde_json::to_vec(request).unwrap_or_default();
        let trace_id = request.trace_id.unwrap_or_default();
        let max_hops = request.ttl.unwrap_or(0) as usize + 1;
        async move {
            let attempt = async {
                let checked = match policy.check_base_url(&url).await {
                    Ok(c) => c,
                    Err(_) => return ForwardOutcome::Unreachable("outbound_policy"),
                };
                let mut response = match checked
                    .client(timeout)
                    .post(format!("{}/nodes/trace", checked.base_url))
                    .header("content-type", "application/json")
                    .body(body)
                    .send()
                    .await
                {
                    Ok(r) => r,
                    Err(e) if e.is_timeout() => return ForwardOutcome::Timeout,
                    Err(_) => return ForwardOutcome::Unreachable("connect"),
                };
                match response.status() {
                    s if s.is_success() => {}
                    StatusCode::TOO_MANY_REQUESTS => {
                        return ForwardOutcome::Unreachable("rate_limited")
                    }
                    _ => return ForwardOutcome::Unreachable("bad_status"),
                }
                let mut bytes = Vec::new();
                loop {
                    match response.chunk().await {
                        Ok(Some(chunk)) => {
                            bytes.extend_from_slice(&chunk);
                            if bytes.len() > MAX_RESPONSE_BYTES {
                                return ForwardOutcome::Unreachable("response_too_large");
                            }
                        }
                        Ok(None) => break,
                        Err(e) if e.is_timeout() => return ForwardOutcome::Timeout,
                        Err(_) => return ForwardOutcome::Unreachable("connect"),
                    }
                }
                match serde_json::from_slice::<TraceResponse>(&bytes)
                    .ok()
                    .and_then(|r| sanitize(r, trace_id, max_hops))
                {
                    Some(r) => ForwardOutcome::Response(r),
                    None => ForwardOutcome::Unreachable("bad_response"),
                }
            };
            tokio::time::timeout(timeout, attempt)
                .await
                .unwrap_or(ForwardOutcome::Timeout)
        }
    }
}

fn limits() -> &'static EndpointLimits {
    static LIMITS: OnceLock<EndpointLimits> = OnceLock::new();
    LIMITS.get_or_init(|| {
        EndpointLimits::from_env(
            "AVALON_TRACE_RATE_LIMIT_PER_MINUTE",
            60,
            "AVALON_TRACE_MAX_CONCURRENT",
            16,
        )
    })
}

/// Trace the overlay route from this node to a target, hop by hop.
///
/// Each node on the path reports itself, so the result is advisory and not a
/// verified fact about the path. Requests forwarded between nodes use the same
/// route with `visited` and `budget_ms` set.
#[utoipa::path(
    post,
    path = "/nodes/trace",
    tag = "nodes",
    request_body = TraceRequest,
    responses(
        (status = 200, body = TraceResponse),
        (status = 400, description = "Invalid target"),
        (status = 429, description = "Rate limit or in-flight cap hit; see Retry-After"),
        (status = 503, description = "This node has no AVALON_NODE_URL"),
    ),
)]
pub async fn trace(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<TraceRequest>,
) -> Result<Json<TraceResponse>, TopologyError> {
    let started = Instant::now();
    let limits = limits();
    limits.admit(client_ip(peer, &headers))?;
    let req = normalize(body)?;
    let own_url = state.own_base_url.clone().ok_or_else(|| {
        TopologyError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "node_url_not_configured",
            "this node has no AVALON_NODE_URL and cannot identify itself in a trace",
        )
    })?;
    let _permit = limits.in_flight.try_enter()?;

    let network_id = state.chain.network_id().to_string();
    let known = state.peers.list_all();
    let overlay = |url: &str| {
        let wanted = canonical_base_url(url);
        match known
            .iter()
            .find(|p| canonical_base_url(&p.base_url) == wanted)
        {
            Some(p) => OverlayNode {
                base_url: p.base_url.clone(),
                network_id: p.network_id.clone(),
                libp2p_peer_id: p.libp2p_peer_id.clone(),
            },
            None => OverlayNode {
                base_url: url.to_string(),
                network_id: network_id.clone(),
                libp2p_peer_id: None,
            },
        }
    };
    let ctx = TraceContext {
        me: OverlayNode {
            base_url: own_url,
            network_id: network_id.clone(),
            libp2p_peer_id: state.own_libp2p_peer_id.clone(),
        },
        roles: crate::nodes::node_roles(),
        protocol_version: crate::version::PROTOCOL_VERSION.to_string(),
        target: overlay(&req.target),
        neighbors: state
            .peers
            .neighbors()
            .snapshot()
            .iter()
            .map(|n| overlay(&n.base_url))
            .collect(),
    };
    let forwarder = HttpForwarder {
        policy: OutboundPolicy::from_env(),
    };
    Ok(Json(run_trace(&ctx, &forwarder, &req, started).await))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn node(url: &str) -> OverlayNode {
        OverlayNode {
            base_url: url.into(),
            network_id: "net".into(),
            libp2p_peer_id: None,
        }
    }

    fn ctx(me: &str, target: &str, neighbors: &[&str]) -> TraceContext {
        TraceContext {
            me: node(me),
            roles: vec!["combined".into()],
            protocol_version: "1.0.0".into(),
            target: node(target),
            neighbors: neighbors.iter().map(|n| node(n)).collect(),
        }
    }

    fn req(target: &str, ttl: u8, visited: &[&str], budget: Duration) -> NormalizedTrace {
        NormalizedTrace {
            target: canonical_base_url(target),
            ttl,
            trace_id: Uuid::nil(),
            visited: visited.iter().map(|v| canonical_base_url(v)).collect(),
            budget,
        }
    }

    fn hop_of(url: &str, to_next: Option<f64>) -> TraceHop {
        TraceHop {
            index: 0,
            base_url: url.into(),
            roles: vec!["combined".into()],
            protocol_version: "1.0.0".into(),
            processing_ms: 1.0,
            to_next_ms: to_next,
        }
    }

    /// Answers every forward from a fixed script.
    struct Fake {
        seen: Mutex<Vec<(String, TraceRequest, Duration)>>,
        answer: Mutex<HashMap<String, ForwardOutcome>>,
    }

    impl Fake {
        fn with(url: &str, outcome: ForwardOutcome) -> Self {
            Self {
                seen: Mutex::new(vec![]),
                answer: Mutex::new(HashMap::from([(canonical_base_url(url), outcome)])),
            }
        }
    }

    impl Forwarder for Fake {
        fn forward(
            &self,
            neighbor: &OverlayNode,
            request: &TraceRequest,
            timeout: Duration,
        ) -> impl std::future::Future<Output = ForwardOutcome> + Send {
            self.seen.lock().unwrap().push((
                neighbor.base_url.clone(),
                TraceRequest {
                    target: request.target.clone(),
                    ttl: request.ttl,
                    trace_id: request.trace_id,
                    visited: request.visited.clone(),
                    budget_ms: request.budget_ms,
                },
                timeout,
            ));
            let out = self
                .answer
                .lock()
                .unwrap()
                .remove(&canonical_base_url(&neighbor.base_url))
                .unwrap_or(ForwardOutcome::Unreachable("connect"));
            async move {
                if let ForwardOutcome::Response(_) = &out {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                out
            }
        }
    }

    fn down(hops: Vec<TraceHop>, reached: bool, reason: Option<StopReason>) -> ForwardOutcome {
        ForwardOutcome::Response(TraceResponse {
            trace_id: Uuid::nil(),
            target: "http://c".into(),
            reached,
            stopped_reason: reason,
            detail: None,
            total_ms: 2.0,
            hops,
        })
    }

    #[tokio::test]
    async fn target_is_self_reports_reached_with_one_hop() {
        let c = ctx("http://a", "http://A/", &[]);
        let f = Fake::with("http://x", ForwardOutcome::Timeout);
        let r = run_trace(
            &c,
            &f,
            &req("http://a", 5, &[], DEFAULT_BUDGET),
            Instant::now(),
        )
        .await;
        assert!(r.reached && r.stopped_reason.is_none());
        assert_eq!(r.hops.len(), 1);
        assert_eq!(r.hops[0].index, 0);
        assert!(r.hops[0].to_next_ms.is_none());
        assert!(f.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn own_url_in_visited_is_a_loop() {
        let c = ctx("http://a", "http://z", &["http://b"]);
        let f = Fake::with("http://b", ForwardOutcome::Timeout);
        let r = run_trace(
            &c,
            &f,
            &req("http://z", 5, &["http://A"], DEFAULT_BUDGET),
            Instant::now(),
        )
        .await;
        assert_eq!(r.stopped_reason, Some(StopReason::Loop));
        assert!(!r.reached);
        assert!(f.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn zero_ttl_stops_without_forwarding() {
        let c = ctx("http://a", "http://z", &["http://z"]);
        let f = Fake::with("http://z", ForwardOutcome::Timeout);
        let r = run_trace(
            &c,
            &f,
            &req("http://z", 0, &[], DEFAULT_BUDGET),
            Instant::now(),
        )
        .await;
        assert_eq!(r.stopped_reason, Some(StopReason::Ttl));
        assert!(f.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn no_route_reports_the_reason() {
        let c = ctx("http://a", "http://z", &[]);
        let f = Fake::with("http://x", ForwardOutcome::Timeout);
        let r = run_trace(
            &c,
            &f,
            &req("http://z", 5, &[], DEFAULT_BUDGET),
            Instant::now(),
        )
        .await;
        assert_eq!(r.stopped_reason, Some(StopReason::NoRoute));
        assert_eq!(r.detail.as_deref(), Some("no_neighbors"));
        assert_eq!(r.hops.len(), 1);
    }

    #[tokio::test]
    async fn forward_carries_decremented_ttl_visited_and_smaller_budget() {
        let c = ctx("http://a", "http://z", &["http://z"]);
        let f = Fake::with("http://z", down(vec![hop_of("http://z", None)], true, None));
        let budget = Duration::from_secs(4);
        let r = run_trace(
            &c,
            &f,
            &req("http://z", 5, &["http://p"], budget),
            Instant::now(),
        )
        .await;
        let seen = f.seen.lock().unwrap();
        let (to, sent, timeout) = &seen[0];
        assert_eq!(to, "http://z");
        assert_eq!(sent.ttl, Some(4));
        assert_eq!(
            sent.visited.as_ref().unwrap(),
            &vec!["http://a".to_string(), "http://p".to_string()]
        );
        let handed = Duration::from_millis(sent.budget_ms.unwrap());
        assert!(handed < budget && handed + DOWNSTREAM_MARGIN <= budget);
        assert!(*timeout <= budget && *timeout > handed);
        assert!(r.reached);
    }

    /// A neighbor URL the routing rule forwards through (strictly closer to
    /// `target` than `me`, not the target itself).
    fn forwarding_neighbor(me: &str, target: &str) -> String {
        (0..500)
            .map(|i| format!("http://n{i}"))
            .find(|url| {
                matches!(
                    next_hop(&node(me), &node(target), &[node(url)], &HashSet::new()),
                    NextHop::Forward(_)
                )
            })
            .expect("some candidate is closer to the target")
    }

    #[tokio::test]
    async fn hops_are_assembled_in_order_with_reindexing_and_leg_time() {
        let mid = forwarding_neighbor("http://a", "http://c");
        let c = ctx("http://a", "http://c", &[&mid]);
        let downstream = vec![
            TraceHop {
                index: 0,
                ..hop_of(&mid, Some(0.5))
            },
            TraceHop {
                index: 1,
                ..hop_of("http://c", None)
            },
        ];
        let f = Fake::with(&mid, down(downstream, true, None));
        let r = run_trace(
            &c,
            &f,
            &req("http://c", 5, &[], DEFAULT_BUDGET),
            Instant::now(),
        )
        .await;
        let urls: Vec<_> = r.hops.iter().map(|h| h.base_url.as_str()).collect();
        assert_eq!(urls, ["http://a", mid.as_str(), "http://c"]);
        let idx: Vec<_> = r.hops.iter().map(|h| h.index).collect();
        assert_eq!(idx, [0, 1, 2]);
        assert!(r.reached);
        let leg = r.hops[0].to_next_ms.unwrap();
        assert!((0.0..1000.0).contains(&leg));
        assert_eq!(r.hops[1].to_next_ms, Some(0.5));
    }

    #[tokio::test]
    async fn downstream_stop_reason_propagates() {
        let c = ctx("http://a", "http://b", &["http://b"]);
        let f = Fake::with(
            "http://b",
            down(vec![hop_of("http://b", None)], false, Some(StopReason::Ttl)),
        );
        let r = run_trace(
            &c,
            &f,
            &req("http://b", 1, &[], DEFAULT_BUDGET),
            Instant::now(),
        )
        .await;
        assert_eq!(r.stopped_reason, Some(StopReason::Ttl));
        assert!(!r.reached);
    }

    #[tokio::test]
    async fn timeout_and_unreachable_end_the_path_at_this_hop() {
        let c = ctx("http://a", "http://b", &["http://b"]);
        let f = Fake::with("http://b", ForwardOutcome::Timeout);
        let r = run_trace(
            &c,
            &f,
            &req("http://b", 3, &[], DEFAULT_BUDGET),
            Instant::now(),
        )
        .await;
        assert_eq!(r.stopped_reason, Some(StopReason::Timeout));
        assert_eq!(r.hops.len(), 1);
        assert!(r.hops[0].to_next_ms.is_some());

        let f = Fake::with("http://b", ForwardOutcome::Unreachable("connect"));
        let r = run_trace(
            &c,
            &f,
            &req("http://b", 3, &[], DEFAULT_BUDGET),
            Instant::now(),
        )
        .await;
        assert_eq!(r.stopped_reason, Some(StopReason::TargetUnreachable));
        assert_eq!(r.detail.as_deref(), Some("connect"));
    }

    #[tokio::test]
    async fn exhausted_budget_times_out_without_forwarding() {
        let c = ctx("http://a", "http://b", &["http://b"]);
        let f = Fake::with("http://b", ForwardOutcome::Timeout);
        let r = run_trace(
            &c,
            &f,
            &req("http://b", 3, &[], Duration::from_millis(120)),
            Instant::now(),
        )
        .await;
        assert_eq!(r.stopped_reason, Some(StopReason::Timeout));
        assert!(f.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn normalize_clamps_untrusted_fields() {
        let n = normalize(TraceRequest {
            target: "http://T/".into(),
            ttl: Some(200),
            trace_id: None,
            visited: Some((0..100).map(|i| format!("http://V{i}/")).collect()),
            budget_ms: Some(u64::MAX),
        })
        .unwrap();
        assert_eq!(n.ttl, MAX_TTL);
        assert_eq!(n.budget, MAX_BUDGET);
        assert_eq!(n.visited.len(), MAX_VISITED);
        assert_eq!(n.visited[0], "http://v0");
        assert_eq!(n.target, "http://t");
        let d = normalize(TraceRequest {
            target: "http://t".into(),
            ttl: None,
            trace_id: None,
            visited: None,
            budget_ms: None,
        })
        .unwrap();
        assert_eq!((d.ttl, d.budget), (DEFAULT_TTL, DEFAULT_BUDGET));
        assert!(normalize(TraceRequest {
            target: "ftp://t".into(),
            ttl: None,
            trace_id: None,
            visited: None,
            budget_ms: None,
        })
        .is_err());
    }

    #[test]
    fn sanitize_rejects_wrong_id_and_too_many_hops_and_bounds_values() {
        let good = |n: usize| TraceResponse {
            trace_id: Uuid::nil(),
            target: "t".into(),
            reached: false,
            stopped_reason: None,
            detail: None,
            total_ms: f64::NAN,
            hops: (0..n)
                .map(|_| TraceHop {
                    processing_ms: -5.0,
                    roles: (0..50).map(|i| "r".repeat(1000 + i)).collect(),
                    ..hop_of(&"u".repeat(5000), Some(f64::INFINITY))
                })
                .collect(),
        };
        assert!(sanitize(good(2), Uuid::new_v4(), 5).is_none());
        assert!(sanitize(good(6), Uuid::nil(), 5).is_none());
        assert!(sanitize(good(0), Uuid::nil(), 5).is_none());
        let s = sanitize(good(2), Uuid::nil(), 5).unwrap();
        assert_eq!(s.total_ms, 0.0);
        assert_eq!(s.hops[0].base_url.len(), MAX_FIELD_LEN);
        assert_eq!(s.hops[0].roles.len(), MAX_ROLES);
        assert_eq!(s.hops[0].processing_ms, 0.0);
        assert_eq!(s.hops[0].to_next_ms, Some(0.0));
    }

    mod http {
        use super::*;
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn fwd(allow_private: bool) -> HttpForwarder {
            HttpForwarder {
                policy: OutboundPolicy::new(allow_private),
            }
        }

        fn request() -> TraceRequest {
            TraceRequest {
                target: "http://z".into(),
                ttl: Some(3),
                trace_id: Some(Uuid::nil()),
                visited: Some(vec!["http://a".into()]),
                budget_ms: Some(1000),
            }
        }

        async fn outcome(server: &MockServer, allow_private: bool) -> ForwardOutcome {
            fwd(allow_private)
                .forward(&node(&server.uri()), &request(), Duration::from_millis(500))
                .await
        }

        fn good_body() -> serde_json::Value {
            serde_json::to_value(TraceResponse {
                trace_id: Uuid::nil(),
                target: "http://z".into(),
                reached: true,
                stopped_reason: None,
                detail: None,
                total_ms: 1.0,
                hops: vec![hop_of("http://n", None)],
            })
            .unwrap()
        }

        #[tokio::test]
        async fn posts_the_internal_fields_and_parses_the_response() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/nodes/trace"))
                .and(body_partial_json(serde_json::json!({
                    "ttl": 3, "visited": ["http://a"], "budget_ms": 1000
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(good_body()))
                .mount(&server)
                .await;
            assert!(matches!(
                outcome(&server, true).await,
                ForwardOutcome::Response(r) if r.reached && r.hops.len() == 1
            ));
        }

        #[tokio::test]
        async fn refuses_private_neighbors_without_the_flag() {
            let server = MockServer::start().await;
            assert!(matches!(
                outcome(&server, false).await,
                ForwardOutcome::Unreachable("outbound_policy")
            ));
            assert!(server.received_requests().await.unwrap().is_empty());
        }

        #[tokio::test]
        async fn maps_failures_to_outcomes() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(429))
                .mount(&server)
                .await;
            assert!(matches!(
                outcome(&server, true).await,
                ForwardOutcome::Unreachable("rate_limited")
            ));

            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
                .mount(&server)
                .await;
            assert!(matches!(
                outcome(&server, true).await,
                ForwardOutcome::Unreachable("bad_response")
            ));

            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
                .mount(&server)
                .await;
            assert!(matches!(
                outcome(&server, true).await,
                ForwardOutcome::Timeout
            ));

            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(
                    ResponseTemplate::new(302).insert_header("location", "http://127.0.0.1:1/"),
                )
                .mount(&server)
                .await;
            assert!(matches!(
                outcome(&server, true).await,
                ForwardOutcome::Unreachable("bad_status")
            ));
        }

        #[tokio::test]
        async fn a_response_for_another_trace_is_rejected() {
            let server = MockServer::start().await;
            let mut body = good_body();
            body["trace_id"] = serde_json::json!(Uuid::new_v4());
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&server)
                .await;
            assert!(matches!(
                outcome(&server, true).await,
                ForwardOutcome::Unreachable("bad_response")
            ));
        }
    }
}
