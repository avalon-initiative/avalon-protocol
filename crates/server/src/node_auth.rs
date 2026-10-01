//! Who is calling a node-to-node write route.
//!
//! `POST /nodes/relay`, `POST /nodes/replicate-chat` and `POST /mirror/notify` are guarded by
//! [`require_node_auth`], which resolves the caller to an [`AuthenticatedNode`]:
//!
//! - over a libp2p stream the noise-authenticated peer id is the credential, and any
//!   `x-avalon-node-auth` header is ignored;
//! - over plain HTTP the signed header is mandatory (see `avalon_protocol::node_request`) and
//!   the signing key must hash to the claimed peer id.
//!
//! Either way the peer must have standing ([`PeerTable::standing`]) and stays within a per-key
//! request budget. A credential grants no authority beyond what the route itself checks.
//!
//! The replay cache lives in process memory: a restart, replicas sharing one identity key or a
//! forward clock step re-open up to [`MAX_SKEW_SECS`] for a captured request. Peer URLs with a
//! path prefix are not signed, so they are refused.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use avalon_protocol::node_request::{
    parse_node_request_header, verify_node_request_header, NodeRequestError, NodeRequestTarget,
    NODE_REQUEST_HEADER,
};
use axum::body::Body;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use libp2p::identity::{ed25519, PublicKey};
use libp2p::PeerId;

use crate::node_http::RemotePeer;
use crate::nodes::PeerTable;
use crate::topology_limits::TopologyError;

/// The routes that require a node credential.
pub const CREDENTIAL_PATHS: &[&str] = &["/nodes/relay", "/nodes/replicate-chat", "/mirror/notify"];

/// Largest accepted body per route, checked before the body is hashed.
pub const RELAY_MAX_BODY_BYTES: usize = 64 * 1024;
pub const REPLICATE_CHAT_MAX_BODY_BYTES: usize = 64 * 1024;
pub const MIRROR_NOTIFY_MAX_BODY_BYTES: usize = 1024;

/// How far a credential's timestamp may be from this node's clock.
pub const MAX_SKEW_SECS: i64 = 60;

/// A timestamp of `now + MAX_SKEW_SECS` stays valid until `now + 2 * MAX_SKEW_SECS`.
const REPLAY_RETENTION_SECS: i64 = 2 * MAX_SKEW_SECS + 1;

/// Requests per minute one key may make across the three routes, unless
/// `AVALON_NODE_AUTH_RATE_PER_MINUTE` says otherwise. It matches the per-IP limit the same
/// traffic passed before, so legitimate fan-out is not refused.
pub const DEFAULT_RATE_PER_MINUTE: u32 = crate::DEFAULT_RATE_LIMIT_PER_MINUTE as u32;

/// Most nonces held in total; the oldest go first when the cap is reached.
const MAX_REPLAY_ENTRIES: usize = 262_144;

/// Most keys with a request budget; keys only get one after passing the standing check.
const MAX_BUDGET_KEYS: usize = 16_384;

/// The node a node-to-node write request was authenticated as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthenticatedNode(pub PeerId);

impl<S: Send + Sync> FromRequestParts<S> for AuthenticatedNode {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AuthenticatedNode>()
            .copied()
            .ok_or(StatusCode::UNAUTHORIZED)
    }
}

/// Why a request was not authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeAuthError {
    /// Plain HTTP without a credential header.
    Missing,
    /// The header failed parsing or verification.
    Invalid(NodeRequestError),
    /// The signing key does not hash to the peer id the header claims.
    PeerMismatch,
    /// The nonce was already used by this signer.
    Replay,
    /// The authenticated peer has no bound entry in the peer table.
    NoStanding,
    /// The peer is over its request budget.
    RateLimited,
    BodyTooLarge,
}

impl NodeAuthError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Missing | Self::Invalid(_) | Self::PeerMismatch | Self::Replay => {
                StatusCode::UNAUTHORIZED
            }
            Self::NoStanding => StatusCode::FORBIDDEN,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::BodyTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        }
    }

    /// Stable snake_case code, used as the response `code` and in the log.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Missing => "node_auth_missing",
            Self::Invalid(NodeRequestError::Malformed(_)) => "node_auth_malformed",
            Self::Invalid(NodeRequestError::InvalidRequest(_)) => "node_auth_invalid_request",
            Self::Invalid(NodeRequestError::InvalidKey) => "node_auth_invalid_key",
            Self::Invalid(NodeRequestError::Stale) => "node_auth_stale",
            Self::Invalid(NodeRequestError::Future) => "node_auth_future",
            Self::Invalid(NodeRequestError::BadSignature) => "node_auth_bad_signature",
            Self::PeerMismatch => "node_auth_peer_mismatch",
            Self::Replay => "node_auth_replay",
            Self::NoStanding => "node_auth_no_standing",
            Self::RateLimited => "node_auth_rate_limited",
            Self::BodyTooLarge => "node_auth_body_too_large",
        }
    }

    fn into_response(self) -> Response {
        let message = match self {
            Self::NoStanding => "this node has no standing with the receiver",
            Self::RateLimited => "too many requests from this node, retry later",
            Self::BodyTooLarge => "request body too large for this route",
            _ => "node credential missing or invalid",
        };
        let mut error = TopologyError::new(self.status(), self.code(), message);
        if self == Self::RateLimited {
            error.retry_after_secs = Some(30);
        }
        error.into_response()
    }
}

/// Nonces seen from each signer, kept for [`REPLAY_RETENTION_SECS`].
struct ReplayCache {
    seen: HashSet<(PeerId, [u8; 16])>,
    /// Insertion order, which is expiry order: `(expires_at, signer, nonce)`.
    order: VecDeque<(i64, PeerId, [u8; 16])>,
    per_signer: HashMap<PeerId, usize>,
    max_total: usize,
    max_per_signer: usize,
}

impl ReplayCache {
    fn new(max_total: usize, max_per_signer: usize) -> Self {
        Self {
            seen: HashSet::new(),
            order: VecDeque::new(),
            per_signer: HashMap::new(),
            max_total,
            max_per_signer,
        }
    }

    fn forget_front(&mut self) {
        let Some((_, signer, nonce)) = self.order.pop_front() else {
            return;
        };
        self.seen.remove(&(signer, nonce));
        if let Some(n) = self.per_signer.get_mut(&signer) {
            *n -= 1;
            if *n == 0 {
                self.per_signer.remove(&signer);
            }
        }
    }

    /// Records `(signer, nonce)`; refuses a repeat, and a signer already holding its share.
    fn insert(&mut self, signer: PeerId, nonce: [u8; 16], now: i64) -> Result<(), NodeAuthError> {
        while self.order.front().is_some_and(|(exp, ..)| *exp <= now) {
            self.forget_front();
        }
        if self.seen.contains(&(signer, nonce)) {
            return Err(NodeAuthError::Replay);
        }
        if self.per_signer.get(&signer).copied().unwrap_or(0) >= self.max_per_signer {
            return Err(NodeAuthError::RateLimited);
        }
        if self.seen.len() >= self.max_total {
            self.forget_front();
        }
        self.seen.insert((signer, nonce));
        self.order
            .push_back((now.saturating_add(REPLAY_RETENTION_SECS), signer, nonce));
        *self.per_signer.entry(signer).or_insert(0) += 1;
        Ok(())
    }
}

/// Requests per key in the current minute.
struct Budget {
    windows: HashMap<PeerId, (i64, u32)>,
    per_minute: u32,
    max_keys: usize,
}

impl Budget {
    fn charge(&mut self, peer: PeerId, now: i64) -> bool {
        let minute = now.div_euclid(60);
        if self.windows.len() >= self.max_keys && !self.windows.contains_key(&peer) {
            self.windows.retain(|_, (m, _)| *m == minute);
            if self.windows.len() >= self.max_keys {
                return false;
            }
        }
        let (window, count) = self.windows.entry(peer).or_insert((minute, 0));
        if *window != minute {
            (*window, *count) = (minute, 0);
        }
        if *count >= self.per_minute {
            return false;
        }
        *count += 1;
        true
    }
}

struct Inner {
    peers: PeerTable,
    network_id: String,
    recipients: Vec<String>,
    replay: Mutex<ReplayCache>,
    budget: Mutex<Budget>,
}

/// The receiving side of the node credential; one per node, shared by the three routes so a
/// nonce is single-use across all of them.
#[derive(Clone)]
pub struct NodeAuth {
    inner: Arc<Inner>,
}

fn rate_per_minute_from_env() -> u32 {
    std::env::var("AVALON_NODE_AUTH_RATE_PER_MINUTE")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_RATE_PER_MINUTE)
}

/// `scheme://host[:port]` of an http(s) URL in its normalized form (lowercase host, default
/// port dropped, no path), the form a node signs and accepts as a recipient. `None` for any
/// other URL.
pub fn normalized_origin(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.origin().ascii_serialization())
}

/// The libp2p peer id a signing key belongs to.
fn peer_id_of_key(public_key: &[u8; 32]) -> Option<PeerId> {
    let key = ed25519::PublicKey::try_from_bytes(public_key).ok()?;
    Some(PeerId::from_public_key(&PublicKey::from(key)))
}

fn header_text(headers: &HeaderMap) -> Result<&str, NodeAuthError> {
    let mut values = headers.get_all(NODE_REQUEST_HEADER).iter();
    let Some(first) = values.next() else {
        return Err(NodeAuthError::Missing);
    };
    let malformed = |what| NodeAuthError::Invalid(NodeRequestError::Malformed(what));
    if values.next().is_some() {
        return Err(malformed("repeated header"));
    }
    first.to_str().map_err(|_| malformed("non-ascii"))
}

impl NodeAuth {
    /// Accepts requests addressed to `own_peer_id` or to `own_base_url` (when http(s)), on
    /// `network_id`, from peers `peers` holds standing for.
    pub fn new(
        peers: PeerTable,
        network_id: &str,
        own_peer_id: Option<&str>,
        own_base_url: Option<&str>,
    ) -> Self {
        Self::with_limits(
            peers,
            network_id,
            own_peer_id,
            own_base_url,
            rate_per_minute_from_env(),
            MAX_REPLAY_ENTRIES,
        )
    }

    /// [`Self::new`] with an explicit per-key budget and global nonce cap. One key can hold
    /// up to `4 * rate + 16` nonces, enough for its budget over the retention window.
    pub fn with_limits(
        peers: PeerTable,
        network_id: &str,
        own_peer_id: Option<&str>,
        own_base_url: Option<&str>,
        rate_per_minute: u32,
        max_replay_entries: usize,
    ) -> Self {
        let mut recipients: Vec<String> = own_peer_id.map(str::to_string).into_iter().collect();
        recipients.extend(own_base_url.and_then(normalized_origin));
        let per_signer = 4 * rate_per_minute as usize + 16;
        Self {
            inner: Arc::new(Inner {
                peers,
                network_id: network_id.to_string(),
                recipients,
                replay: Mutex::new(ReplayCache::new(max_replay_entries, per_signer)),
                budget: Mutex::new(Budget {
                    windows: HashMap::new(),
                    per_minute: rate_per_minute,
                    max_keys: MAX_BUDGET_KEYS,
                }),
            }),
        }
    }

    /// The middleware state for one route with `max_body_bytes` as its body bound.
    pub fn route(&self, max_body_bytes: usize) -> NodeAuthRoute {
        NodeAuthRoute {
            auth: self.clone(),
            max_body_bytes,
        }
    }

    /// Nonces currently held.
    pub fn replay_len(&self) -> usize {
        self.inner
            .replay
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .seen
            .len()
    }

    /// Keys currently holding a request budget.
    pub fn budget_keys(&self) -> usize {
        self.inner
            .budget
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .windows
            .len()
    }

    /// Cheap checks on an HTTP credential before the body is read: header form, clock window,
    /// key-to-peer-id binding and standing. No signature is verified and nothing is recorded.
    pub fn precheck(&self, headers: &HeaderMap, now: i64) -> Result<(), NodeAuthError> {
        let auth =
            parse_node_request_header(header_text(headers)?).map_err(NodeAuthError::Invalid)?;
        if auth.timestamp < now.saturating_sub(MAX_SKEW_SECS) {
            return Err(NodeAuthError::Invalid(NodeRequestError::Stale));
        }
        if auth.timestamp > now.saturating_add(MAX_SKEW_SECS) {
            return Err(NodeAuthError::Invalid(NodeRequestError::Future));
        }
        let derived = peer_id_of_key(&auth.public_key)
            .filter(|id| id.to_string() == auth.peer_id)
            .ok_or(NodeAuthError::PeerMismatch)?;
        if !self.inner.peers.standing(&derived) {
            return Err(NodeAuthError::NoStanding);
        }
        Ok(())
    }

    /// Resolves the caller. `remote` is the noise-authenticated peer of a stream request; when
    /// set, the header is not read at all.
    pub fn authenticate(
        &self,
        remote: Option<PeerId>,
        headers: &HeaderMap,
        method: &str,
        path: &str,
        body: &[u8],
        now: i64,
    ) -> Result<AuthenticatedNode, NodeAuthError> {
        let inner = &*self.inner;
        let (signer, nonce) = match remote {
            Some(peer) => (peer, None),
            None => {
                let header = header_text(headers)?;
                let target = NodeRequestTarget {
                    method,
                    path,
                    body,
                    network_id: &inner.network_id,
                };
                let recipients: Vec<&str> = inner.recipients.iter().map(String::as_str).collect();
                let auth =
                    verify_node_request_header(header, &target, &recipients, now, MAX_SKEW_SECS)
                        .map_err(NodeAuthError::Invalid)?;
                let derived = peer_id_of_key(&auth.public_key)
                    .filter(|id| id.to_string() == auth.peer_id)
                    .ok_or(NodeAuthError::PeerMismatch)?;
                (derived, Some(auth.nonce))
            }
        };
        if !inner.peers.standing(&signer) {
            return Err(NodeAuthError::NoStanding);
        }
        // Charged first so a refused request records no nonce.
        let mut budget = inner.budget.lock().unwrap_or_else(|p| p.into_inner());
        if !budget.charge(signer, now) {
            return Err(NodeAuthError::RateLimited);
        }
        drop(budget);
        if let Some(nonce) = nonce {
            let mut replay = inner.replay.lock().unwrap_or_else(|p| p.into_inner());
            replay.insert(signer, nonce, now)?;
        }
        Ok(AuthenticatedNode(signer))
    }
}

/// State of [`require_node_auth`] for one route.
#[derive(Clone)]
pub struct NodeAuthRoute {
    auth: NodeAuth,
    max_body_bytes: usize,
}

/// Middleware for the node-to-node write routes: refuses the request unless it resolves to an
/// [`AuthenticatedNode`], which it adds as a request extension. The body is read, bounded by
/// the route's limit, before it is hashed.
pub async fn require_node_auth(
    State(route): State<NodeAuthRoute>,
    request: Request,
    next: Next,
) -> Response {
    let (mut parts, body) = request.into_parts();
    let remote = parts.extensions.get::<RemotePeer>().map(|r| r.0);
    let refuse = |e: NodeAuthError| {
        let (reason, path) = (e.code(), parts.uri.path());
        let transport = if remote.is_some() { "stream" } else { "http" };
        // Refusals a stranger can cause at will stay at debug.
        if matches!(
            e,
            NodeAuthError::Replay | NodeAuthError::NoStanding | NodeAuthError::RateLimited
        ) {
            tracing::warn!(reason, path, transport, "node-auth: request refused");
        } else {
            tracing::debug!(reason, path, transport, "node-auth: request refused");
        }
        e.into_response()
    };
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    if remote.is_none() {
        if let Err(e) = route.auth.precheck(&parts.headers, now) {
            return refuse(e);
        }
    }
    let Ok(bytes) = axum::body::to_bytes(body, route.max_body_bytes).await else {
        return refuse(NodeAuthError::BodyTooLarge);
    };
    let node = match route.auth.authenticate(
        remote,
        &parts.headers,
        parts.method.as_str(),
        parts.uri.path(),
        &bytes,
        now,
    ) {
        Ok(node) => node,
        Err(e) => return refuse(e),
    };
    parts.extensions.insert(node);
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::PeerInfo;
    use axum::routing::post;
    use axum::Router;
    use libp2p::identity::Keypair;
    use std::sync::Arc;
    use tower::ServiceExt;

    const NET: &str = "net";
    const NOW: i64 = 1_790_000_000;
    const PATH: &str = "/nodes/relay";

    fn entry(url: &str, peer: &PeerId, bound: bool) -> PeerInfo {
        PeerInfo {
            base_url: url.into(),
            roles: vec![],
            protocol_version: "0.1.0".into(),
            network_id: NET.into(),
            last_announced_at: time::OffsetDateTime::now_utc(),
            libp2p_peer_id: Some(peer.to_string()),
            libp2p_listen_addrs: vec![],
            witness: None,
            connectivity: None,
            identity_bound: bound,
        }
    }

    struct Node {
        key: Keypair,
        id: PeerId,
    }

    fn node() -> Node {
        let key = Keypair::generate_ed25519();
        Node {
            id: PeerId::from(key.public()),
            key,
        }
    }

    struct Fixture {
        auth: NodeAuth,
        peers: PeerTable,
        me: PeerId,
    }

    fn fixture() -> Fixture {
        fixture_with(600, MAX_REPLAY_ENTRIES)
    }

    fn fixture_with(rate: u32, max_replay: usize) -> Fixture {
        let peers = PeerTable::new();
        let me = PeerId::random();
        let auth = NodeAuth::with_limits(
            peers.clone(),
            NET,
            Some(&me.to_string()),
            Some("https://Me.test:443/"),
            rate,
            max_replay,
        );
        Fixture { auth, peers, me }
    }

    impl Fixture {
        fn grant(&self, n: &Node) {
            self.peers
                .upsert(entry(&format!("http://{}.test", n.id), &n.id, true));
        }

        fn http(
            &self,
            header: &str,
            body: &[u8],
            now: i64,
        ) -> Result<AuthenticatedNode, NodeAuthError> {
            let mut headers = HeaderMap::new();
            headers.insert(NODE_REQUEST_HEADER, header.parse().unwrap());
            self.auth
                .authenticate(None, &headers, "POST", PATH, body, now)
        }
    }

    fn header_at(n: &Node, body: &[u8], recipient: &str, ts: i64, nonce: [u8; 16]) -> String {
        let target = NodeRequestTarget {
            method: "POST",
            path: PATH,
            body,
            network_id: NET,
        };
        let ed = n.key.clone().try_into_ed25519().unwrap();
        let seed: [u8; 32] = ed.secret().as_ref().try_into().unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&seed);
        let auth = avalon_protocol::node_request::sign_node_request(
            &key,
            &n.id.to_string(),
            &target,
            recipient,
            ts,
            nonce,
        )
        .unwrap();
        avalon_protocol::node_request::encode_node_request_header(&auth)
    }

    fn invalid(e: NodeRequestError) -> Result<AuthenticatedNode, NodeAuthError> {
        Err(NodeAuthError::Invalid(e))
    }

    #[test]
    fn origins_are_normalized_and_non_http_urls_have_none() {
        assert_eq!(
            normalized_origin("HTTP://Node.Test:80/nodes/relay?x=1").as_deref(),
            Some("http://node.test")
        );
        assert_eq!(
            normalized_origin("https://node.test:8443/").as_deref(),
            Some("https://node.test:8443")
        );
        assert_eq!(normalized_origin("p2p://12D3KooWabc"), None);
        assert_eq!(normalized_origin("not a url"), None);
    }

    #[test]
    fn a_signed_request_from_a_node_with_standing_is_accepted_by_either_recipient() {
        let f = fixture();
        let n = node();
        f.grant(&n);
        let me = f.me.to_string();
        for (i, recipient) in [me.as_str(), "https://me.test"].into_iter().enumerate() {
            let h = header_at(&n, b"{}", recipient, NOW, [i as u8; 16]);
            assert_eq!(f.http(&h, b"{}", NOW), Ok(AuthenticatedNode(n.id)));
        }
        // A node that has standing only through its p2p:// entry qualifies too.
        let p = node();
        f.peers
            .upsert(entry(&crate::node_http::p2p_base_url(&p.id), &p.id, true));
        let h = header_at(&p, b"{}", &me, NOW, [9; 16]);
        assert_eq!(f.http(&h, b"{}", NOW), Ok(AuthenticatedNode(p.id)));
    }

    #[test]
    fn a_header_is_mandatory_over_http_and_must_be_single_and_ascii() {
        let f = fixture();
        let none = HeaderMap::new();
        let r = f.auth.authenticate(None, &none, "POST", PATH, b"", NOW);
        assert_eq!(r, Err(NodeAuthError::Missing));
        let mut two = HeaderMap::new();
        let n = node();
        f.grant(&n);
        let valid = header_at(&n, b"", &f.me.to_string(), NOW, [1; 16]);
        assert!(f.http(&valid, b"", NOW).is_ok());
        let again = header_at(&n, b"", &f.me.to_string(), NOW, [2; 16]);
        two.append(NODE_REQUEST_HEADER, again.parse().unwrap());
        two.append(NODE_REQUEST_HEADER, again.parse().unwrap());
        let r = f.auth.authenticate(None, &two, "POST", PATH, b"", NOW);
        assert!(matches!(
            r,
            Err(NodeAuthError::Invalid(NodeRequestError::Malformed(_)))
        ));
        let mut bin = HeaderMap::new();
        bin.insert(
            NODE_REQUEST_HEADER,
            axum::http::HeaderValue::from_bytes(b"v1\xff").unwrap(),
        );
        let r = f.auth.authenticate(None, &bin, "POST", PATH, b"", NOW);
        assert!(matches!(
            r,
            Err(NodeAuthError::Invalid(NodeRequestError::Malformed(_)))
        ));
        assert!(matches!(
            f.http("garbage", b"", NOW),
            Err(NodeAuthError::Invalid(NodeRequestError::Malformed(_)))
        ));
    }

    #[test]
    fn stale_future_wrong_recipient_network_body_and_signature_are_all_refused() {
        let f = fixture();
        let n = node();
        f.grant(&n);
        let me = f.me.to_string();
        let ok = |ts, nonce| header_at(&n, b"{}", &me, ts, [nonce; 16]);
        assert_eq!(
            f.http(&ok(NOW - MAX_SKEW_SECS - 1, 1), b"{}", NOW),
            invalid(NodeRequestError::Stale)
        );
        assert_eq!(
            f.http(&ok(NOW + MAX_SKEW_SECS + 1, 2), b"{}", NOW),
            invalid(NodeRequestError::Future)
        );
        // The skew edges are inside the window.
        assert!(f.http(&ok(NOW - MAX_SKEW_SECS, 3), b"{}", NOW).is_ok());
        assert!(f.http(&ok(NOW + MAX_SKEW_SECS, 4), b"{}", NOW).is_ok());

        let bad = invalid(NodeRequestError::BadSignature);
        let other = header_at(&n, b"{}", "https://elsewhere.test", NOW, [5; 16]);
        assert_eq!(f.http(&other, b"{}", NOW), bad);
        assert_eq!(f.http(&ok(NOW, 6), b"{ }", NOW), bad);
        // A header signed for another network.
        let other_net = Fixture {
            auth: NodeAuth::with_limits(f.peers.clone(), "other-net", Some(&me), None, 600, 1000),
            ..fixture()
        };
        assert_eq!(other_net.http(&ok(NOW, 7), b"{}", NOW), bad);
        let mut h = ok(NOW, 8);
        let at = h.rfind("sig=").unwrap() + 4;
        h.replace_range(at..at + 1, if &h[at..at + 1] == "0" { "1" } else { "0" });
        assert_eq!(f.http(&h, b"{}", NOW), bad);
        assert_eq!(
            f.auth.replay_len(),
            2,
            "only the two accepted requests were recorded"
        );
    }

    #[test]
    fn a_nonce_is_single_use_across_recipients_and_expires_after_twice_the_skew() {
        let f = fixture();
        let n = node();
        f.grant(&n);
        let me = f.me.to_string();
        let a = header_at(&n, b"{}", &me, NOW, [1; 16]);
        assert!(f.http(&a, b"{}", NOW).is_ok());
        assert_eq!(f.http(&a, b"{}", NOW), Err(NodeAuthError::Replay));
        // The same nonce for the other recipient is the same signer's nonce: still a replay.
        let b = header_at(&n, b"{}", "https://me.test", NOW, [1; 16]);
        assert_eq!(f.http(&b, b"{}", NOW + 1), Err(NodeAuthError::Replay));
        // Another signer may use the same nonce bytes.
        let m = node();
        f.grant(&m);
        let c = header_at(&m, b"{}", &me, NOW, [1; 16]);
        assert!(f.http(&c, b"{}", NOW).is_ok());
        // A timestamp at the future edge is still valid until now + 2 * skew: the nonce is held.
        let edge = header_at(&n, b"{}", &me, NOW + MAX_SKEW_SECS, [2; 16]);
        assert!(f.http(&edge, b"{}", NOW).is_ok());
        assert_eq!(
            f.http(&edge, b"{}", NOW + 2 * MAX_SKEW_SECS),
            Err(NodeAuthError::Replay)
        );
        // Once past it the entry is gone (and the timestamp is stale anyway).
        assert_eq!(
            f.http(&edge, b"{}", NOW + 2 * MAX_SKEW_SECS + 2),
            invalid(NodeRequestError::Stale)
        );
        let later = header_at(&n, b"{}", &me, NOW + 300, [1; 16]);
        assert!(
            f.http(&later, b"{}", NOW + 300).is_ok(),
            "the old nonce expired"
        );
        assert_eq!(f.auth.replay_len(), 1, "everything older was pruned");
    }

    #[test]
    fn the_key_must_hash_to_the_claimed_peer_id() {
        let f = fixture();
        let (victim, attacker) = (node(), node());
        f.grant(&victim);
        // The attacker signs with its own key but claims the victim's id.
        let target = NodeRequestTarget {
            method: "POST",
            path: PATH,
            body: b"{}",
            network_id: NET,
        };
        let ed = attacker.key.clone().try_into_ed25519().unwrap();
        let seed: [u8; 32] = ed.secret().as_ref().try_into().unwrap();
        let auth = avalon_protocol::node_request::sign_node_request(
            &ed25519_dalek::SigningKey::from_bytes(&seed),
            &victim.id.to_string(),
            &target,
            &f.me.to_string(),
            NOW,
            [3; 16],
        )
        .unwrap();
        let h = avalon_protocol::node_request::encode_node_request_header(&auth);
        assert_eq!(f.http(&h, b"{}", NOW), Err(NodeAuthError::PeerMismatch));
        assert_eq!(f.auth.replay_len(), 0);
        assert_eq!(f.auth.budget_keys(), 0);
    }

    #[test]
    fn a_signer_without_standing_is_refused_and_leaves_no_state() {
        let f = fixture();
        let n = node();
        let me = f.me.to_string();
        for i in 0..20u8 {
            let h = header_at(&n, b"{}", &me, NOW, [i; 16]);
            assert_eq!(f.http(&h, b"{}", NOW), Err(NodeAuthError::NoStanding));
        }
        // An unbound entry, and one only in the unverified pool, are no standing either.
        f.peers.upsert(entry("http://u.test", &n.id, false));
        let g = node();
        f.peers
            .insert_unverified(entry("http://g.test", &g.id, true));
        let h = header_at(&g, b"{}", &me, NOW, [1; 16]);
        assert_eq!(f.http(&h, b"{}", NOW), Err(NodeAuthError::NoStanding));
        let h = header_at(&n, b"{}", &me, NOW, [99; 16]);
        assert_eq!(f.http(&h, b"{}", NOW), Err(NodeAuthError::NoStanding));
        assert_eq!((f.auth.replay_len(), f.auth.budget_keys()), (0, 0));
    }

    #[test]
    fn unauthenticated_traffic_cannot_fill_the_nonce_cache() {
        let f = fixture();
        let known = node();
        f.grant(&known);
        let me = f.me.to_string();
        let strangers: Vec<Node> = (0..30).map(|_| node()).collect();
        for (i, s) in strangers.iter().enumerate() {
            let h = header_at(s, b"{}", &me, NOW, [i as u8; 16]);
            assert!(f.http(&h, b"{}", NOW).is_err());
            // Bad signatures, stale and wrong-recipient requests from a known key too.
            let h = header_at(&known, b"x", &me, NOW, [100 + i as u8; 16]);
            assert!(f.http(&h, b"{}", NOW).is_err());
            let h = header_at(&known, b"{}", &me, NOW - 1000, [i as u8; 16]);
            assert!(f.http(&h, b"{}", NOW).is_err());
            let h = header_at(&known, b"{}", "https://x.test", NOW, [i as u8; 16]);
            assert!(f.http(&h, b"{}", NOW).is_err());
        }
        assert_eq!((f.auth.replay_len(), f.auth.budget_keys()), (0, 0));
    }

    #[test]
    fn the_nonce_cache_is_bounded_globally_and_per_signer() {
        // Global cap: the oldest nonce is forgotten to make room, the total never exceeds it.
        let f = fixture_with(600, 5);
        let me = f.me.to_string();
        let nodes: Vec<Node> = (0..4).map(|_| node()).collect();
        for n in &nodes {
            f.grant(n);
        }
        for i in 0..40u8 {
            let n = &nodes[i as usize % 4];
            let h = header_at(n, b"{}", &me, NOW, [i; 16]);
            assert!(f.http(&h, b"{}", NOW).is_ok());
            assert!(f.auth.replay_len() <= 5);
        }
        assert_eq!(f.auth.replay_len(), 5);

        // Per-signer cap (4 * rate + 16): one key cannot take more than its share.
        let g = fixture_with(1, MAX_REPLAY_ENTRIES);
        let n = node();
        g.grant(&n);
        let me = g.me.to_string();
        let mut accepted = 0;
        let mut refused = None;
        for i in 0..40u8 {
            let h = header_at(&n, b"{}", &me, NOW, [i; 16]);
            match g.http(&h, b"{}", NOW) {
                Ok(_) => accepted += 1,
                Err(e) => {
                    refused.get_or_insert(e);
                }
            }
        }
        assert!(accepted <= 20);
        assert_eq!(refused, Some(NodeAuthError::RateLimited));
        assert!(g.auth.replay_len() <= 20);
    }

    #[test]
    fn each_standing_key_has_its_own_request_budget_per_minute() {
        let f = fixture_with(3, MAX_REPLAY_ENTRIES);
        let (a, b) = (node(), node());
        f.grant(&a);
        f.grant(&b);
        let me = f.me.to_string();
        let send =
            |n: &Node, i: u8, now: i64| f.http(&header_at(n, b"{}", &me, now, [i; 16]), b"{}", now);
        for i in 0..3 {
            assert!(send(&a, i, NOW).is_ok());
        }
        assert_eq!(send(&a, 3, NOW), Err(NodeAuthError::RateLimited));
        assert!(send(&b, 10, NOW).is_ok(), "another key is unaffected");
        assert!(
            send(&a, 4, NOW + 60).is_ok(),
            "the next minute starts afresh"
        );
        assert_eq!(f.auth.budget_keys(), 2);
    }

    #[test]
    fn budget_keys_are_capped() {
        let mut b = Budget {
            windows: HashMap::new(),
            per_minute: 5,
            max_keys: 3,
        };
        let ids: Vec<PeerId> = (0..4).map(|_| PeerId::random()).collect();
        for id in &ids[..3] {
            assert!(b.charge(*id, NOW));
        }
        assert!(
            !b.charge(ids[3], NOW),
            "a new key is refused while the table is full"
        );
        assert!(b.charge(ids[0], NOW), "a known key keeps its budget");
        assert!(b.charge(ids[3], NOW + 60), "stale windows are reclaimed");
        assert!(b.windows.len() <= 3);
    }

    #[test]
    fn a_stream_peer_is_authenticated_by_its_handshake_and_the_header_is_ignored() {
        let f = fixture();
        let (caller, other) = (node(), node());
        f.grant(&caller);
        f.grant(&other);
        let none = HeaderMap::new();
        let r = f
            .auth
            .authenticate(Some(caller.id), &none, "POST", PATH, b"x", NOW);
        assert_eq!(r, Ok(AuthenticatedNode(caller.id)));

        // A valid header from another standing node does not change who the caller is.
        let mut headers = HeaderMap::new();
        let h = header_at(&other, b"x", &f.me.to_string(), NOW, [1; 16]);
        headers.insert(NODE_REQUEST_HEADER, h.parse().unwrap());
        let r = f
            .auth
            .authenticate(Some(caller.id), &headers, "POST", PATH, b"x", NOW);
        assert_eq!(r, Ok(AuthenticatedNode(caller.id)));
        // Garbage, and a repeated header, are ignored as well.
        headers.append(NODE_REQUEST_HEADER, "garbage".parse().unwrap());
        let r = f
            .auth
            .authenticate(Some(caller.id), &headers, "POST", PATH, b"x", NOW);
        assert_eq!(r, Ok(AuthenticatedNode(caller.id)));
        // The header is not consumed as a nonce either.
        assert_eq!(f.auth.replay_len(), 0);

        // A valid header from a standing node cannot lend its standing to a stream peer.
        let stranger = node();
        let r = f
            .auth
            .authenticate(Some(stranger.id), &headers, "POST", PATH, b"x", NOW);
        assert_eq!(r, Err(NodeAuthError::NoStanding));
    }

    #[test]
    fn a_stream_peer_shares_the_per_key_budget_with_its_http_requests() {
        let f = fixture_with(2, MAX_REPLAY_ENTRIES);
        let n = node();
        f.grant(&n);
        let none = HeaderMap::new();
        let go = || {
            f.auth
                .authenticate(Some(n.id), &none, "POST", PATH, b"", NOW)
        };
        assert!(go().is_ok());
        assert!(go().is_ok());
        assert_eq!(go(), Err(NodeAuthError::RateLimited));
        let h = header_at(&n, b"{}", &f.me.to_string(), NOW, [1; 16]);
        assert_eq!(f.http(&h, b"{}", NOW), Err(NodeAuthError::RateLimited));
    }

    #[test]
    fn error_codes_and_statuses_are_stable() {
        use NodeAuthError::*;
        let all = [
            (Missing, 401, "node_auth_missing"),
            (
                Invalid(NodeRequestError::Malformed("x")),
                401,
                "node_auth_malformed",
            ),
            (
                Invalid(NodeRequestError::InvalidRequest("x")),
                401,
                "node_auth_invalid_request",
            ),
            (
                Invalid(NodeRequestError::InvalidKey),
                401,
                "node_auth_invalid_key",
            ),
            (Invalid(NodeRequestError::Stale), 401, "node_auth_stale"),
            (Invalid(NodeRequestError::Future), 401, "node_auth_future"),
            (
                Invalid(NodeRequestError::BadSignature),
                401,
                "node_auth_bad_signature",
            ),
            (PeerMismatch, 401, "node_auth_peer_mismatch"),
            (Replay, 401, "node_auth_replay"),
            (NoStanding, 403, "node_auth_no_standing"),
            (RateLimited, 429, "node_auth_rate_limited"),
            (BodyTooLarge, 413, "node_auth_body_too_large"),
        ];
        for (e, status, code) in all {
            assert_eq!((e.status().as_u16(), e.code()), (status, code));
        }
    }

    fn app(f: &Fixture, limit: usize) -> Router {
        Router::new().route(
            PATH,
            post(
                |node: AuthenticatedNode, body: axum::body::Bytes| async move {
                    format!("{}:{}", node.0, body.len())
                },
            )
            .layer(axum::middleware::from_fn_with_state(
                f.auth.route(limit),
                require_node_auth,
            )),
        )
    }

    async fn call(app: &Router, req: Request) -> (StatusCode, String) {
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let body = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    fn post_req(header: Option<&str>, body: &[u8], remote: Option<PeerId>) -> Request {
        let mut b = Request::builder().method("POST").uri(PATH);
        if let Some(h) = header {
            b = b.header(NODE_REQUEST_HEADER, h);
        }
        let mut req = b.body(Body::from(body.to_vec())).unwrap();
        if let Some(p) = remote {
            req.extensions_mut().insert(RemotePeer(p));
        }
        req
    }

    #[tokio::test]
    async fn the_middleware_passes_the_authenticated_node_and_the_whole_body_on() {
        let f = fixture();
        let n = node();
        f.grant(&n);
        let app = app(&f, 64);
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let h = header_at(&n, b"hello", &f.me.to_string(), now, [1; 16]);
        let (status, body) = call(&app, post_req(Some(&h), b"hello", None)).await;
        assert_eq!((status, body), (StatusCode::OK, format!("{}:5", n.id)));
        // The same request again is a replay: 401 with its own code.
        let (status, body) = call(&app, post_req(Some(&h), b"hello", None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.contains("node_auth_replay"), "{body}");
        // Over a stream, no header is needed.
        let (status, body) = call(&app, post_req(None, b"hello", Some(n.id))).await;
        assert_eq!((status, body), (StatusCode::OK, format!("{}:5", n.id)));
        // Without standing, 403; without a header over HTTP, 401.
        let (status, _) = call(&app, post_req(None, b"hello", Some(PeerId::random()))).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = call(&app, post_req(None, b"hello", None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn an_oversized_body_is_refused_before_it_is_hashed_and_a_missing_header_before_it_is_read(
    ) {
        let f = fixture();
        let n = node();
        f.grant(&n);
        let app = app(&f, 8);
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let big = [b'x'; 9];
        let h = header_at(&n, &big, &f.me.to_string(), now, [1; 16]);
        let (status, body) = call(&app, post_req(Some(&h), &big, None)).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert!(body.contains("node_auth_body_too_large"));
        // Over a stream the bound applies too.
        let (status, _) = call(&app, post_req(None, &big, Some(n.id))).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        // Within the bound the same request goes through.
        let ok = [b'x'; 8];
        let h = header_at(&n, &ok, &f.me.to_string(), now, [2; 16]);
        assert_eq!(
            call(&app, post_req(Some(&h), &ok, None)).await.0,
            StatusCode::OK
        );
        // No credential: 401, not 413, since the body is never read.
        let (status, _) = call(&app, post_req(None, &big, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(f.auth.replay_len(), 1);
    }

    #[tokio::test]
    async fn a_request_header_cannot_stand_in_for_the_stream_marker() {
        // The stream marker is a request extension; nothing on the wire can set one.
        let f = fixture();
        let n = node();
        f.grant(&n);
        let app = app(&f, 64);
        let mut req = Request::builder()
            .method("POST")
            .uri(PATH)
            .header("x-avalon-peer", n.id.to_string())
            .header("x-remote-peer", n.id.to_string())
            .body(Body::empty())
            .unwrap();
        assert!(req.extensions().get::<RemotePeer>().is_none());
        req.headers_mut()
            .insert("forwarded", "for=127.0.0.1".parse().unwrap());
        assert_eq!(call(&app, req).await.0, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn a_request_over_budget_records_no_nonce() {
        let f = fixture_with(1, MAX_REPLAY_ENTRIES);
        let n = node();
        f.grant(&n);
        let me = f.me.to_string();
        let send = |i: u8| f.http(&header_at(&n, b"{}", &me, NOW, [i; 16]), b"{}", NOW);
        assert!(send(1).is_ok());
        for i in 2..10 {
            assert_eq!(send(i), Err(NodeAuthError::RateLimited));
        }
        assert_eq!(f.auth.replay_len(), 1);
    }

    #[test]
    fn the_precheck_refuses_garbage_before_any_body_or_signature_work() {
        let f = fixture();
        let n = node();
        f.grant(&n);
        let me = f.me.to_string();
        let check = |h: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(NODE_REQUEST_HEADER, h.parse().unwrap());
            f.auth.precheck(&headers, NOW)
        };
        let good = header_at(&n, b"{}", &me, NOW, [1; 16]);
        assert_eq!(check(&good), Ok(()));
        assert!(matches!(
            check("garbage"),
            Err(NodeAuthError::Invalid(NodeRequestError::Malformed(_)))
        ));
        let long = format!("{good}{}", "a".repeat(600));
        assert!(matches!(
            check(&long),
            Err(NodeAuthError::Invalid(NodeRequestError::Malformed(_)))
        ));
        let stale = header_at(&n, b"{}", &me, NOW - 1000, [2; 16]);
        assert_eq!(
            check(&stale),
            Err(NodeAuthError::Invalid(NodeRequestError::Stale))
        );
        let future = header_at(&n, b"{}", &me, NOW + 1000, [3; 16]);
        assert_eq!(
            check(&future),
            Err(NodeAuthError::Invalid(NodeRequestError::Future))
        );
        let stranger = node();
        let h = header_at(&stranger, b"{}", &me, NOW, [4; 16]);
        assert_eq!(check(&h), Err(NodeAuthError::NoStanding));
        // A claimed id that does not belong to the key.
        let h = good.replacen(&n.id.to_string(), &stranger.id.to_string(), 1);
        assert_eq!(check(&h), Err(NodeAuthError::PeerMismatch));
        assert_eq!(f.auth.replay_len(), 0);
    }

    /// A body that records whether anything polled it.
    fn watched_body(polled: Arc<std::sync::atomic::AtomicBool>) -> Body {
        Body::from_stream(futures_util::stream::poll_fn(move |_| {
            polled.store(true, std::sync::atomic::Ordering::SeqCst);
            std::task::Poll::Ready(None::<Result<axum::body::Bytes, std::io::Error>>)
        }))
    }

    #[tokio::test]
    async fn a_request_that_fails_the_precheck_never_has_its_body_read() {
        let f = fixture();
        let n = node();
        f.grant(&n);
        let app = app(&f, 64);
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let stale = header_at(&n, b"", &f.me.to_string(), now - 1000, [1; 16]);
        let stranger = header_at(&node(), b"", &f.me.to_string(), now, [2; 16]);
        for h in [stale.as_str(), stranger.as_str(), "garbage"] {
            let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let req = Request::builder()
                .method("POST")
                .uri(PATH)
                .header(NODE_REQUEST_HEADER, h)
                .body(watched_body(polled.clone()))
                .unwrap();
            let status = call(&app, req).await.0;
            assert!(status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN);
            assert!(!polled.load(std::sync::atomic::Ordering::SeqCst), "{h}");
        }
        // A credential that passes the precheck does get its body read.
        let ok = header_at(&n, b"", &f.me.to_string(), now, [3; 16]);
        let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let req = Request::builder()
            .method("POST")
            .uri(PATH)
            .header(NODE_REQUEST_HEADER, ok)
            .body(watched_body(polled.clone()))
            .unwrap();
        assert_eq!(call(&app, req).await.0, StatusCode::OK);
        assert!(polled.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn the_default_budget_matches_the_per_ip_limit_these_routes_were_under() {
        assert_eq!(DEFAULT_RATE_PER_MINUTE, 3000);
        assert_eq!(
            DEFAULT_RATE_PER_MINUTE as u64,
            crate::DEFAULT_RATE_LIMIT_PER_MINUTE
        );
    }
}
