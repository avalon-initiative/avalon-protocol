//! Opt-in path tracing for real node-to-node operations.
//!
//! A request carrying `X-Avalon-Trace: <uuid>` asks each node that handles or
//! forwards it to report a hop. Nodes answer in the `X-Avalon-Trace-Hops`
//! response header: unpadded URL-safe base64 of an [`OpTrace`] JSON document.
//! A request without the header, or any request when `AVALON_TOPOLOGY_PUBLIC`
//! is false, is handled exactly as before.
//!
//! Hop entries use the same public fields as `POST /nodes/trace`. A forwarding
//! node prepends its own hop to each path the downstream node returned, so the
//! originating client reads whole paths from its final response. Fan-out
//! reports one branch per target. Branch count, hops per branch and the encoded
//! header size are capped; excess is dropped and flagged, never an error.
//!
//! Everything received from a downstream node is untrusted and sanitized, and
//! trace data can never fail the operation it rides on. Hops are self-reported
//! and advisory.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::state::AppState;
use crate::topology_trace::{clip, finite, TraceHop, MAX_ROLES};

pub const TRACE_HEADER: &str = "x-avalon-trace";
pub const HOPS_HEADER: &str = "x-avalon-trace-hops";
pub const MAX_BRANCHES: usize = 8;
pub const MAX_HOPS_PER_BRANCH: usize = 8;
pub const MAX_HEADER_BYTES: usize = 8 * 1024;
/// Bound on waiting for a downstream node when a trace makes a handler wait
/// for a relay it would otherwise not wait for.
pub const TRACED_FORWARD_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PENDING: usize = 256;
const STORED_TTL: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchOutcome {
    #[default]
    Ok,
    Timeout,
    Unreachable,
}

/// One path from the originating node to a node that handled the operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpBranch {
    /// Base URL the originating node sent to; empty for a node's own record.
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub outcome: BranchOutcome,
    pub hops: Vec<TraceHop>,
}

/// The decoded value of the `X-Avalon-Trace-Hops` header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpTrace {
    pub trace_id: Uuid,
    pub branches: Vec<OpBranch>,
    /// Set when branches, hops or bytes were dropped to stay within the caps.
    #[serde(default)]
    pub truncated: bool,
}

/// How a node describes itself in a hop.
#[derive(Debug, Clone)]
pub struct HopIdentity {
    pub base_url: String,
    pub roles: Vec<String>,
    pub protocol_version: String,
}

impl HopIdentity {
    pub fn from_state(state: &AppState) -> Option<Self> {
        Some(Self {
            base_url: state.own_base_url.clone()?,
            roles: crate::nodes::node_roles(),
            protocol_version: crate::version::PROTOCOL_VERSION.to_string(),
        })
    }

    pub fn hop(&self, processing: Duration, to_next: Option<Duration>) -> TraceHop {
        TraceHop {
            index: 0,
            base_url: self.base_url.clone(),
            roles: self.roles.clone(),
            protocol_version: self.protocol_version.clone(),
            processing_ms: processing.as_secs_f64() * 1000.0,
            to_next_ms: to_next.map(|d| d.as_secs_f64() * 1000.0),
        }
    }
}

#[derive(Default)]
struct ScopeState {
    participated: bool,
    branches: Vec<OpBranch>,
    truncated: bool,
    stored: Option<OpTrace>,
}

/// Per-request trace state, alive for one traced request on this node.
pub struct TraceScope {
    pub id: Uuid,
    pub started: Instant,
    pub identity: HopIdentity,
    state: Mutex<ScopeState>,
}

tokio::task_local! {
    static SCOPE: Arc<TraceScope>;
}

/// The trace scope of the request being handled, if it is traced.
pub fn current() -> Option<Arc<TraceScope>> {
    SCOPE.try_with(Arc::clone).ok()
}

/// Marks the current request as one this node reports a hop for.
pub fn participate() {
    if let Some(scope) = current() {
        scope.lock().participated = true;
    }
}

impl TraceScope {
    fn new(id: Uuid, identity: HopIdentity) -> Self {
        Self {
            id,
            started: Instant::now(),
            identity,
            state: Mutex::new(ScopeState::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ScopeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records the branches a fan-out produced; marks the request participating.
    pub fn add_branches(&self, branches: Vec<OpBranch>, truncated: bool) {
        let mut st = self.lock();
        st.participated = true;
        st.truncated |= truncated;
        for b in branches {
            if st.branches.len() >= MAX_BRANCHES {
                st.truncated = true;
                break;
            }
            st.branches.push(b);
        }
    }

    fn set_stored(&self, trace: OpTrace) {
        let mut st = self.lock();
        st.participated = true;
        st.stored = Some(trace);
    }

    fn finish(&self) -> Option<OpTrace> {
        let mut st = self.lock();
        if !st.participated {
            return None;
        }
        if let Some(stored) = st.stored.take() {
            return Some(stored);
        }
        let mut branches = std::mem::take(&mut st.branches);
        if branches.is_empty() {
            branches.push(OpBranch {
                target: String::new(),
                outcome: BranchOutcome::Ok,
                hops: vec![self.identity.hop(self.started.elapsed(), None)],
            });
        }
        Some(OpTrace {
            trace_id: self.id,
            branches,
            truncated: st.truncated,
        })
    }
}

fn reindex(branches: &mut [OpBranch]) {
    for b in branches {
        for (i, h) in b.hops.iter_mut().enumerate() {
            h.index = i as u32;
        }
    }
}

/// Serializes within [`MAX_HEADER_BYTES`], dropping trailing branches, then
/// trailing hops, and setting `truncated`.
pub fn encode(trace: &OpTrace) -> Option<HeaderValue> {
    let mut t = trace.clone();
    if t.branches.len() > MAX_BRANCHES {
        t.branches.truncate(MAX_BRANCHES);
        t.truncated = true;
    }
    for b in &mut t.branches {
        if b.hops.len() > MAX_HOPS_PER_BRANCH {
            b.hops.truncate(MAX_HOPS_PER_BRANCH);
            t.truncated = true;
        }
    }
    reindex(&mut t.branches);
    loop {
        let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&t).ok()?);
        if encoded.len() <= MAX_HEADER_BYTES {
            return HeaderValue::from_str(&encoded).ok();
        }
        t.truncated = true;
        if t.branches.len() > 1 {
            t.branches.pop();
        } else if let Some(b) = t.branches.first_mut().filter(|b| b.hops.len() > 1) {
            b.hops.pop();
        } else {
            t.branches.clear();
        }
    }
}

/// Decodes and sanitizes a header from a downstream node. `None` means the
/// data is malformed, oversized or for another trace, and is ignored.
pub fn decode(value: &str, expected: Uuid) -> Option<OpTrace> {
    if value.len() > MAX_HEADER_BYTES {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(value).ok()?;
    let mut t: OpTrace = serde_json::from_slice(&bytes).ok()?;
    if t.trace_id != expected || t.branches.is_empty() {
        return None;
    }
    if t.branches.len() > MAX_BRANCHES {
        t.branches.truncate(MAX_BRANCHES);
        t.truncated = true;
    }
    t.branches.retain(|b| !b.hops.is_empty());
    if t.branches.is_empty() {
        return None;
    }
    for b in &mut t.branches {
        b.target = clip(&b.target);
        if b.hops.len() > MAX_HOPS_PER_BRANCH {
            b.hops.truncate(MAX_HOPS_PER_BRANCH);
            t.truncated = true;
        }
        for h in &mut b.hops {
            h.base_url = clip(&h.base_url);
            h.protocol_version = clip(&h.protocol_version);
            h.roles.truncate(MAX_ROLES);
            h.roles = h.roles.iter().map(|r| clip(r)).collect();
            h.processing_ms = finite(h.processing_ms);
            h.to_next_ms = h.to_next_ms.map(finite);
        }
    }
    reindex(&mut t.branches);
    Some(t)
}

/// What one forward to `target` produced.
pub enum Downstream {
    /// The node answered; `None` when its trace header was absent or invalid.
    Answered(Option<OpTrace>),
    Timeout,
    Unreachable,
}

/// Builds the branches for one forward: this node's hop, then each path the
/// downstream node reported. `processing` is this node's time before
/// forwarding, `waited` the round trip.
pub fn branches_for_forward(
    me: &HopIdentity,
    processing: Duration,
    waited: Duration,
    target: &str,
    down: Downstream,
) -> (Vec<OpBranch>, bool) {
    let mut truncated = false;
    let (outcome, paths) = match down {
        Downstream::Answered(Some(t)) => {
            truncated |= t.truncated;
            (
                BranchOutcome::Ok,
                t.branches.into_iter().map(|b| b.hops).collect(),
            )
        }
        Downstream::Answered(None) => (BranchOutcome::Ok, vec![vec![]]),
        Downstream::Timeout => (BranchOutcome::Timeout, vec![vec![]]),
        Downstream::Unreachable => (BranchOutcome::Unreachable, vec![vec![]]),
    };
    let mut out = Vec::new();
    for down_hops in paths {
        let leg = match down_hops.first() {
            Some(first) => {
                waited.saturating_sub(Duration::from_secs_f64(first.processing_ms / 1000.0))
            }
            None => waited,
        };
        let mut hops = vec![me.hop(processing, Some(leg))];
        hops.extend(down_hops);
        if hops.len() > MAX_HOPS_PER_BRANCH {
            hops.truncate(MAX_HOPS_PER_BRANCH);
            truncated = true;
        }
        if out.len() >= MAX_BRANCHES {
            truncated = true;
            break;
        }
        out.push(OpBranch {
            target: target.to_string(),
            outcome,
            hops,
        });
    }
    reindex(&mut out);
    (out, truncated)
}

/// Reads the trace header off a downstream response.
pub fn from_response(headers: &axum::http::HeaderMap, expected: Uuid) -> Option<OpTrace> {
    decode(headers.get(HOPS_HEADER)?.to_str().ok()?, expected)
}

async fn run(identity: Option<HopIdentity>, req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(TRACE_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v.trim()).ok());
    let (Some(id), Some(identity)) = (id, identity) else {
        return next.run(req).await;
    };
    let scope = Arc::new(TraceScope::new(id, identity));
    let mut response = SCOPE.scope(scope.clone(), next.run(req)).await;
    if let Some(value) = scope.finish().and_then(|t| encode(&t)) {
        response.headers_mut().insert(HOPS_HEADER, value);
    }
    response
}

async fn middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    run(HopIdentity::from_state(&state), req, next).await
}

/// Adds the trace layer when `AVALON_TOPOLOGY_PUBLIC` is true.
pub fn mount(router: Router, state: AppState) -> Router {
    if crate::topology_access::topology_public() {
        router.layer(axum::middleware::from_fn_with_state(state, middleware))
    } else {
        router
    }
}

#[cfg(test)]
fn layer_for_tests(router: Router, identity: Option<HopIdentity>) -> Router {
    router.layer(axum::middleware::from_fn(move |req, next| {
        run(identity.clone(), req, next)
    }))
}

/// A submit queued by a traced request, waiting for the outbox worker.
#[derive(Clone)]
pub struct PendingTrace {
    pub trace_id: Uuid,
    pub identity: HopIdentity,
    pub queued: Instant,
}

fn pending() -> &'static Mutex<HashMap<Uuid, PendingTrace>> {
    static P: OnceLock<Mutex<HashMap<Uuid, PendingTrace>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

fn stored() -> &'static Mutex<HashMap<Uuid, (Instant, OpTrace)>> {
    static S: OnceLock<Mutex<HashMap<Uuid, (Instant, OpTrace)>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn evict_oldest<V>(map: &mut HashMap<Uuid, V>, at: impl Fn(&V) -> Instant) {
    if map.len() >= MAX_PENDING {
        if let Some(oldest) = map.iter().min_by_key(|(_, v)| at(v)).map(|(k, _)| *k) {
            map.remove(&oldest);
        }
    }
}

/// The current request's trace, when it is traced: the id and identity an
/// outbox row should carry to its later submit.
pub fn pending_for_current() -> Option<PendingTrace> {
    let scope = current()?;
    Some(PendingTrace {
        trace_id: scope.id,
        identity: scope.identity.clone(),
        queued: Instant::now(),
    })
}

/// Registers an outbox row queued by a traced request.
pub fn register_pending(row_id: Uuid, p: PendingTrace) {
    let mut map = pending().lock().unwrap_or_else(|e| e.into_inner());
    evict_oldest(&mut map, |v| v.queued);
    map.insert(row_id, p);
}

/// The trace of the first traced row among `row_ids`, if any.
pub fn find_pending(row_ids: &[Uuid]) -> Option<PendingTrace> {
    let map = pending().lock().unwrap_or_else(|e| e.into_inner());
    row_ids.iter().find_map(|id| map.get(id).cloned())
}

pub fn clear_pending(row_ids: &[Uuid]) {
    let mut map = pending().lock().unwrap_or_else(|e| e.into_inner());
    for id in row_ids {
        map.remove(id);
    }
}

/// Keeps the outcome of a traced submit for later reads.
pub fn store_result(trace: OpTrace) {
    let mut map = stored().lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, (at, _)| at.elapsed() < STORED_TTL);
    evict_oldest(&mut map, |v| v.0);
    map.insert(trace.trace_id, (Instant::now(), trace));
}

/// Makes the current traced request answer with the stored submit path for
/// its trace id, if one exists.
pub fn attach_stored_result() {
    let Some(scope) = current() else { return };
    let map = stored().lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, t)) = map.get(&scope.id) {
        if at.elapsed() < STORED_TTL {
            scope.set_stored(t.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{get, post};
    use tower::ServiceExt;

    fn ident(url: &str) -> HopIdentity {
        HopIdentity {
            base_url: url.into(),
            roles: vec!["combined".into()],
            protocol_version: "1.0.0".into(),
        }
    }

    fn hop(url: &str, processing: f64) -> TraceHop {
        TraceHop {
            index: 0,
            base_url: url.into(),
            roles: vec!["combined".into()],
            protocol_version: "1.0.0".into(),
            processing_ms: processing,
            to_next_ms: None,
        }
    }

    fn one_branch(id: Uuid, hops: Vec<TraceHop>) -> OpTrace {
        OpTrace {
            trace_id: id,
            branches: vec![OpBranch {
                target: String::new(),
                outcome: BranchOutcome::Ok,
                hops,
            }],
            truncated: false,
        }
    }

    fn header_of(t: &OpTrace) -> String {
        encode(t).unwrap().to_str().unwrap().to_string()
    }

    #[test]
    fn round_trips_and_reindexes() {
        let id = Uuid::new_v4();
        let t = one_branch(id, vec![hop("http://a", 1.0), hop("http://b", 2.0)]);
        let d = decode(&header_of(&t), id).unwrap();
        assert_eq!(d.branches[0].hops[1].index, 1);
        assert_eq!(d.branches[0].hops[1].base_url, "http://b");
        assert!(!d.truncated);
    }

    #[test]
    fn hops_merge_in_travel_order() {
        let id = Uuid::new_v4();
        let down = one_branch(id, vec![hop("http://authority", 4.0)]);
        let (b, trunc) = branches_for_forward(
            &ident("http://gateway"),
            Duration::from_millis(2),
            Duration::from_millis(10),
            "http://authority",
            Downstream::Answered(Some(down)),
        );
        assert!(!trunc);
        assert_eq!(b.len(), 1);
        let urls: Vec<_> = b[0].hops.iter().map(|h| h.base_url.as_str()).collect();
        assert_eq!(urls, ["http://gateway", "http://authority"]);
        assert_eq!(b[0].hops[1].index, 1);
        let leg = b[0].hops[0].to_next_ms.unwrap();
        assert!((leg - 6.0).abs() < 0.01, "leg was {leg}");
    }

    #[test]
    fn failed_forward_still_reports_the_origin_hop() {
        let (b, _) = branches_for_forward(
            &ident("http://a"),
            Duration::from_millis(1),
            Duration::from_millis(5),
            "http://dead",
            Downstream::Unreachable,
        );
        assert_eq!(b[0].outcome, BranchOutcome::Unreachable);
        assert_eq!(b[0].hops.len(), 1);
        assert_eq!(b[0].target, "http://dead");
        assert_eq!(b[0].hops[0].to_next_ms, Some(5.0));
    }

    #[test]
    fn malformed_downstream_data_is_ignored() {
        let id = Uuid::new_v4();
        assert!(decode("not base64 !!", id).is_none());
        assert!(decode(&URL_SAFE_NO_PAD.encode(b"{\"x\":1}"), id).is_none());
        assert!(decode(&"a".repeat(MAX_HEADER_BYTES + 1), id).is_none());
        let other = one_branch(Uuid::new_v4(), vec![hop("http://a", 1.0)]);
        assert!(decode(&header_of(&other), id).is_none());
        let empty = one_branch(id, vec![]);
        assert!(decode(
            &URL_SAFE_NO_PAD.encode(serde_json::to_vec(&empty).unwrap()),
            id
        )
        .is_none());
    }

    #[test]
    fn downstream_values_are_bounded() {
        let id = Uuid::new_v4();
        let mut h = hop(&"x".repeat(1500), -1.0);
        h.to_next_ms = Some(-3.0);
        h.roles = vec!["r".repeat(200); 12];
        let raw = one_branch(id, vec![h]);
        let wire = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&raw).unwrap());
        assert!(wire.len() <= MAX_HEADER_BYTES);
        let d = decode(&wire, id).unwrap();
        let h = &d.branches[0].hops[0];
        assert_eq!(h.base_url.chars().count(), 256);
        assert_eq!(h.processing_ms, 0.0);
        assert_eq!(h.to_next_ms, Some(0.0));
        assert_eq!(h.roles.len(), MAX_ROLES);
    }

    #[test]
    fn excess_hops_are_truncated_and_flagged() {
        let id = Uuid::new_v4();
        let hops: Vec<_> = (0..12).map(|i| hop(&format!("http://n{i}"), 1.0)).collect();
        let d = decode(&header_of(&one_branch(id, hops)), id).unwrap();
        assert_eq!(d.branches[0].hops.len(), MAX_HOPS_PER_BRANCH);
        assert!(d.truncated);
    }

    #[test]
    fn excess_branches_are_truncated_and_flagged() {
        let id = Uuid::new_v4();
        let branch = OpBranch {
            target: String::new(),
            outcome: BranchOutcome::Ok,
            hops: vec![hop("http://a", 1.0)],
        };
        let t = OpTrace {
            trace_id: id,
            branches: vec![branch; 12],
            truncated: false,
        };
        let d = decode(&header_of(&t), id).unwrap();
        assert_eq!(d.branches.len(), MAX_BRANCHES);
        assert!(d.truncated);
    }

    #[test]
    fn oversized_output_is_cut_to_the_byte_cap() {
        let id = Uuid::new_v4();
        let mut big = hop(&"u".repeat(256), 1.0);
        big.roles = vec!["r".repeat(256); MAX_ROLES];
        let branch = OpBranch {
            target: "t".repeat(256),
            outcome: BranchOutcome::Ok,
            hops: vec![big; MAX_HOPS_PER_BRANCH],
        };
        let t = OpTrace {
            trace_id: id,
            branches: vec![branch; MAX_BRANCHES],
            truncated: false,
        };
        let v = encode(&t).unwrap();
        assert!(v.len() <= MAX_HEADER_BYTES);
        let d = decode(v.to_str().unwrap(), id).unwrap();
        assert!(d.truncated);
        assert!(d.branches.len() < MAX_BRANCHES);
    }

    fn app(identity: Option<HopIdentity>) -> Router {
        let base = Router::new()
            .route("/plain", get(|| async { ([("x-custom", "1")], "hello") }))
            .route(
                "/recv",
                post(|| async {
                    participate();
                    StatusCode::NO_CONTENT
                }),
            );
        layer_for_tests(base, identity)
    }

    async fn call(app: Router, method: &str, path: &str, trace: Option<&str>) -> Response {
        let mut b = Request::builder().method(method).uri(path);
        if let Some(t) = trace {
            b = b.header(TRACE_HEADER, t);
        }
        app.oneshot(b.body(Body::empty()).unwrap()).await.unwrap()
    }

    async fn snapshot(r: Response) -> (StatusCode, Vec<(String, String)>, Vec<u8>) {
        let status = r.status();
        let mut headers: Vec<_> = r
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_string()))
            .collect();
        headers.sort();
        let body = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, headers, body)
    }

    #[tokio::test]
    async fn untraced_and_non_participating_responses_are_unchanged() {
        let id = Uuid::new_v4().to_string();
        let bare = Router::new().route("/plain", get(|| async { ([("x-custom", "1")], "hello") }));
        let want = snapshot(call(bare.clone(), "GET", "/plain", None).await).await;
        let untraced =
            snapshot(call(app(Some(ident("http://n"))), "GET", "/plain", None).await).await;
        assert_eq!(untraced, want);
        // A traced request to a route that does not participate is unchanged too.
        let traced =
            snapshot(call(app(Some(ident("http://n"))), "GET", "/plain", Some(&id)).await).await;
        assert_eq!(traced, want);
        // A malformed trace id is ignored, not an error.
        let bad =
            snapshot(call(app(Some(ident("http://n"))), "GET", "/plain", Some("nope")).await).await;
        assert_eq!(bad, want);
        // Untraced participating route: no hop header.
        let r = call(app(Some(ident("http://n"))), "POST", "/recv", None).await;
        assert!(r.headers().get(HOPS_HEADER).is_none());
    }

    #[tokio::test]
    async fn traced_receiver_reports_its_hop() {
        let id = Uuid::new_v4();
        let r = call(
            app(Some(ident("http://n"))),
            "POST",
            "/recv",
            Some(&id.to_string()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        let t = from_response(r.headers(), id).unwrap();
        assert_eq!(t.branches.len(), 1);
        assert_eq!(t.branches[0].hops.len(), 1);
        assert_eq!(t.branches[0].hops[0].base_url, "http://n");
        assert!(t.branches[0].hops[0].to_next_ms.is_none());
    }

    #[tokio::test]
    async fn node_without_a_url_ignores_the_header() {
        let id = Uuid::new_v4().to_string();
        let r = call(app(None), "POST", "/recv", Some(&id)).await;
        assert!(r.headers().get(HOPS_HEADER).is_none());
    }

    #[tokio::test]
    async fn gate_off_ignores_the_header() {
        // `mount` only adds the layer when the topology group is public, so a
        // router without it is what a gated-off node serves.
        let base = Router::new().route(
            "/recv",
            post(|| async {
                participate();
                StatusCode::NO_CONTENT
            }),
        );
        let id = Uuid::new_v4().to_string();
        let r = call(base, "POST", "/recv", Some(&id)).await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        assert!(r.headers().get(HOPS_HEADER).is_none());
    }

    #[tokio::test]
    async fn fan_out_branches_are_reported_per_target() {
        let id = Uuid::new_v4();
        let app = Router::new().route(
            "/fan",
            post(|| async {
                let scope = current().unwrap();
                let me = scope.identity.clone();
                for t in ["http://x", "http://y"] {
                    let down = one_branch(scope.id, vec![hop(t, 1.0)]);
                    let (b, tr) = branches_for_forward(
                        &me,
                        Duration::from_millis(1),
                        Duration::from_millis(3),
                        t,
                        Downstream::Answered(Some(down)),
                    );
                    scope.add_branches(b, tr);
                }
                StatusCode::OK
            }),
        );
        let r = call(
            layer_for_tests(app, Some(ident("http://origin"))),
            "POST",
            "/fan",
            Some(&id.to_string()),
        )
        .await;
        let t = from_response(r.headers(), id).unwrap();
        assert_eq!(t.branches.len(), 2);
        assert_eq!(t.branches[1].hops[0].base_url, "http://origin");
        assert_eq!(t.branches[1].hops[1].base_url, "http://y");
    }
}
