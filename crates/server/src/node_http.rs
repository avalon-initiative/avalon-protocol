//! Node-to-node HTTP that also works for peers with no reachable URL.
//!
//! [`NodeClient`] is a drop-in for the slice of `reqwest` the node-to-node call sites use.
//! `http(s)://` URLs go through the wrapped `reqwest::Client` untouched (including the pinned
//! clients `crate::outbound_policy` builds); `p2p://<libp2p peer id>` URLs are carried as one
//! request-response exchange on a libp2p stream, so relays and hole punching apply to them.
//! A connect error or timeout on one transport falls back to the other when the peer has both,
//! and recent outcomes demote a failing one; see [`crate::transport_stats`].
//! The receiving side dispatches into the normal router; see [`inbound`].

mod inbound;
mod wire;

use std::sync::OnceLock;
use std::time::Duration;

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use libp2p::PeerId;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::oneshot;

use crate::dht::{DhtCommand, DhtCommandSender};
use crate::nodes::{PeerInfo, PeerTable};
use crate::transport_stats::{Transport, TransportStats};
use avalon_protocol::connectivity::Connectivity;

pub use inbound::{
    path_allowed, requires_bound_peer, synthetic_addr, InboundPermit, InboundService, RemotePeer,
    RouterSlot, ALLOWED_EXACT, BOUND_ONLY_PATHS, SHARED_PEER_ADDR,
};
pub use wire::{
    protocol_name, BufferGrant, NodeHttpCodec, NodeHttpRequest, NodeHttpResponse, NodeHttpSettings,
    MAX_CONCURRENT_STREAMS, MAX_HEADER_BYTES, MAX_HEADER_COUNT,
};

/// URL scheme for a peer reached over a libp2p stream.
pub const P2P_SCHEME: &str = "p2p";

/// `p2p://<peer id>` for `peer`.
pub fn p2p_base_url(peer: &PeerId) -> String {
    format!("{P2P_SCHEME}://{peer}")
}

/// The peer id of a `p2p://<peer id>` base URL (no path, query or fragment), else `None`.
pub fn parse_p2p_base(url: &str) -> Option<PeerId> {
    let rest = url.trim().strip_prefix("p2p://")?;
    rest.trim_end_matches('/').parse().ok()
}

/// Splits `p2p://<peer id>/path?query` into the peer and its path-and-query.
fn split_p2p_url(url: &str) -> Option<(PeerId, String)> {
    let rest = url.strip_prefix("p2p://")?;
    let (host, tail) = match rest.find(['/', '?']) {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let path = if tail.starts_with('/') {
        tail.to_string()
    } else {
        format!("/{tail}")
    };
    Some((host.parse().ok()?, path))
}

/// Why a node-to-node request failed. Mirrors what call sites ask of a `reqwest::Error`.
#[derive(Debug, thiserror::Error)]
pub enum NodeHttpError {
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("stream request failed ({kind:?}): {message}")]
    Stream {
        kind: StreamErrorKind,
        message: String,
    },
    #[error("HTTP status {0}")]
    Status(StatusCode),
    #[error("decode: {0}")]
    Decode(String),
    #[error("invalid request: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamErrorKind {
    /// No stream transport is running on this node.
    Unavailable,
    /// The peer could not be dialed or the connection dropped.
    Connect,
    Timeout,
    /// The frame broke a size or format limit.
    Protocol,
}

impl NodeHttpError {
    fn stream(kind: StreamErrorKind, message: impl Into<String>) -> Self {
        Self::Stream {
            kind,
            message: message.into(),
        }
    }

    pub fn is_timeout(&self) -> bool {
        match self {
            Self::Http(e) => e.is_timeout(),
            Self::Stream { kind, .. } => *kind == StreamErrorKind::Timeout,
            _ => false,
        }
    }

    pub fn is_connect(&self) -> bool {
        match self {
            Self::Http(e) => e.is_connect(),
            Self::Stream { kind, .. } => *kind == StreamErrorKind::Connect,
            _ => false,
        }
    }

    pub fn status(&self) -> Option<StatusCode> {
        match self {
            Self::Http(e) => e.status(),
            Self::Status(s) => Some(*s),
            _ => None,
        }
    }

    /// Strips the URL from the underlying HTTP error so it is safe to log.
    pub fn without_url(self) -> Self {
        match self {
            Self::Http(e) => Self::Http(e.without_url()),
            other => other,
        }
    }
}

/// How this process reaches its libp2p swarm; cloned into every [`NodeClient`] that uses it.
#[derive(Clone)]
pub struct StreamHandle {
    pub commands: DhtCommandSender,
    /// Used to find a peer's libp2p id for the HTTP-to-stream fallback.
    pub peers: Option<PeerTable>,
    pub settings: NodeHttpSettings,
}

impl StreamHandle {
    async fn request(
        &self,
        peer: PeerId,
        request: NodeHttpRequest,
        timeout: Duration,
    ) -> Result<NodeHttpResponse, NodeHttpError> {
        let (respond_to, rx) = oneshot::channel();
        let command = DhtCommand::HttpRequest {
            peer,
            request,
            respond_to,
        };
        let exchange = async {
            self.commands.send(command).await.map_err(|_| {
                NodeHttpError::stream(StreamErrorKind::Unavailable, "libp2p worker stopped")
            })?;
            rx.await.map_err(|_| {
                NodeHttpError::stream(StreamErrorKind::Unavailable, "libp2p worker stopped")
            })?
        };
        tokio::time::timeout(timeout, exchange)
            .await
            .unwrap_or_else(|_| {
                Err(NodeHttpError::stream(
                    StreamErrorKind::Timeout,
                    "timed out connecting to or waiting on the peer",
                ))
            })
    }
}

/// The process-wide stream handle, so call-site signatures stay `(client, url)`. Set once at
/// startup after the swarm is running; a client with its own handle ignores it.
static GLOBAL_STREAM: OnceLock<StreamHandle> = OnceLock::new();

/// Registers the stream transport for every [`NodeClient`] without its own handle.
pub fn install_stream_handle(handle: StreamHandle) {
    let _ = GLOBAL_STREAM.set(handle);
}

/// Cheap to clone; see the module docs.
#[derive(Clone)]
pub struct NodeClient {
    http: reqwest::Client,
    stream: Option<StreamHandle>,
    /// Timeout for stream requests, matching the wrapped client's own where it has one.
    timeout: Option<Duration>,
}

impl Default for NodeClient {
    fn default() -> Self {
        Self::new()
    }
}

impl From<reqwest::Client> for NodeClient {
    fn from(http: reqwest::Client) -> Self {
        Self {
            http,
            stream: None,
            timeout: None,
        }
    }
}

impl NodeClient {
    /// A plain client with no timeout of its own (stream requests use the configured default).
    pub fn new() -> Self {
        reqwest::Client::new().into()
    }

    /// The bounded client for requests to other nodes; see [`crate::outbound_policy::peer_client`].
    pub fn peer() -> Self {
        Self::from(crate::outbound_policy::peer_client())
            .with_timeout(crate::outbound_policy::PEER_REQUEST_TIMEOUT)
    }

    /// Wraps `http`, giving stream requests the same `timeout` it enforces.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Uses `handle` instead of the process-wide one.
    pub fn with_stream(mut self, handle: StreamHandle) -> Self {
        self.stream = Some(handle);
        self
    }

    fn stream_handle(&self) -> Option<&StreamHandle> {
        self.stream.as_ref().or_else(|| GLOBAL_STREAM.get())
    }

    pub fn get(&self, url: impl AsRef<str>) -> NodeRequestBuilder {
        self.request(Method::GET, url)
    }

    pub fn post(&self, url: impl AsRef<str>) -> NodeRequestBuilder {
        self.request(Method::POST, url)
    }

    pub fn request(&self, method: Method, url: impl AsRef<str>) -> NodeRequestBuilder {
        NodeRequestBuilder {
            client: self.clone(),
            method,
            url: url.as_ref().to_string(),
            headers: Vec::new(),
            query: Vec::new(),
            body: None,
            timeout: None,
            error: None,
            pinned: false,
        }
    }

    /// The base URL to use for `peer`: `p2p://<id>` when it has a libp2p id and either only
    /// dials out or is reached through a relay or hole punch, or its `base_url` is not a usable http(s) URL;
    /// otherwise its `base_url`.
    pub fn url_for(peer: &PeerInfo) -> String {
        Self::url_for_with(peer, None)
    }

    /// [`Self::url_for`], with the hinted transport swapped for the other one while recent
    /// outcomes in `stats` have it failing and the other is usable and not failing.
    pub fn url_for_with(peer: &PeerInfo, stats: Option<&TransportStats>) -> String {
        if !peer.identity_bound {
            return peer.base_url.clone();
        }
        let Some(id) = peer
            .libp2p_peer_id
            .as_ref()
            .and_then(|s| s.parse::<PeerId>().ok())
        else {
            return peer.base_url.clone();
        };
        let url_usable =
            crate::outbound_policy::OutboundPolicy::parse_base_url(&peer.base_url).is_ok();
        let prefer_stream = matches!(
            peer.connectivity,
            Some(Connectivity::NatTraversed | Connectivity::Relayed | Connectivity::OutboundOnly)
        );
        let hinted = if prefer_stream || !url_usable {
            Transport::Stream
        } else {
            Transport::Http
        };
        let chosen = stats.map_or(hinted, |s| s.choose(&id, hinted, url_usable));
        match chosen {
            Transport::Stream => p2p_base_url(&id),
            Transport::Http => peer.base_url.clone(),
        }
    }

    /// The libp2p peer serving the http(s) `url`, when the peer table knows one.
    fn fallback_peer(&self, url: &str) -> Option<PeerId> {
        self.stream_handle()?
            .peers
            .as_ref()?
            .libp2p_peer_for_url(url)
    }
}

/// Mirrors the `reqwest::RequestBuilder` calls node-to-node sites make.
#[derive(Clone)]
pub struct NodeRequestBuilder {
    client: NodeClient,
    method: Method,
    url: String,
    headers: Vec<(String, String)>,
    query: Vec<(String, String)>,
    body: Option<Bytes>,
    timeout: Option<Duration>,
    error: Option<String>,
    pinned: bool,
}

impl NodeRequestBuilder {
    pub fn header(mut self, name: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        self.headers
            .push((name.as_ref().to_string(), value.as_ref().to_string()));
        self
    }

    pub fn bearer_auth(self, token: impl std::fmt::Display) -> Self {
        self.header("authorization", format!("Bearer {token}"))
    }

    pub fn query<K: AsRef<str>, V: AsRef<str>>(mut self, pairs: &[(K, V)]) -> Self {
        self.query.extend(
            pairs
                .iter()
                .map(|(k, v)| (k.as_ref().to_string(), v.as_ref().to_string())),
        );
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Keeps the request on the transport its URL names, for a caller whose request means
    /// something only over that transport.
    pub fn pinned(mut self) -> Self {
        self.pinned = true;
        self
    }

    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub fn json<T: Serialize + ?Sized>(mut self, value: &T) -> Self {
        match serde_json::to_vec(value) {
            Ok(bytes) => {
                if !self
                    .headers
                    .iter()
                    .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                {
                    self.headers
                        .push(("content-type".into(), "application/json".into()));
                }
                self.body = Some(bytes.into());
            }
            Err(e) => self.error = Some(e.to_string()),
        }
        self
    }

    pub fn try_clone(&self) -> Option<Self> {
        Some(self.clone())
    }

    fn http_builder_to(&self, url: &str) -> reqwest::RequestBuilder {
        let mut rb = self.client.http.request(self.method.clone(), url);
        if !self.query.is_empty() {
            rb = rb.query(&self.query);
        }
        for (k, v) in &self.headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        if let Some(body) = &self.body {
            rb = rb.body(body.clone());
        }
        if let Some(t) = self.timeout {
            rb = rb.timeout(t);
        }
        rb
    }

    /// The stream form of this request for `path` (already carrying any query in the URL).
    fn stream_request(&self, path: String) -> NodeHttpRequest {
        let mut path_and_query = path;
        if !self.query.is_empty() {
            let encoded = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(&self.query)
                .finish();
            path_and_query.push(if path_and_query.contains('?') {
                '&'
            } else {
                '?'
            });
            path_and_query.push_str(&encoded);
        }
        NodeHttpRequest {
            method: self.method.as_str().to_string(),
            path_and_query,
            headers: self.headers.clone(),
            body: self.body.as_ref().map(|b| b.to_vec()).unwrap_or_default(),
            grant: Default::default(),
        }
    }

    async fn send_stream(&self, peer: PeerId, path: String) -> Result<NodeResponse, NodeHttpError> {
        let handle = self.client.stream_handle().ok_or_else(|| {
            NodeHttpError::stream(StreamErrorKind::Unavailable, "no libp2p transport running")
        })?;
        let timeout = self
            .timeout
            .or(self.client.timeout)
            .unwrap_or(handle.settings.timeout);
        let response = handle
            .request(peer, self.stream_request(path), timeout)
            .await?;
        NodeResponse::from_stream(response)
    }

    pub async fn send(self) -> Result<NodeResponse, NodeHttpError> {
        if let Some(e) = self.error {
            return Err(NodeHttpError::Invalid(e));
        }
        let mut route = self.route()?;
        if self.pinned {
            route.pin();
        }
        let mut first = route.primary;
        let other = first.other();
        // Demotion only reorders; the stricter stream gate is never moved to proactively.
        if let (Some(peer), Some(stats)) = (&route.peer, &route.stats) {
            let to_gated_stream = other == Transport::Stream
                && route.path.as_deref().is_some_and(requires_bound_peer);
            if route.has(other) && !to_gated_stream && stats.choose(peer, first, true) != first {
                first = other;
            }
        }
        match self.attempt(&route, first).await {
            Err(e) if route.has(first.other()) && fails_over(&e, &self.method) => {
                tracing::debug!(
                    ?first,
                    "node-http: transport failed, retrying over the other"
                );
                self.attempt(&route, first.other()).await
            }
            result => result,
        }
    }

    /// The transports this request may use. The URL's own kind is tried first; the other is
    /// there only when the peer table binds both to one peer and the stream gate allows the path.
    fn route(&self) -> Result<Route, NodeHttpError> {
        let handle = self.client.stream_handle();
        let table = handle.and_then(|h| h.peers.as_ref());
        let stats = table.map(|t| t.transport_stats().clone());
        if self.url.starts_with("p2p://") {
            let (peer, path) = split_p2p_url(&self.url)
                .ok_or_else(|| NodeHttpError::Invalid("bad p2p url".into()))?;
            let http = table
                .and_then(|t| t.http_url_for_libp2p_peer(&peer))
                .map(|base| format!("{}{path}", base.trim_end_matches('/')));
            return Ok(Route {
                primary: Transport::Stream,
                peer: Some(peer),
                http,
                path: Some(path),
                stream: true,
                stats,
            });
        }
        let peer = self.client.fallback_peer(&self.url);
        let path = http_path_and_query(&self.url);
        let stream = peer.is_some() && path.as_deref().is_some_and(path_allowed);
        Ok(Route {
            primary: Transport::Http,
            peer,
            http: Some(self.url.clone()),
            path,
            stream,
            stats,
        })
    }

    /// One try over `transport`, recording a transport-level outcome for the peer. Any answer,
    /// whatever its status, counts as the transport working.
    async fn attempt(
        &self,
        route: &Route,
        transport: Transport,
    ) -> Result<NodeResponse, NodeHttpError> {
        let start = std::time::Instant::now();
        let result = match transport {
            Transport::Http => {
                let url = route.http.as_deref().unwrap_or(&self.url);
                self.http_builder_to(url)
                    .send()
                    .await
                    .map(NodeResponse::from_http)
                    .map_err(NodeHttpError::from)
            }
            Transport::Stream => {
                let (Some(peer), Some(path)) = (route.peer, route.path.clone()) else {
                    return Err(NodeHttpError::Invalid("no stream route".into()));
                };
                self.send_stream(peer, path).await
            }
        };
        if let (Some(peer), Some(stats)) = (route.peer, &route.stats) {
            match &result {
                Ok(_) => stats.record_success(peer, transport, start.elapsed()),
                Err(e) if e.is_connect() || e.is_timeout() => stats.record_failure(peer, transport),
                Err(_) => {}
            }
        }
        result
    }
}

/// The transports one request may use, and where outcomes are recorded.
struct Route {
    primary: Transport,
    peer: Option<PeerId>,
    /// The full http(s) URL, when the request can go over HTTP.
    http: Option<String>,
    /// Path and query as a stream request carries them.
    path: Option<String>,
    stream: bool,
    stats: Option<TransportStats>,
}

impl Route {
    fn pin(&mut self) {
        match self.primary {
            Transport::Http => self.stream = false,
            Transport::Stream => self.http = None,
        }
    }

    fn has(&self, transport: Transport) -> bool {
        match transport {
            Transport::Http => self.http.is_some(),
            Transport::Stream => self.stream && self.peer.is_some() && self.path.is_some(),
        }
    }
}

/// Whether `err` allows trying the other transport: the request never got an answer. An
/// application status never does, since a refusal retried over a different path could be a
/// downgrade, and a write that timed out may already have been applied.
fn fails_over(err: &NodeHttpError, method: &Method) -> bool {
    err.is_connect() || (err.is_timeout() && method == Method::GET)
}

/// The path and query of an http(s) URL.
fn http_path_and_query(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let mut out = parsed.path().to_string();
    if let Some(q) = parsed.query() {
        out.push('?');
        out.push_str(q);
    }
    Some(out)
}

enum Inner {
    Http(reqwest::Response),
    Stream {
        status: StatusCode,
        headers: HeaderMap,
        body: Option<Bytes>,
    },
}

/// A response from either transport with the `reqwest::Response` methods call sites use.
pub struct NodeResponse(Inner);

impl NodeResponse {
    fn from_http(r: reqwest::Response) -> Self {
        Self(Inner::Http(r))
    }

    fn from_stream(r: NodeHttpResponse) -> Result<Self, NodeHttpError> {
        let status = StatusCode::from_u16(r.status)
            .map_err(|_| NodeHttpError::stream(StreamErrorKind::Protocol, "bad status"))?;
        let mut headers = HeaderMap::new();
        for (k, v) in r.headers {
            if let (Ok(k), Ok(v)) = (
                HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(&v),
            ) {
                headers.append(k, v);
            }
        }
        Ok(Self(Inner::Stream {
            status,
            headers,
            body: Some(Bytes::from(r.body)),
        }))
    }

    /// Whether this response came over a libp2p stream rather than an HTTP connection.
    pub fn via_stream(&self) -> bool {
        matches!(self.0, Inner::Stream { .. })
    }

    pub fn status(&self) -> StatusCode {
        match &self.0 {
            Inner::Http(r) => r.status(),
            Inner::Stream { status, .. } => *status,
        }
    }

    pub fn headers(&self) -> &HeaderMap {
        match &self.0 {
            Inner::Http(r) => r.headers(),
            Inner::Stream { headers, .. } => headers,
        }
    }

    pub fn error_for_status(self) -> Result<Self, NodeHttpError> {
        match self.0 {
            Inner::Http(r) => Ok(Self(Inner::Http(r.error_for_status()?))),
            Inner::Stream { status, .. }
                if status.is_client_error() || status.is_server_error() =>
            {
                Err(NodeHttpError::Status(status))
            }
            other => Ok(Self(other)),
        }
    }

    /// The next body chunk; a stream response arrives whole, already bounded by its limit.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, NodeHttpError> {
        match &mut self.0 {
            Inner::Http(r) => Ok(r.chunk().await?),
            Inner::Stream { body, .. } => Ok(body.take().filter(|b| !b.is_empty())),
        }
    }

    pub async fn bytes(self) -> Result<Bytes, NodeHttpError> {
        match self.0 {
            Inner::Http(r) => Ok(r.bytes().await?),
            Inner::Stream { body, .. } => Ok(body.unwrap_or_default()),
        }
    }

    pub async fn text(self) -> Result<String, NodeHttpError> {
        Ok(String::from_utf8_lossy(&self.bytes().await?).into_owned())
    }

    pub async fn json<T: DeserializeOwned>(self) -> Result<T, NodeHttpError> {
        match self.0 {
            Inner::Http(r) => Ok(r.json().await?),
            Inner::Stream { body, .. } => serde_json::from_slice(&body.unwrap_or_default())
                .map_err(|e| NodeHttpError::Decode(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::identity;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sample_peer_id() -> PeerId {
        identity::Keypair::generate_ed25519().public().into()
    }

    fn info(base_url: &str, peer: Option<&PeerId>, connectivity: Option<Connectivity>) -> PeerInfo {
        PeerInfo {
            identity_bound: true,
            base_url: base_url.to_string(),
            roles: vec!["combined".to_string()],
            protocol_version: "0.1.0".to_string(),
            network_id: "n".to_string(),
            last_announced_at: time::OffsetDateTime::now_utc(),
            libp2p_peer_id: peer.map(|p| p.to_string()),
            libp2p_listen_addrs: vec![],
            connectivity,
            witness: None,
        }
    }

    #[test]
    fn url_for_prefers_the_stream_only_when_the_url_is_not_the_way_in() {
        let id = sample_peer_id();
        let p2p = p2p_base_url(&id);
        let plain = |c| NodeClient::url_for(&info("http://a.test", Some(&id), c));
        assert_eq!(plain(None), "http://a.test");
        assert_eq!(plain(Some(Connectivity::Direct)), "http://a.test");
        assert_eq!(plain(Some(Connectivity::NatTraversed)), p2p);
        assert_eq!(plain(Some(Connectivity::Relayed)), p2p);
        assert_eq!(plain(Some(Connectivity::OutboundOnly)), p2p);
        // Unusable URL falls to the stream whatever the connectivity says.
        for c in [
            None,
            Some(Connectivity::Direct),
            Some(Connectivity::NatTraversed),
        ] {
            assert_eq!(NodeClient::url_for(&info("", Some(&id), c)), p2p);
            assert_eq!(NodeClient::url_for(&info("not a url", Some(&id), c)), p2p);
        }
        // An entry whose id was never vouched for by its own URL always uses the URL.
        let mut unbound = info("http://a.test", Some(&id), Some(Connectivity::Relayed));
        unbound.identity_bound = false;
        assert_eq!(NodeClient::url_for(&unbound), "http://a.test");
        unbound.base_url = String::new();
        assert_eq!(NodeClient::url_for(&unbound), "");
        // No libp2p id: nothing to fall back to.
        assert_eq!(
            NodeClient::url_for(&info("http://a.test", None, Some(Connectivity::Relayed))),
            "http://a.test"
        );
        assert_eq!(NodeClient::url_for(&info("", None, None)), "",);
    }

    #[test]
    fn p2p_urls_parse_with_case_preserved() {
        let id = sample_peer_id();
        assert_eq!(parse_p2p_base(&p2p_base_url(&id)), Some(id));
        assert_eq!(parse_p2p_base(&format!("{}/", p2p_base_url(&id))), Some(id));
        assert_eq!(parse_p2p_base("http://x"), None);
        assert_eq!(parse_p2p_base("p2p://nope"), None);
        let (peer, path) =
            split_p2p_url(&format!("{}/ledger/sth/latest?a=b", p2p_base_url(&id))).unwrap();
        assert_eq!((peer, path.as_str()), (id, "/ledger/sth/latest?a=b"));
    }

    #[test]
    fn query_pairs_are_appended_to_the_stream_path() {
        let c = NodeClient::new();
        let req = c
            .get("p2p://x/ledger/entries")
            .query(&[("shard_id", "a b"), ("n", "1")])
            .stream_request("/ledger/entries".into());
        assert_eq!(req.path_and_query, "/ledger/entries?shard_id=a+b&n=1");
        let req = c
            .post("p2p://x/a?z=1")
            .query(&[("k", "v")])
            .json(&serde_json::json!({"a": 1}))
            .stream_request("/a?z=1".into());
        assert_eq!(req.path_and_query, "/a?z=1&k=v");
        assert_eq!(req.method, "POST");
        assert!(req
            .headers
            .contains(&("content-type".into(), "application/json".into())));
        assert_eq!(req.body, br#"{"a":1}"#);
    }

    #[tokio::test]
    async fn a_p2p_request_without_a_transport_fails_cleanly() {
        let id = sample_peer_id();
        let err = NodeClient::new()
            .get(format!("{}/nodes/status", p2p_base_url(&id)))
            .send()
            .await
            .err()
            .unwrap();
        assert!(matches!(
            err,
            NodeHttpError::Stream {
                kind: StreamErrorKind::Unavailable,
                ..
            }
        ));
    }

    /// What the fake stream worker does with every request.
    #[derive(Clone)]
    enum Script {
        Answer(NodeHttpResponse),
        Fail(StreamErrorKind),
    }

    /// A stream handle whose worker follows `script`, recording the paths it was asked for.
    fn scripted_worker(
        peers: PeerTable,
        script: Script,
    ) -> (StreamHandle, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                if let DhtCommand::HttpRequest {
                    request,
                    respond_to,
                    ..
                } = cmd
                {
                    log.lock().unwrap().push(request.path_and_query);
                    let _ = respond_to.send(match &script {
                        Script::Answer(a) => Ok(a.clone()),
                        Script::Fail(kind) => Err(NodeHttpError::stream(*kind, "scripted")),
                    });
                }
            }
        });
        (
            StreamHandle {
                commands: tx,
                peers: Some(peers),
                settings: NodeHttpSettings::default(),
            },
            seen,
        )
    }

    /// A stream handle whose worker answers every request with `answer`, recording the paths.
    fn fake_worker(
        peers: PeerTable,
        answer: NodeHttpResponse,
    ) -> (StreamHandle, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        scripted_worker(peers, Script::Answer(answer))
    }

    fn table_with(id: &PeerId, base_url: &str) -> PeerTable {
        let table = PeerTable::new();
        table.upsert(info(base_url, Some(id), None));
        table
    }

    fn ok_answer() -> NodeHttpResponse {
        NodeHttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: br#"{"via":"stream"}"#.to_vec(),
        }
    }

    #[tokio::test]
    async fn a_connect_failure_to_a_peer_with_a_libp2p_id_retries_over_the_stream() {
        // A closed port: connect refused immediately.
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", dead.local_addr().unwrap());
        drop(dead);
        let id = sample_peer_id();
        let (handle, seen) = fake_worker(table_with(&id, &base), ok_answer());
        let client = NodeClient::new().with_stream(handle);
        let res = client
            .get(format!("{base}/ledger/sth/latest"))
            .query(&[("shard_id", "core")])
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body: serde_json::Value = res.json().await.unwrap();
        assert_eq!(body["via"], "stream");
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["/ledger/sth/latest?shard_id=core"]
        );
    }

    #[tokio::test]
    async fn an_unknown_peer_or_a_successful_http_call_never_touches_the_stream() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/nodes/status"))
            .respond_with(ResponseTemplate::new(200).set_body_string("http"))
            .expect(1)
            .mount(&server)
            .await;
        let id = sample_peer_id();
        let (handle, seen) = fake_worker(table_with(&id, &server.uri()), ok_answer());
        let client = NodeClient::new().with_stream(handle.clone());
        let res = client
            .get(format!("{}/nodes/status", server.uri()))
            .send()
            .await
            .unwrap();
        assert_eq!(res.text().await.unwrap(), "http");
        assert!(seen.lock().unwrap().is_empty());

        // A dead URL for a peer the table does not know: the error surfaces, no retry.
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", dead.local_addr().unwrap());
        drop(dead);
        let err = client.get(format!("{base}/x")).send().await.err().unwrap();
        assert!(err.is_connect());
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_timed_out_write_is_not_replayed_over_the_stream() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;
        let id = sample_peer_id();
        let (handle, seen) = fake_worker(table_with(&id, &server.uri()), ok_answer());
        let client = NodeClient::new().with_stream(handle);
        let err = client
            .post(format!("{}/nodes/announce", server.uri()))
            .timeout(Duration::from_millis(200))
            .send()
            .await
            .err()
            .unwrap();
        assert!(err.is_timeout());
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_timed_out_read_retries_over_the_stream() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;
        let id = sample_peer_id();
        let (handle, seen) = fake_worker(table_with(&id, &server.uri()), ok_answer());
        let client = NodeClient::new().with_stream(handle);
        let res = client
            .get(format!("{}/nodes/status", server.uri()))
            .timeout(Duration::from_millis(200))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stream_responses_expose_the_reqwest_style_accessors() {
        let id = sample_peer_id();
        let (handle, _) = fake_worker(
            PeerTable::new(),
            NodeHttpResponse {
                status: 404,
                headers: vec![("x-avalon-trace-hops".into(), "abc".into())],
                body: b"nope".to_vec(),
            },
        );
        let client = NodeClient::new().with_stream(handle);
        let mut res = client
            .get(format!("{}/x", p2p_base_url(&id)))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_eq!(res.headers().get("x-avalon-trace-hops").unwrap(), "abc");
        assert_eq!(res.chunk().await.unwrap().unwrap().as_ref(), b"nope");
        assert!(res.chunk().await.unwrap().is_none());
        let res = client
            .get(format!("{}/x", p2p_base_url(&id)))
            .send()
            .await
            .unwrap();
        let err = res.error_for_status().err().unwrap();
        assert_eq!(err.status(), Some(StatusCode::NOT_FOUND));
    }

    fn status_answer(status: u16) -> NodeHttpResponse {
        NodeHttpResponse {
            status,
            headers: vec![],
            body: b"refused".to_vec(),
        }
    }

    fn dead_base() -> String {
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", dead.local_addr().unwrap())
    }

    fn table_with_connectivity(
        id: &PeerId,
        base_url: &str,
        connectivity: Option<Connectivity>,
    ) -> PeerTable {
        let table = PeerTable::new();
        table.upsert(info(base_url, Some(id), connectivity));
        table
    }

    async fn http_hits(server: &MockServer) -> usize {
        server.received_requests().await.unwrap().len()
    }

    async fn http_server(status: u16) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(status).set_body_string("http"))
            .mount(&server)
            .await;
        server
    }

    #[test]
    fn the_hint_picks_a_transport_and_a_failing_one_is_demoted_across_the_matrix() {
        use Connectivity::*;
        let stats = TransportStats::default();
        let id = sample_peer_id();
        let p2p = p2p_base_url(&id);
        let url = "http://a.test";
        let pick =
            |base: &str, c| NodeClient::url_for_with(&info(base, Some(&id), c), Some(&stats));
        // Healthy: the hint decides, as before.
        assert_eq!(pick(url, Some(Direct)), url);
        assert_eq!(pick(url, Some(Relayed)), p2p);
        assert_eq!(pick(url, Some(OutboundOnly)), p2p);
        assert_eq!(pick(url, Some(NatTraversed)), p2p);
        assert_eq!(pick("", None), p2p);

        // The stream keeps failing for a peer the hint sends over it: fall back to its URL.
        stats.record_failure(id, Transport::Stream);
        assert_eq!(pick(url, Some(Relayed)), url);
        assert_eq!(pick(url, Some(OutboundOnly)), url);
        // A url-less peer has nothing to fall back to, so the demotion changes nothing.
        assert_eq!(pick("", Some(Relayed)), p2p);
        assert_eq!(pick("p2p://x", None), p2p);
        // A direct hint is unaffected by stream trouble.
        assert_eq!(pick(url, Some(Direct)), url);

        // A stale hint (claims direct, but its URL no longer connects): the stream takes over.
        stats.record_failure(id, Transport::Http);
        stats.record_success(id, Transport::Stream, Duration::from_millis(1));
        assert_eq!(pick(url, Some(Direct)), p2p);
        assert_eq!(pick(url, None), p2p);
        // Both failing: the hint stands again.
        stats.record_failure(id, Transport::Stream);
        assert_eq!(pick(url, Some(Direct)), url);
        assert_eq!(pick(url, Some(Relayed)), p2p);

        // An entry whose id was never bound is never moved off its URL.
        let mut unbound = info(url, Some(&id), Some(Relayed));
        unbound.identity_bound = false;
        assert_eq!(NodeClient::url_for_with(&unbound, Some(&stats)), url);
    }

    #[test]
    fn the_peer_table_url_follows_recorded_outcomes() {
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, "http://a.test", Some(Connectivity::Direct));
        assert_eq!(table.transport_url("http://a.test"), "http://a.test");
        table.transport_stats().record_failure(id, Transport::Http);
        assert_eq!(table.transport_url("http://a.test"), p2p_base_url(&id));
    }

    #[tokio::test]
    async fn http_failure_fails_over_to_the_stream_and_the_next_request_skips_http() {
        let base = dead_base();
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, &base, Some(Connectivity::Direct));
        let (handle, seen) = fake_worker(table.clone(), ok_answer());
        let client = NodeClient::new().with_stream(handle);
        let res = client
            .get(format!("{base}/nodes/status"))
            .send()
            .await
            .unwrap();
        assert!(res.via_stream());
        assert!(table.transport_stats().is_demoted(&id, Transport::Http));
        // Demoted: a second request goes straight to the stream (a live URL would show a hit).
        let live = http_server(200).await;
        let table2 = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Direct));
        table2.transport_stats().record_failure(id, Transport::Http);
        let (handle2, seen2) = fake_worker(table2, ok_answer());
        let res = NodeClient::new()
            .with_stream(handle2)
            .get(format!("{}/nodes/status", live.uri()))
            .send()
            .await
            .unwrap();
        assert!(res.via_stream());
        assert_eq!(http_hits(&live).await, 0);
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(seen2.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stream_failure_fails_over_to_http_for_a_peer_with_a_url() {
        let live = http_server(200).await;
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Relayed));
        let mut table = table;
        for kind in [StreamErrorKind::Connect, StreamErrorKind::Timeout] {
            table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Relayed));
            let (handle, seen) = scripted_worker(table.clone(), Script::Fail(kind));
            let client = NodeClient::new().with_stream(handle);
            let res = client
                .get(format!("{}/nodes/status", p2p_base_url(&id)))
                .send()
                .await
                .unwrap();
            assert!(!res.via_stream(), "{kind:?}");
            assert_eq!(res.text().await.unwrap(), "http");
            assert!(table.transport_stats().is_demoted(&id, Transport::Stream));
            assert_eq!(seen.lock().unwrap().len(), 1);
        }
        assert!(table.transport_stats().is_demoted(&id, Transport::Stream));
        // Demoted: the next request does not even try the stream.
        let (handle, seen) = scripted_worker(table.clone(), Script::Fail(StreamErrorKind::Connect));
        let res = NodeClient::new()
            .with_stream(handle)
            .get(format!("{}/nodes/status", p2p_base_url(&id)))
            .send()
            .await
            .unwrap();
        assert!(!res.via_stream());
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_application_status_never_changes_transport() {
        for status in [400u16, 401, 403, 404, 429, 500, 503] {
            // HTTP answers with a refusal: the stream is not tried and HTTP stays healthy.
            let live = http_server(status).await;
            let id = sample_peer_id();
            let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Direct));
            let (handle, seen) = fake_worker(table.clone(), ok_answer());
            let res = NodeClient::new()
                .with_stream(handle)
                .get(format!("{}/nodes/status", live.uri()))
                .send()
                .await
                .unwrap();
            assert_eq!(res.status().as_u16(), status);
            assert!(!res.via_stream());
            assert!(seen.lock().unwrap().is_empty(), "{status}");
            assert!(!table.transport_stats().is_demoted(&id, Transport::Http));

            // The stream answers with a refusal: HTTP is not tried and the stream stays healthy.
            let live = http_server(200).await;
            let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Relayed));
            let (handle, seen) =
                scripted_worker(table.clone(), Script::Answer(status_answer(status)));
            let res = NodeClient::new()
                .with_stream(handle)
                .post(format!("{}/nodes/relay", p2p_base_url(&id)))
                .send()
                .await
                .unwrap();
            assert_eq!(res.status().as_u16(), status);
            assert!(res.via_stream());
            assert_eq!(seen.lock().unwrap().len(), 1);
            assert_eq!(http_hits(&live).await, 0, "{status}");
            assert!(!table.transport_stats().is_demoted(&id, Transport::Stream));
        }
    }

    #[tokio::test]
    async fn non_transport_stream_errors_do_not_fail_over() {
        let live = http_server(200).await;
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Relayed));
        for kind in [StreamErrorKind::Protocol, StreamErrorKind::Unavailable] {
            let (handle, _) = scripted_worker(table.clone(), Script::Fail(kind));
            let err = NodeClient::new()
                .with_stream(handle)
                .get(format!("{}/nodes/status", p2p_base_url(&id)))
                .send()
                .await
                .err()
                .unwrap();
            assert!(matches!(err, NodeHttpError::Stream { kind: k, .. } if k == kind));
        }
        assert_eq!(http_hits(&live).await, 0);
        assert!(!table.transport_stats().is_demoted(&id, Transport::Stream));
    }

    #[tokio::test]
    async fn a_timed_out_stream_write_is_not_replayed_over_http_but_a_connect_failure_is() {
        let live = http_server(200).await;
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Relayed));
        let (handle, _) = scripted_worker(table.clone(), Script::Fail(StreamErrorKind::Timeout));
        let err = NodeClient::new()
            .with_stream(handle)
            .post(format!("{}/nodes/announce", p2p_base_url(&id)))
            .send()
            .await
            .err()
            .unwrap();
        assert!(err.is_timeout());
        assert_eq!(http_hits(&live).await, 0);
        // The timeout still counts against the stream.
        assert!(table.transport_stats().is_demoted(&id, Transport::Stream));

        let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Relayed));
        let (handle, _) = scripted_worker(table, Script::Fail(StreamErrorKind::Connect));
        let res = NodeClient::new()
            .with_stream(handle)
            .post(format!("{}/nodes/announce", p2p_base_url(&id)))
            .send()
            .await
            .unwrap();
        assert!(!res.via_stream());
        assert_eq!(http_hits(&live).await, 1);
    }

    #[tokio::test]
    async fn a_peer_with_no_usable_url_has_no_second_transport() {
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, &p2p_base_url(&id), None);
        let (handle, seen) = scripted_worker(table.clone(), Script::Fail(StreamErrorKind::Connect));
        let client = NodeClient::new().with_stream(handle);
        for _ in 0..2 {
            let err = client
                .get(format!("{}/nodes/status", p2p_base_url(&id)))
                .send()
                .await
                .err()
                .unwrap();
            assert!(err.is_connect());
        }
        // Demoted yet still tried: the record never removes the only way in.
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn an_expired_backoff_restores_the_preferred_transport() {
        let live = http_server(200).await;
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Direct));
        let long_ago = std::time::Instant::now()
            .checked_sub(crate::transport_stats::BACKOFF_BASE + Duration::from_secs(1))
            .unwrap();
        table
            .transport_stats()
            .record_failure_at(id, Transport::Http, long_ago);
        assert!(!table.transport_stats().is_demoted(&id, Transport::Http));
        assert_eq!(table.transport_url(&live.uri()), live.uri());
        let (handle, seen) = fake_worker(table, ok_answer());
        let res = NodeClient::new()
            .with_stream(handle)
            .get(format!("{}/nodes/status", live.uri()))
            .send()
            .await
            .unwrap();
        assert!(!res.via_stream());
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_stream_gate_is_never_moved_to_proactively_and_only_allowed_paths_fail_over() {
        // HTTP is demoted but the path needs a bound peer on the stream: stay on HTTP.
        let live = http_server(200).await;
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Direct));
        table.transport_stats().record_failure(id, Transport::Http);
        let (handle, seen) = fake_worker(table, ok_answer());
        let client = NodeClient::new().with_stream(handle);
        let res = client
            .post(format!("{}/nodes/relay", live.uri()))
            .send()
            .await
            .unwrap();
        assert!(!res.via_stream());
        assert!(seen.lock().unwrap().is_empty());

        // A path the stream gate does not allow gets the connect error, not a stream refusal.
        let base = dead_base();
        let table = table_with_connectivity(&id, &base, Some(Connectivity::Direct));
        let (handle, seen) = fake_worker(table, ok_answer());
        let err = NodeClient::new()
            .with_stream(handle)
            .get(format!("{base}/identities/x"))
            .send()
            .await
            .err()
            .unwrap();
        assert!(err.is_connect());
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn many_failing_peers_keep_the_record_bounded() {
        let base = dead_base();
        let table = PeerTable::new();
        let (handle, _) = fake_worker(table.clone(), ok_answer());
        let client = NodeClient::new().with_stream(handle);
        let ids: Vec<_> = (0..40).map(|_| sample_peer_id()).collect();
        for id in &ids {
            table.upsert(info(&format!("{base}/{id}"), Some(id), None));
            let _ = client.get(format!("{base}/{id}/nodes/status")).send().await;
        }
        assert!(table.transport_stats().tracked() <= crate::transport_stats::MAX_TRACKED_PEERS);
        assert_eq!(table.transport_stats().tracked(), ids.len());
    }

    #[tokio::test]
    async fn an_answer_of_any_status_counts_as_the_transport_working() {
        let live = http_server(503).await;
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Direct));
        let past = std::time::Instant::now()
            .checked_sub(crate::transport_stats::BACKOFF_BASE + Duration::from_secs(1))
            .unwrap();
        table
            .transport_stats()
            .record_failure_at(id, Transport::Http, past);
        let (handle, _) = fake_worker(table.clone(), ok_answer());
        let res = NodeClient::new()
            .with_stream(handle)
            .get(format!("{}/nodes/status", live.uri()))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 503);
        let o = table
            .transport_stats()
            .outcome(&id, Transport::Http)
            .unwrap();
        assert_eq!(o.consecutive_failures, 0);
        assert!(o.last_success.is_some() && o.rtt.is_some());
    }

    #[tokio::test]
    async fn a_pinned_request_never_changes_transport() {
        let live = http_server(200).await;
        let id = sample_peer_id();
        // Stream-primary, stream down, URL up: stays an error.
        let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Relayed));
        let (handle, _) = scripted_worker(table.clone(), Script::Fail(StreamErrorKind::Connect));
        let client = NodeClient::new().with_stream(handle);
        let err = client
            .post(format!("{}/nodes/announce", p2p_base_url(&id)))
            .pinned()
            .send()
            .await
            .err()
            .unwrap();
        assert!(err.is_connect());
        assert_eq!(http_hits(&live).await, 0);
        // Demoted stream is still the only transport tried.
        let (handle, seen) = scripted_worker(table, Script::Fail(StreamErrorKind::Connect));
        let _ = NodeClient::new()
            .with_stream(handle)
            .post(format!("{}/nodes/announce", p2p_base_url(&id)))
            .pinned()
            .send()
            .await;
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(http_hits(&live).await, 0);

        // HTTP-primary, URL down, stream up: stays an error.
        let base = dead_base();
        let table = table_with_connectivity(&id, &base, Some(Connectivity::Direct));
        let (handle, seen) = fake_worker(table, ok_answer());
        let err = NodeClient::new()
            .with_stream(handle)
            .get(format!("{base}/nodes/status"))
            .pinned()
            .send()
            .await
            .err()
            .unwrap();
        assert!(err.is_connect());
        assert!(seen.lock().unwrap().is_empty());
    }
}
