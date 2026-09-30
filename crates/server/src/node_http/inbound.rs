//! Serving node-to-node requests that arrive over a libp2p stream.
//!
//! A request is dispatched into the same axum `Router` the HTTP listener serves, so
//! authentication, validation and rate limits behave identically. Only an explicit set of
//! node-to-node routes is reachable this way; user-facing routes are refused before dispatch.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, HeaderName, HeaderValue, Method, Request, Uri};
use axum::Router;
use libp2p::PeerId;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

use crate::nodes::PeerTable;
use crate::shutdown::Shutdown;

use super::wire::{check_headers, NodeHttpRequest, NodeHttpResponse, NodeHttpSettings};

/// The libp2p peer a stream request came from, authenticated by the noise handshake. Present as
/// an `Extension` only on requests that arrived over a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemotePeer(pub PeerId);

/// Routes a peer may call over a stream: exactly what node-to-node call sites use. Admin,
/// internal-role and every user-facing route are deliberately absent.
pub const ALLOWED_EXACT: &[&str] = &[
    "/nodes/announce",
    "/nodes/peers",
    "/nodes/discover",
    "/nodes/status",
    "/nodes/relay",
    "/nodes/probe",
    "/nodes/trace",
    "/nodes/topology",
    "/nodes/replicate-chat",
    "/mirror/notify",
    "/ledger/sth/latest",
    "/ledger/proof/consistency",
    "/ledger/proof/inclusion",
    "/ledger/entries",
    "/ledger/submit",
    "/ledger/prepare-batch",
    "/ledger/finalize-batch",
    "/ledger/cross-shard-root",
    "/ledger/remote-submit-status",
    "/ledger/mirror-progress",
];

/// Routes that write or inject data without their own credential. Over a stream they are
/// reserved for peers bound in the peer table; the rest stay open to any authenticated peer id.
pub const BOUND_ONLY_PATHS: &[&str] = &["/nodes/relay", "/nodes/replicate-chat", "/mirror/notify"];

/// Whether `path_and_query` names a route in [`BOUND_ONLY_PATHS`].
pub fn requires_bound_peer(path_and_query: &str) -> bool {
    let path = path_and_query.split('?').next().unwrap_or("");
    BOUND_ONLY_PATHS.contains(&path)
}

/// `/ledger/sth/{tree_size}`.
const ALLOWED_STH_PREFIX: &str = "/ledger/sth/";

/// Whether `path_and_query` names an allowed route. Paths with encoding, dot segments or
/// backslashes are refused so no alias can slip past the exact match.
pub fn path_allowed(path_and_query: &str) -> bool {
    let path = path_and_query.split('?').next().unwrap_or("");
    if path.contains(['%', '\\', '#']) || path.contains("//") || path.contains("/.") {
        return false;
    }
    if ALLOWED_EXACT.contains(&path) {
        return true;
    }
    path.strip_prefix(ALLOWED_STH_PREFIX)
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The one address every peer that is not bound in the peer table shares, so free peer ids
/// cannot multiply per-IP budgets. Unique-local, never loopback.
pub const SHARED_PEER_ADDR: SocketAddr =
    SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1)), 0);

/// The address handlers see for `peer`: its own stable unique-local address if it is a bound
/// peer-table entry (so the table cap bounds the buckets), else [`SHARED_PEER_ADDR`].
pub fn synthetic_addr(peer: &PeerId, bound: bool) -> SocketAddr {
    if !bound {
        return SHARED_PEER_ADDR;
    }
    let digest = Sha256::new()
        .chain_update(b"avalon-node-http-peer")
        .chain_update(peer.to_bytes())
        .finalize();
    let mut octets = [0u8; 16];
    octets[0] = 0xfd;
    octets[1..].copy_from_slice(&digest[..15]);
    SocketAddr::new(IpAddr::V6(Ipv6Addr::from(octets)), 0)
}

/// Set once the router exists, which is after the swarm starts; stream requests before that
/// get a 503.
#[derive(Clone, Default)]
pub struct RouterSlot {
    router: Arc<OnceLock<Router>>,
    shutdown: Arc<OnceLock<Shutdown>>,
}

impl RouterSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stream requests get a 503 once `shutdown` has been requested.
    pub fn set_shutdown(&self, shutdown: Shutdown) {
        let _ = self.shutdown.set(shutdown);
    }

    fn draining(&self) -> bool {
        self.shutdown.get().is_some_and(Shutdown::is_requested)
    }

    /// Publishes the router; a second call is ignored.
    pub fn set(&self, router: Router) {
        let _ = self.router.set(router);
    }

    pub fn get(&self) -> Option<&Router> {
        self.router.get()
    }
}

#[derive(Default)]
struct Counts {
    total: usize,
    per_peer: HashMap<PeerId, usize>,
}

/// Holds one inbound slot until dropped.
pub struct InboundPermit {
    peer: PeerId,
    counts: Arc<Mutex<Counts>>,
}

impl Drop for InboundPermit {
    fn drop(&mut self) {
        let mut c = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        c.total = c.total.saturating_sub(1);
        if let Some(n) = c.per_peer.get_mut(&self.peer) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                c.per_peer.remove(&self.peer);
            }
        }
    }
}

/// The inbound side: limits, the router slot and dispatch.
#[derive(Clone)]
pub struct InboundService {
    slot: RouterSlot,
    settings: NodeHttpSettings,
    counts: Arc<Mutex<Counts>>,
    peers: PeerTable,
}

/// Headers that never cross the stream: connection framing, and anything a proxy chain would
/// use to spoof the client address.
fn dropped_request_header(name: &str) -> bool {
    matches!(
        name,
        "host"
            | "content-length"
            | "connection"
            | "keep-alive"
            | "transfer-encoding"
            | "upgrade"
            | "te"
            | "trailer"
            | "proxy-authorization"
            | "proxy-connection"
            | "forwarded"
            | "x-forwarded-for"
            | "x-forwarded-host"
            | "x-forwarded-proto"
            | "x-real-ip"
    )
}

impl InboundService {
    pub fn new(slot: RouterSlot, settings: NodeHttpSettings, peers: PeerTable) -> Self {
        Self {
            slot,
            settings,
            counts: Arc::default(),
            peers,
        }
    }

    /// Takes a slot for `peer`, or the immediate 429 to answer with.
    pub fn admit(
        &self,
        peer: PeerId,
        request: &NodeHttpRequest,
    ) -> Result<InboundPermit, NodeHttpResponse> {
        if request.grant.overloaded {
            return Err(NodeHttpResponse::error(429, "buffer_budget_exhausted"));
        }
        let mut c = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        let mine = c.per_peer.get(&peer).copied().unwrap_or(0);
        if c.total >= self.settings.max_inflight || mine >= self.settings.max_inflight_per_peer {
            let mut res = NodeHttpResponse::error(429, "too_many_requests");
            res.headers.push(("retry-after".into(), "1".into()));
            return Err(res);
        }
        c.total += 1;
        *c.per_peer.entry(peer).or_insert(0) += 1;
        Ok(InboundPermit {
            peer,
            counts: self.counts.clone(),
        })
    }

    /// Answers `request` from `peer`, never panicking on hostile input.
    pub async fn handle(&self, peer: PeerId, request: NodeHttpRequest) -> NodeHttpResponse {
        let method = match request.method.as_str() {
            "GET" => Method::GET,
            "POST" => Method::POST,
            _ => return NodeHttpResponse::error(405, "method_not_allowed"),
        };
        if !path_allowed(&request.path_and_query) {
            return NodeHttpResponse::error(403, "path_not_allowed");
        }
        let bound = self.peers.is_bound_libp2p_peer(&peer);
        if requires_bound_peer(&request.path_and_query) && !bound {
            return NodeHttpResponse::error(403, "peer_not_bound");
        }
        if self.slot.draining() {
            return NodeHttpResponse::error(503, "shutting_down");
        }
        let Some(router) = self.slot.get() else {
            return NodeHttpResponse::error(503, "node_starting");
        };
        let Ok(req) = build_request(method, peer, bound, request) else {
            return NodeHttpResponse::error(400, "bad_request");
        };
        let dispatch = async {
            let response = router
                .clone()
                .oneshot(req)
                .await
                .unwrap_or_else(|e| match e {});
            let (parts, body) = response.into_parts();
            let Ok(bytes) = axum::body::to_bytes(body, self.settings.max_response_bytes).await
            else {
                return NodeHttpResponse::error(502, "response_too_large");
            };
            let headers: Vec<(String, String)> = parts
                .headers
                .iter()
                .filter(|(k, _)| {
                    !matches!(
                        k.as_str(),
                        "connection" | "keep-alive" | "transfer-encoding"
                    )
                })
                .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
                .collect();
            if check_headers(&headers).is_err() {
                return NodeHttpResponse::error(502, "response_headers_too_large");
            }
            NodeHttpResponse {
                status: parts.status.as_u16(),
                headers,
                body: bytes.to_vec(),
            }
        };
        tokio::time::timeout(self.settings.timeout, dispatch)
            .await
            .unwrap_or_else(|_| NodeHttpResponse::error(504, "timeout"))
    }
}

fn build_request(
    method: Method,
    peer: PeerId,
    bound: bool,
    request: NodeHttpRequest,
) -> Result<Request<Body>, ()> {
    let uri: Uri = request.path_and_query.parse().map_err(|_| ())?;
    if uri.scheme().is_some() || uri.authority().is_some() {
        return Err(());
    }
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in &request.headers {
        let lower = name.to_ascii_lowercase();
        if dropped_request_header(&lower) {
            continue;
        }
        let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(lower.as_bytes()),
            HeaderValue::from_str(value),
        ) else {
            return Err(());
        };
        builder = builder.header(n, v);
    }
    builder = builder.header(header::CONTENT_LENGTH, request.body.len());
    let mut req = builder.body(Body::from(request.body)).map_err(|_| ())?;
    req.extensions_mut().insert(RemotePeer(peer));
    req.extensions_mut()
        .insert(ConnectInfo(synthetic_addr(&peer, bound)));
    Ok(req)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_admits_node_routes_and_refuses_the_rest() {
        for ok in [
            "/nodes/announce",
            "/nodes/status?x=1",
            "/ledger/sth/latest?shard_id=core",
            "/ledger/sth/42",
            "/ledger/entries?from=1",
            "/ledger/prepare-batch",
            "/mirror/notify",
            "/nodes/trace",
        ] {
            assert!(path_allowed(ok), "{ok}");
        }
        for bad in [
            "/nodes/log-level",
            "/internal/indexer/apply",
            "/auth/login",
            "/conversations",
            "/ledger/sth/",
            "/ledger/sth/abc",
            "/ledger/sth/1/2",
            "/ledger/sth/latest/../../auth",
            "/nodes/status/../log-level",
            "/nodes/%73tatus",
            "//nodes/status",
            "/nodes//status",
            "/nodes/statusx",
            "nodes/status",
            "",
        ] {
            assert!(!path_allowed(bad), "{bad}");
        }
    }

    fn bound_table(peer: &PeerId) -> PeerTable {
        let table = PeerTable::new();
        table.upsert(crate::nodes::PeerInfo {
            base_url: "http://bound.test".into(),
            roles: vec![],
            protocol_version: "0.1.0".into(),
            network_id: "n".into(),
            last_announced_at: time::OffsetDateTime::now_utc(),
            libp2p_peer_id: Some(peer.to_string()),
            libp2p_listen_addrs: vec![],
            witness: None,
            connectivity: None,
            identity_bound: true,
        });
        table
    }

    #[test]
    fn unbound_peers_share_one_address_and_a_bound_peer_has_its_own() {
        let bound = PeerId::random();
        let table = bound_table(&bound);
        let addr_for = |p: &PeerId| synthetic_addr(p, table.is_bound_libp2p_peer(p));
        let strangers: std::collections::HashSet<_> =
            (0..50).map(|_| addr_for(&PeerId::random())).collect();
        assert_eq!(strangers, [SHARED_PEER_ADDR].into());
        assert_ne!(addr_for(&bound), SHARED_PEER_ADDR);
        assert_eq!(addr_for(&bound), synthetic_addr(&bound, true));
        for ip in [addr_for(&bound).ip(), SHARED_PEER_ADDR.ip()] {
            assert!(!ip.is_loopback() && !ip.is_unspecified());
            let IpAddr::V6(v6) = ip else { panic!("ipv6") };
            assert_eq!(v6.octets()[0], 0xfd);
        }
    }

    #[test]
    fn write_routes_require_a_bound_peer() {
        for p in [
            "/nodes/relay",
            "/nodes/replicate-chat?x=1",
            "/mirror/notify",
        ] {
            assert!(requires_bound_peer(p), "{p}");
        }
        for p in [
            "/nodes/announce",
            "/nodes/status",
            "/ledger/submit",
            "/nodes/trace",
        ] {
            assert!(!requires_bound_peer(p), "{p}");
        }
        for p in BOUND_ONLY_PATHS {
            assert!(path_allowed(p), "{p} must also be allowlisted");
        }
    }

    fn req() -> NodeHttpRequest {
        NodeHttpRequest {
            method: "GET".into(),
            path_and_query: "/nodes/status".into(),
            headers: vec![],
            body: vec![],
            grant: Default::default(),
        }
    }

    #[test]
    fn limits_apply_per_peer_and_in_total_and_release_on_drop() {
        let settings = NodeHttpSettings {
            max_inflight: 3,
            max_inflight_per_peer: 2,
            ..NodeHttpSettings::default()
        };
        let svc = InboundService::new(RouterSlot::new(), settings, PeerTable::new());
        let (a, b, c) = (PeerId::random(), PeerId::random(), PeerId::random());
        let a1 = svc.admit(a, &req()).unwrap();
        let _a2 = svc.admit(a, &req()).unwrap();
        assert_eq!(svc.admit(a, &req()).err().unwrap().status, 429);
        let _b1 = svc.admit(b, &req()).unwrap();
        // Total of 3 reached: a fresh peer is refused too.
        assert_eq!(svc.admit(c, &req()).err().unwrap().status, 429);
        drop(a1);
        assert!(svc.admit(c, &req()).is_ok());
    }

    #[tokio::test]
    async fn requests_are_refused_before_dispatch_and_before_the_router_exists() {
        let svc = InboundService::new(
            RouterSlot::new(),
            NodeHttpSettings::default(),
            PeerTable::new(),
        );
        let peer = PeerId::random();
        let req = |method: &str, path: &str| NodeHttpRequest {
            method: method.into(),
            path_and_query: path.into(),
            headers: vec![],
            body: vec![],
            grant: Default::default(),
        };
        assert_eq!(
            svc.handle(peer, req("DELETE", "/nodes/status"))
                .await
                .status,
            405
        );
        assert_eq!(
            svc.handle(peer, req("GET", "/auth/login")).await.status,
            403
        );
        assert_eq!(
            svc.handle(peer, req("GET", "/nodes/status")).await.status,
            503
        );
    }

    #[tokio::test]
    async fn write_routes_are_refused_for_unbound_peers_and_everything_for_a_draining_node() {
        let bound = PeerId::random();
        let slot = RouterSlot::new();
        slot.set(Router::new().route("/nodes/relay", axum::routing::post(|| async { "ok" })));
        let svc = InboundService::new(
            slot.clone(),
            NodeHttpSettings::default(),
            bound_table(&bound),
        );
        let post = |path: &str| NodeHttpRequest {
            method: "POST".into(),
            path_and_query: path.into(),
            headers: vec![],
            body: vec![],
            grant: Default::default(),
        };
        let stranger = PeerId::random();
        assert_eq!(svc.handle(stranger, post("/nodes/relay")).await.status, 403);
        assert_eq!(svc.handle(bound, post("/nodes/relay")).await.status, 200);

        let (tx, shutdown) = Shutdown::manual();
        slot.set_shutdown(shutdown);
        assert_eq!(svc.handle(bound, post("/nodes/relay")).await.status, 200);
        tx.send(true).unwrap();
        assert_eq!(svc.handle(bound, post("/nodes/relay")).await.status, 503);
    }

    #[test]
    fn an_exhausted_buffer_budget_answers_429_before_taking_a_slot() {
        let svc = InboundService::new(
            RouterSlot::new(),
            NodeHttpSettings::default(),
            PeerTable::new(),
        );
        let mut r = req();
        r.grant.overloaded = true;
        assert_eq!(svc.admit(PeerId::random(), &r).err().unwrap().status, 429);
    }
}
