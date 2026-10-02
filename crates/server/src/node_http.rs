//! Node-to-node HTTP that also works for peers with no reachable URL.
//!
//! [`NodeClient`] is a drop-in for the slice of `reqwest` the node-to-node call sites use.
//! `http(s)://` URLs go through the wrapped `reqwest::Client` untouched (including the pinned
//! clients `crate::outbound_policy` builds); `p2p://<libp2p peer id>` URLs are carried as one
//! request-response exchange on a libp2p stream, so relays and hole punching apply to them.
//! A connect error or timeout on one transport falls back to the other when the peer has both,
//! and recent outcomes demote a failing one; see [`crate::transport_stats`]. Stream-to-HTTP
//! failover is for reads only, policy-checked, and drops credential headers.
//! The receiving side dispatches into the normal router; see [`inbound`].

mod inbound;
mod signer;
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
use crate::node_auth::{normalized_origin, CREDENTIAL_PATHS};
use crate::nodes::{PeerInfo, PeerTable};
use crate::outbound_policy::{CheckedTarget, OutboundPolicy};
use crate::transport_stats::{Transport, TransportStats};
use avalon_protocol::connectivity::Connectivity;
use avalon_protocol::node_request::NODE_REQUEST_HEADER;

pub use inbound::{
    path_allowed, synthetic_addr, InboundPermit, InboundService, RemotePeer, RouterSlot,
    ALLOWED_EXACT, SHARED_PEER_ADDR,
};
pub use signer::NodeSigner;
pub use wire::{
    protocol_name, BufferGrant, NodeHttpCodec, NodeHttpRequest, NodeHttpResponse, NodeHttpSettings,
    MAX_CONCURRENT_STREAMS, MAX_HEADER_BYTES, MAX_HEADER_COUNT,
};

/// Longest a failover target's policy check (a DNS lookup) may take.
const VET_TIMEOUT: Duration = Duration::from_secs(3);

/// Headers forwarded to a peer-table URL the stream did not vouch for; anything else, which may
/// carry a credential, is dropped. A failover GET's query must be non-secret.
fn is_forwarded_header(name: &str) -> bool {
    ["content-type", "accept", crate::op_trace::TRACE_HEADER]
        .iter()
        .any(|allowed| name.eq_ignore_ascii_case(allowed))
}

/// Runs a policy check under [`VET_TIMEOUT`]; a lookup that hangs counts as a refusal.
async fn bounded_check<E>(
    check: impl std::future::Future<Output = Result<CheckedTarget, E>>,
) -> Option<CheckedTarget> {
    tokio::time::timeout(VET_TIMEOUT, check)
        .await
        .ok()
        .and_then(Result::ok)
}

/// URL scheme for a peer reached over a libp2p stream.
pub const P2P_SCHEME: &str = "p2p";

/// Whether `url` names a `p2p://` peer; the scheme is lowercase only, so `P2P://x` is not one.
pub fn is_p2p_url(url: &str) -> bool {
    url.strip_prefix(P2P_SCHEME)
        .is_some_and(|rest| rest.starts_with("://"))
}

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
    /// The request was never sent: the peer could not be dialed or did not speak the protocol.
    Connect,
    /// The request was never sent because of a local limit or missing address, not the peer.
    Local,
    /// The connection closed before a response; the peer may have processed the request.
    Dropped,
    /// No response in time on a connection that may have carried the request.
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

    /// Not sent for a local reason: it falls over like a connect failure but never demotes.
    pub fn is_local(&self) -> bool {
        matches!(
            self,
            Self::Stream {
                kind: StreamErrorKind::Local,
                ..
            }
        )
    }

    /// The stream closed before a response, so a write may already have been applied.
    pub fn is_dropped(&self) -> bool {
        matches!(
            self,
            Self::Stream {
                kind: StreamErrorKind::Dropped,
                ..
            }
        )
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

/// The process-wide signer for the write routes. Set once at startup; a client with its own
/// signer ignores it, and one with neither sends those routes unsigned.
static GLOBAL_SIGNER: OnceLock<NodeSigner> = OnceLock::new();

/// Registers the signer for every [`NodeClient`] without its own.
pub fn install_node_signer(signer: NodeSigner) {
    let _ = GLOBAL_SIGNER.set(signer);
}

/// Cheap to clone; see the module docs.
#[derive(Clone)]
pub struct NodeClient {
    http: reqwest::Client,
    stream: Option<StreamHandle>,
    signer: Option<NodeSigner>,
    /// Timeout for stream requests, matching the wrapped client's own where it has one.
    timeout: Option<Duration>,
    /// Policy a peer-table URL must pass before a stream failure falls over to it.
    policy: Option<OutboundPolicy>,
    /// Set by [`NodeClient::guarded`]: http(s) dials are held to the policy at connect time.
    guarded: bool,
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
            signer: None,
            timeout: None,
            policy: None,
            guarded: false,
        }
    }
}

impl NodeClient {
    /// A plain client with no timeout of its own (stream requests use the configured default).
    pub fn new() -> Self {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .unwrap_or_default()
            .into()
    }

    /// The bounded client for requests to other nodes; see [`crate::outbound_policy::peer_client`].
    pub fn peer() -> Self {
        Self::from(crate::outbound_policy::peer_client())
            .with_timeout(crate::outbound_policy::PEER_REQUEST_TIMEOUT)
    }

    /// [`Self::peer`] for addresses read from lookups: every http(s) dial must pass the
    /// environment's outbound policy, checked when the connection is made.
    pub fn guarded() -> Self {
        Self::guarded_with(OutboundPolicy::from_env())
    }

    /// [`Self::guarded`] under an explicit `policy`.
    pub fn guarded_with(policy: OutboundPolicy) -> Self {
        let mut client = Self::from(crate::outbound_policy::guarded_peer_client(policy))
            .with_timeout(crate::outbound_policy::PEER_REQUEST_TIMEOUT)
            .with_policy(policy);
        client.guarded = true;
        client
    }

    /// Wraps `http`, giving stream requests the same `timeout` it enforces.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Checks stream-to-HTTP failover targets with `policy` instead of the environment's.
    pub fn with_policy(mut self, policy: OutboundPolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    /// Uses `handle` instead of the process-wide one.
    pub fn with_stream(mut self, handle: StreamHandle) -> Self {
        self.stream = Some(handle);
        self
    }

    /// Signs the write routes with `signer` instead of the process-wide one.
    pub fn with_signer(mut self, signer: NodeSigner) -> Self {
        self.signer = Some(signer);
        self
    }

    fn signer(&self) -> Option<&NodeSigner> {
        self.signer.as_ref().or_else(|| GLOBAL_SIGNER.get())
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

    /// `unvouched` forwards only allowlisted headers and no body, for a URL the stream did not vouch for.
    fn http_builder_with(
        &self,
        client: &reqwest::Client,
        url: &str,
        unvouched: bool,
    ) -> reqwest::RequestBuilder {
        let mut rb = client.request(self.method.clone(), url);
        if !self.query.is_empty() {
            rb = rb.query(&self.query);
        }
        for (k, v) in &self.headers {
            let credential = k.eq_ignore_ascii_case(NODE_REQUEST_HEADER);
            if !credential && (!unvouched || is_forwarded_header(k)) {
                rb = rb.header(k.as_str(), v.as_str());
            }
        }
        if let Some(body) = self.body.as_ref().filter(|_| !unvouched) {
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
            // A stream is authenticated by its handshake and never carries the credential.
            headers: self
                .headers
                .iter()
                .filter(|(k, _)| !k.eq_ignore_ascii_case(NODE_REQUEST_HEADER))
                .cloned()
                .collect(),
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
        // An http target the caller did not name must pass the outbound policy before any dial.
        let mut vetted: Option<CheckedTarget> = None;
        // Demotion only reorders; it never adds a transport the route lacks.
        if let (Some(peer), Some(stats)) = (&route.peer, &route.stats) {
            if route.has(other) && stats.choose(peer, first, true) != first {
                if other == Transport::Http {
                    vetted = self.vet_http(&route).await;
                }
                if other == Transport::Stream || vetted.is_some() {
                    first = other;
                }
            }
        }
        let err = match self.attempt(&route, first, vetted.as_ref()).await {
            Err(e) if route.has(first.other()) && fails_over(&e, &self.method) => e,
            result => return result,
        };
        if first.other() == Transport::Http && route.primary == Transport::Stream {
            vetted = self.vet_http(&route).await;
            if vetted.is_none() {
                return Err(err);
            }
        }
        tracing::debug!(
            ?first,
            "node-http: transport failed, retrying over the other"
        );
        self.attempt(&route, first.other(), vetted.as_ref()).await
    }

    /// The transports this request may use. The URL's own kind is tried first; the other is
    /// there only when the peer table binds both to one peer and the stream allowlist has the path.
    fn route(&self) -> Result<Route, NodeHttpError> {
        let handle = self.client.stream_handle();
        let table = handle.and_then(|h| h.peers.as_ref());
        let stats = || table.map(|t| t.transport_stats().clone());
        if is_p2p_url(&self.url) {
            let (peer, path) = split_p2p_url(&self.url)
                .ok_or_else(|| NodeHttpError::Invalid("bad p2p url".into()))?;
            let (holders, url) = table
                .map(|t| t.bound_http_url_for_libp2p_peer(&peer))
                .unwrap_or((0, None));
            // Binding is self-reported, so only a read may leave the authenticated stream for the
            // table's URL: a write could hand non-public data to whoever claimed the id.
            let alt_http_base = url.filter(|_| self.method == Method::GET);
            return Ok(Route {
                primary: Transport::Stream,
                peer: Some(peer),
                http: None,
                alt_http_base,
                path: Some(path),
                stream: true,
                stats: if holders <= 1 { stats() } else { None },
            });
        }
        let peer = self.client.fallback_peer(&self.url);
        let path = http_path_and_query(&self.url);
        let stream = peer.is_some() && path.as_deref().is_some_and(path_allowed);
        Ok(Route {
            primary: Transport::Http,
            peer,
            http: Some(self.url.clone()),
            alt_http_base: None,
            path,
            stream,
            stats: peer.and_then(|_| stats()),
        })
    }

    /// The table's http URL for a stream-first request, resolved and pinned under the policy;
    /// `None` when there is none or it is refused. Both answers are reused for a while, and the
    /// resolution is bounded.
    async fn vet_http(&self, route: &Route) -> Option<CheckedTarget> {
        let base = route.alt_http_base.as_deref()?;
        let policy = self.client.policy.unwrap_or_else(OutboundPolicy::from_env);
        let (peer, stats) = (route.peer?, route.stats.as_ref()?);
        let now = std::time::Instant::now();
        if let Some(hit) = stats.cached_vet(peer, base, policy.allow_private, now) {
            return hit;
        }
        let vetted = bounded_check(policy.check_base_url(base)).await;
        stats.store_vet(peer, base, policy.allow_private, vetted.clone(), now);
        vetted
    }

    /// The timeout this request runs under on either transport.
    fn effective_timeout(&self) -> Duration {
        self.timeout.or(self.client.timeout).unwrap_or(
            self.client
                .stream_handle()
                .map_or(crate::outbound_policy::PEER_REQUEST_TIMEOUT, |h| {
                    h.settings.timeout
                }),
        )
    }

    /// Adds a freshly signed credential to a request for one of the write routes; any other
    /// request, and any request while no signer is installed, is returned as it is.
    fn sign_http(
        &self,
        rb: reqwest::RequestBuilder,
        route: &Route,
        url: &str,
    ) -> Result<reqwest::RequestBuilder, NodeHttpError> {
        let (Some(signer), Ok(parsed)) = (self.client.signer(), url::Url::parse(url)) else {
            return Ok(rb);
        };
        let path = parsed.path();
        if !CREDENTIAL_PATHS.contains(&path) {
            return Ok(rb);
        }
        // The peer's libp2p id when the table has one, else the URL it is reached at.
        let recipient = route
            .peer
            .map(|p| p.to_string())
            .or_else(|| normalized_origin(url))
            .ok_or_else(|| NodeHttpError::Invalid("no recipient for a signed request".into()))?;
        let body = self.body.as_deref().unwrap_or_default();
        let header = signer
            .header(self.method.as_str(), path, body, &recipient)
            .map_err(|e| NodeHttpError::Invalid(format!("cannot sign node request: {e}")))?;
        Ok(rb.header(NODE_REQUEST_HEADER, header))
    }

    /// One try over `transport`, recording a transport-level outcome for the peer. Any answer,
    /// whatever its status, counts as the transport working.
    async fn attempt(
        &self,
        route: &Route,
        transport: Transport,
        vetted: Option<&CheckedTarget>,
    ) -> Result<NodeResponse, NodeHttpError> {
        let start = std::time::Instant::now();
        let result = match transport {
            Transport::Http => {
                let rb = match (vetted, route.primary) {
                    (Some(target), Transport::Stream) => {
                        let path = route.path.as_deref().unwrap_or("/");
                        let url = format!("{}{path}", target.base_url);
                        // Built per request so each caller's own timeout applies.
                        let client = target.client(self.effective_timeout());
                        self.http_builder_with(&client, &url, true)
                    }
                    (None, Transport::Stream) => {
                        return Err(NodeHttpError::Invalid("no vetted http target".into()))
                    }
                    _ => {
                        let url = route.http.as_deref().unwrap_or(&self.url);
                        if let Some(policy) = self.client.policy.filter(|_| self.client.guarded) {
                            policy
                                .check_url_literal(url)
                                .map_err(|e| NodeHttpError::Invalid(e.to_string()))?;
                        }
                        let rb = self.http_builder_with(&self.client.http, url, false);
                        self.sign_http(rb, route, url)?
                    }
                };
                rb.send()
                    .await
                    .inspect(warn_on_redirect)
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
                // Only a request that never connected demotes; a slow answer does not.
                Err(e) if e.is_connect() => stats.record_failure(peer, transport),
                Err(_) => {}
            }
        }
        result
    }
}

/// `scheme://host[:port]/path` of `url`, without userinfo or query, for logs.
fn loggable_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(u) => format!("{}{}", u.origin().ascii_serialization(), u.path()),
        Err(_) => "<unparsable url>".to_string(),
    }
}

/// The line logged when a node answers a node-to-node request with a redirect.
fn redirect_notice(target: &str, location: Option<&str>) -> String {
    format!(
        "node-to-node request to {} was answered with a redirect to {}; redirects are not \
         followed, configure the final URL",
        loggable_url(target),
        location.map_or("<none>".to_string(), loggable_url)
    )
}

/// Makes a 3xx answer visible, once per target per interval: it is returned unfollowed.
fn warn_on_redirect(response: &reqwest::Response) {
    if !response.status().is_redirection() {
        return;
    }
    static LOG: OnceLock<crate::log_throttle::LogThrottle> = OnceLock::new();
    let log = LOG.get_or_init(|| crate::log_throttle::LogThrottle::new(Duration::from_secs(300)));
    let target = loggable_url(response.url().as_str());
    if let Some(held_back) = log.permit(&target, std::time::Instant::now()) {
        let location = response
            .headers()
            .get(axum::http::header::LOCATION)
            .and_then(|v| v.to_str().ok());
        tracing::warn!(held_back, status = %response.status(), "{}", redirect_notice(response.url().as_str(), location));
    }
}

/// The transports one request may use, and where outcomes are recorded.
struct Route {
    primary: Transport,
    peer: Option<PeerId>,
    /// The full http(s) URL the caller named, for an http-first request.
    http: Option<String>,
    /// The table's http base URL, for a stream-first request.
    alt_http_base: Option<String>,
    /// Path and query as a stream request carries them.
    path: Option<String>,
    stream: bool,
    /// Present only when one bound entry holds the peer id, so a failure is that URL's own.
    stats: Option<TransportStats>,
}

impl Route {
    fn pin(&mut self) {
        match self.primary {
            Transport::Http => self.stream = false,
            Transport::Stream => self.alt_http_base = None,
        }
    }

    fn has(&self, transport: Transport) -> bool {
        match transport {
            Transport::Http => self.http.is_some() || self.alt_http_base.is_some(),
            Transport::Stream => self.stream && self.peer.is_some() && self.path.is_some(),
        }
    }
}

/// Whether `err` allows trying the other transport. A connect failure never reached the peer; a
/// timeout may have been processed, so only a read retries. A dropped stream, which is also how a
/// peer's connection limit shows, and an application status never fail over.
fn fails_over(err: &NodeHttpError, method: &Method) -> bool {
    err.is_connect() || err.is_local() || (err.is_timeout() && method == Method::GET)
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
    use crate::outbound_policy::OutboundPolicy;
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

    /// A client over `handle` that may fail over to loopback test servers.
    fn lax_client(handle: StreamHandle) -> NodeClient {
        NodeClient::new()
            .with_stream(handle)
            .with_policy(OutboundPolicy::new(true))
    }

    #[tokio::test]
    async fn stream_failure_fails_over_to_http_for_a_peer_with_a_url() {
        let live = http_server(200).await;
        let id = sample_peer_id();
        // A never-sent failure demotes; a slow read falls over without demoting.
        for (kind, demoted) in [
            (StreamErrorKind::Connect, true),
            (StreamErrorKind::Timeout, false),
        ] {
            let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Relayed));
            let (handle, seen) = scripted_worker(table.clone(), Script::Fail(kind));
            let res = lax_client(handle)
                .get(format!("{}/nodes/status", p2p_base_url(&id)))
                .send()
                .await
                .unwrap();
            assert!(!res.via_stream(), "{kind:?}");
            assert_eq!(res.text().await.unwrap(), "http");
            assert_eq!(seen.lock().unwrap().len(), 1);
            assert_eq!(
                table.transport_stats().is_demoted(&id, Transport::Stream),
                demoted,
                "{kind:?}"
            );
        }
        // Demoted: the next request does not even try the stream.
        let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Relayed));
        table
            .transport_stats()
            .record_failure(id, Transport::Stream);
        let (handle, seen) = scripted_worker(table, Script::Fail(StreamErrorKind::Connect));
        let res = lax_client(handle)
            .get(format!("{}/nodes/status", p2p_base_url(&id)))
            .send()
            .await
            .unwrap();
        assert!(!res.via_stream());
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_dropped_stream_never_fails_over() {
        let live = http_server(200).await;
        let id = sample_peer_id();
        for get in [true, false] {
            let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Relayed));
            let (handle, _) =
                scripted_worker(table.clone(), Script::Fail(StreamErrorKind::Dropped));
            let url = format!("{}/nodes/status", p2p_base_url(&id));
            let client = lax_client(handle);
            let req = if get {
                client.get(url)
            } else {
                client.post(url)
            };
            assert!(req.send().await.err().unwrap().is_dropped());
            assert_eq!(http_hits(&live).await, 0);
            assert!(!table.transport_stats().is_demoted(&id, Transport::Stream));
        }
    }

    #[tokio::test]
    async fn a_stream_write_never_leaves_the_stream_for_the_tables_url() {
        let live = http_server(200).await;
        let id = sample_peer_id();
        for kind in [
            StreamErrorKind::Connect,
            StreamErrorKind::Timeout,
            StreamErrorKind::Dropped,
        ] {
            let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Relayed));
            let (handle, seen) = scripted_worker(table.clone(), Script::Fail(kind));
            let client = lax_client(handle);
            for method in [Method::POST, Method::PUT, Method::DELETE] {
                let err = client
                    .request(method, format!("{}/nodes/announce", p2p_base_url(&id)))
                    .body("private")
                    .send()
                    .await
                    .err()
                    .unwrap();
                assert!(matches!(err, NodeHttpError::Stream { kind: k, .. } if k == kind));
            }
            assert_eq!(http_hits(&live).await, 0, "{kind:?}");
            assert_eq!(seen.lock().unwrap().len(), 3);
        }
        // Even demoted, a write stays on the stream rather than swapping to the table's URL.
        let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Relayed));
        table
            .transport_stats()
            .record_failure(id, Transport::Stream);
        let (handle, seen) = scripted_worker(table, Script::Answer(ok_answer()));
        let res = lax_client(handle)
            .post(format!("{}/nodes/announce", p2p_base_url(&id)))
            .send()
            .await
            .unwrap();
        assert!(res.via_stream());
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(http_hits(&live).await, 0);
    }

    #[test]
    fn only_allowlisted_headers_are_forwarded_to_an_unvouched_url() {
        for h in ["Content-Type", "accept", "X-Avalon-Trace"] {
            assert!(is_forwarded_header(h), "{h}");
        }
        for h in [
            "authorization",
            "cookie",
            "proxy-authorization",
            "x-api-key",
            "x-api-foo",
            "api-key",
            "x-token",
            "x-auth-token",
            "x-avalon-integrator-key-id",
            "x-trace",
        ] {
            assert!(!is_forwarded_header(h), "{h}");
        }
    }

    #[tokio::test]
    async fn a_read_failing_over_to_http_forwards_only_allowlisted_headers_and_no_body() {
        let live = http_server(200).await;
        let id = sample_peer_id();
        // The sole bound holder of the id could be anyone: the id binding is self-reported.
        let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Relayed));
        let (handle, _) = scripted_worker(table, Script::Fail(StreamErrorKind::Connect));
        let res = lax_client(handle)
            .get(format!("{}/nodes/status", p2p_base_url(&id)))
            .bearer_auth("secret")
            .header("Cookie", "s=1")
            .header("proxy-authorization", "x")
            .header("x-api-foo", "k")
            .header("api-key", "k")
            .header("x-token", "t")
            .header("accept", "text/plain")
            .header(crate::op_trace::TRACE_HEADER, "trace")
            .body("not for strangers")
            .send()
            .await
            .unwrap();
        assert!(!res.via_stream());
        let received = live.received_requests().await.unwrap();
        let headers = &received[0].headers;
        for h in [
            "authorization",
            "cookie",
            "proxy-authorization",
            "x-api-foo",
            "api-key",
            "x-token",
        ] {
            assert!(!headers.contains_key(h), "{h} leaked");
        }
        assert!(headers.contains_key("accept"));
        assert!(headers.contains_key(crate::op_trace::TRACE_HEADER));
        assert!(received[0].body.is_empty());
    }

    #[tokio::test]
    async fn an_http_first_request_keeps_its_own_credentials() {
        let live = http_server(200).await;
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Direct));
        let (handle, _) = fake_worker(table, ok_answer());
        lax_client(handle)
            .get(format!("{}/nodes/status", live.uri()))
            .bearer_auth("secret")
            .send()
            .await
            .unwrap();
        let received = live.received_requests().await.unwrap();
        assert!(received[0].headers.contains_key("authorization"));
    }

    #[test]
    fn only_a_never_sent_failure_or_a_timed_out_read_fails_over() {
        let stream = |kind| NodeHttpError::stream(kind, "x");
        for (kind, get, post) in [
            (StreamErrorKind::Connect, true, true),
            (StreamErrorKind::Local, true, true),
            (StreamErrorKind::Dropped, false, false),
            (StreamErrorKind::Timeout, true, false),
            (StreamErrorKind::Protocol, false, false),
            (StreamErrorKind::Unavailable, false, false),
        ] {
            assert_eq!(fails_over(&stream(kind), &Method::GET), get, "{kind:?} GET");
            assert_eq!(
                fails_over(&stream(kind), &Method::POST),
                post,
                "{kind:?} POST"
            );
        }
        assert!(!fails_over(
            &NodeHttpError::Status(StatusCode::FORBIDDEN),
            &Method::GET
        ));
        assert!(!fails_over(
            &NodeHttpError::Decode("x".into()),
            &Method::GET
        ));
    }

    #[tokio::test]
    async fn a_failover_target_that_fails_the_policy_is_never_dialed() {
        let live = http_server(200).await;
        let id = sample_peer_id();
        let strict = OutboundPolicy::new(false);
        // A loopback literal and a name that resolves to loopback.
        let name_url = live.uri().replace("127.0.0.1", "localhost");
        for base in [live.uri(), name_url] {
            let table = table_with_connectivity(&id, &base, Some(Connectivity::Relayed));
            let (handle, seen) =
                scripted_worker(table.clone(), Script::Fail(StreamErrorKind::Connect));
            let err = NodeClient::new()
                .with_stream(handle.clone())
                .with_policy(strict)
                .get(format!("{}/nodes/status", p2p_base_url(&id)))
                .send()
                .await
                .err()
                .unwrap();
            // The original stream error comes back.
            assert!(matches!(
                err,
                NodeHttpError::Stream {
                    kind: StreamErrorKind::Connect,
                    ..
                }
            ));
            assert_eq!(seen.lock().unwrap().len(), 1);
            // A demoted stream is not swapped away from onto a refused target either.
            let _ = NodeClient::new()
                .with_stream(handle)
                .with_policy(strict)
                .get(format!("{}/nodes/status", p2p_base_url(&id)))
                .send()
                .await;
            assert_eq!(seen.lock().unwrap().len(), 2);
            assert_eq!(http_hits(&live).await, 0, "{base}");
        }
        // Link-local is refused even where private ranges are allowed.
        let table =
            table_with_connectivity(&id, "http://169.254.169.254", Some(Connectivity::Relayed));
        let (handle, _) = scripted_worker(table, Script::Fail(StreamErrorKind::Connect));
        let err = lax_client(handle)
            .get(format!("{}/nodes/status", p2p_base_url(&id)))
            .send()
            .await
            .err()
            .unwrap();
        assert!(matches!(err, NodeHttpError::Stream { .. }));
    }

    #[tokio::test]
    async fn a_failover_to_http_does_not_follow_redirects() {
        let elsewhere = http_server(200).await;
        let live = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(302).insert_header("location", elsewhere.uri()))
            .mount(&live)
            .await;
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Relayed));
        let (handle, _) = scripted_worker(table, Script::Fail(StreamErrorKind::Connect));
        let res = lax_client(handle)
            .get(format!("{}/nodes/status", p2p_base_url(&id)))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FOUND);
        assert_eq!(http_hits(&elsewhere).await, 0);
    }

    #[tokio::test]
    async fn a_slow_answer_does_not_demote_the_transport() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(3)))
            .mount(&server)
            .await;
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, &server.uri(), Some(Connectivity::Direct));
        let (handle, seen) = fake_worker(table.clone(), ok_answer());
        // The read times out on an established connection and falls over, undemoted.
        let res = lax_client(handle)
            .get(format!("{}/nodes/status", server.uri()))
            .timeout(Duration::from_millis(200))
            .send()
            .await
            .unwrap();
        assert!(res.via_stream());
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(!table.transport_stats().is_demoted(&id, Transport::Http));
    }

    #[tokio::test]
    async fn outcomes_are_not_recorded_under_an_id_more_than_one_entry_holds() {
        let base = dead_base();
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, &base, Some(Connectivity::Direct));
        let mut twin = info("http://twin.test", Some(&id), Some(Connectivity::Direct));
        twin.identity_bound = true;
        table.upsert(twin);
        let (handle, seen) = fake_worker(table.clone(), ok_answer());
        let _ = lax_client(handle)
            .get(format!("{base}/nodes/status"))
            .send()
            .await;
        // No stream failover for an id several entries claim, and nothing is recorded for it.
        assert!(seen.lock().unwrap().is_empty());
        assert!(table
            .transport_stats()
            .outcome(&id, Transport::Http)
            .is_none());
        assert!(table
            .transport_stats()
            .outcome(&id, Transport::Stream)
            .is_none());
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
    async fn a_demoted_http_write_route_moves_to_the_stream_and_only_allowed_paths_fail_over() {
        // HTTP is demoted and the path is on the stream allowlist: the write goes over the stream.
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
        assert!(res.via_stream());
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(http_hits(&live).await, 0);

        // A path the stream allowlist does not have gets the connect error, not a stream refusal.
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

    #[test]
    fn the_failover_url_comes_only_from_the_one_bound_entry_with_a_usable_url() {
        let id = sample_peer_id();
        let table = PeerTable::new();
        assert_eq!(table.http_url_for_libp2p_peer(&id), None);
        // Unbound and unusable entries never name a URL.
        let mut unbound = info("http://u.test", Some(&id), None);
        unbound.identity_bound = false;
        table.upsert(unbound);
        assert_eq!(table.http_url_for_libp2p_peer(&id), None);
        for unusable in ["p2p://x", "not a url", ""] {
            let single = PeerTable::new();
            single.upsert(info(unusable, Some(&id), None));
            assert_eq!(single.http_url_for_libp2p_peer(&id), None, "{unusable}");
        }
        // A bound entry with a usable URL does, until a second bound entry claims the id.
        let table = PeerTable::new();
        table.upsert(info("http://a.test", Some(&id), None));
        assert_eq!(
            table.http_url_for_libp2p_peer(&id).as_deref(),
            Some("http://a.test")
        );
        assert!(table.libp2p_id_is_unambiguous(&id));
        table.upsert(info("http://evil.test", Some(&id), None));
        assert_eq!(table.http_url_for_libp2p_peer(&id), None);
        assert!(!table.libp2p_id_is_unambiguous(&id));
    }

    #[tokio::test]
    async fn a_stream_first_request_for_a_duplicated_id_has_no_http_failover() {
        let live = http_server(200).await;
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Relayed));
        table.upsert(info(
            "http://evil.test",
            Some(&id),
            Some(Connectivity::Relayed),
        ));
        let (handle, _) = scripted_worker(table.clone(), Script::Fail(StreamErrorKind::Connect));
        let err = lax_client(handle)
            .get(format!("{}/nodes/status", p2p_base_url(&id)))
            .send()
            .await
            .err()
            .unwrap();
        assert!(err.is_connect());
        assert_eq!(http_hits(&live).await, 0);
        assert!(table
            .transport_stats()
            .outcome(&id, Transport::Stream)
            .is_none());
    }

    #[test]
    fn the_peer_table_url_ignores_outcomes_for_an_ambiguous_id() {
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, "http://a.test", Some(Connectivity::Direct));
        table.transport_stats().record_failure(id, Transport::Http);
        assert_eq!(table.transport_url("http://a.test"), p2p_base_url(&id));
        table.upsert(info(
            "http://evil.test",
            Some(&id),
            Some(Connectivity::Direct),
        ));
        assert_eq!(table.transport_url("http://a.test"), "http://a.test");
        assert_eq!(table.transport_url("http://evil.test"), "http://evil.test");
    }

    #[tokio::test]
    async fn a_refused_check_is_remembered_and_a_passed_one_is_reused() {
        let live = http_server(200).await;
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Relayed));
        let stats = table.transport_stats().clone();
        let now = std::time::Instant::now();
        // A remembered refusal blocks failover although the policy would now pass.
        stats.store_vet(id, &live.uri(), true, None, now);
        let (handle, seen) = scripted_worker(table.clone(), Script::Fail(StreamErrorKind::Connect));
        let err = lax_client(handle.clone())
            .get(format!("{}/nodes/status", p2p_base_url(&id)))
            .send()
            .await
            .err()
            .unwrap();
        assert!(err.is_connect());
        assert_eq!(http_hits(&live).await, 0);
        // Once it expires the target is checked again and the answer is stored.
        let later = now + crate::transport_stats::VET_TTL;
        assert!(stats.cached_vet(id, &live.uri(), true, later).is_none());
        stats.store_vet(
            id,
            &live.uri(),
            true,
            None,
            now - crate::transport_stats::VET_TTL,
        );
        let res = lax_client(handle)
            .get(format!("{}/nodes/status", p2p_base_url(&id)))
            .send()
            .await
            .unwrap();
        assert!(!res.via_stream());
        assert!(matches!(
            stats.cached_vet(id, &live.uri(), true, std::time::Instant::now()),
            Some(Some(_))
        ));
        // The demoted stream was skipped the second time.
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_local_failure_fails_a_read_over_without_demoting_the_stream() {
        let live = http_server(200).await;
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Relayed));
        let (handle, _) = scripted_worker(table.clone(), Script::Fail(StreamErrorKind::Local));
        let res = lax_client(handle.clone())
            .get(format!("{}/nodes/status", p2p_base_url(&id)))
            .send()
            .await
            .unwrap();
        assert!(!res.via_stream());
        assert!(!table.transport_stats().is_demoted(&id, Transport::Stream));
        // A write stays on the stream and surfaces the local error.
        let err = lax_client(handle)
            .post(format!("{}/nodes/announce", p2p_base_url(&id)))
            .send()
            .await
            .err()
            .unwrap();
        assert!(err.is_local());
        assert_eq!(http_hits(&live).await, 1);
    }

    #[tokio::test]
    async fn each_caller_fails_over_under_its_own_timeout() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(800)))
            .mount(&server)
            .await;
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, &server.uri(), Some(Connectivity::Relayed));
        let (handle, _) = scripted_worker(table, Script::Fail(StreamErrorKind::Connect));
        let url = format!("{}/nodes/status", p2p_base_url(&id));
        // A short caller first, so a cached client would hand its timeout to the long one.
        let short = lax_client(handle.clone()).with_timeout(Duration::from_millis(200));
        let err = short.get(&url).send().await.err().unwrap();
        assert!(err.is_timeout());
        let long = lax_client(handle).with_timeout(Duration::from_secs(5));
        let res = long.get(&url).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_policy_check_that_hangs_is_a_refusal_after_the_vet_timeout() {
        let check = async {
            std::future::pending::<()>().await;
            Ok::<CheckedTarget, ()>(unreachable_target())
        };
        let outcome =
            tokio::time::timeout(VET_TIMEOUT + Duration::from_secs(2), bounded_check(check)).await;
        assert!(matches!(outcome, Ok(None)));
    }

    fn unreachable_target() -> CheckedTarget {
        CheckedTarget {
            base_url: "http://x.invalid".into(),
            pinned_host: None,
            addr: "127.0.0.1:1".parse().unwrap(),
            policy: OutboundPolicy::new(true),
        }
    }

    #[tokio::test]
    async fn a_passed_check_is_reused_within_the_ttl_and_never_across_policies() {
        let first = http_server(200).await;
        let second = http_server(200).await;
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, &first.uri(), Some(Connectivity::Relayed));
        let stats = table.transport_stats().clone();
        let now = std::time::Instant::now();
        // A stored pass for another address is what a reuse would dial, not a fresh resolution.
        let cached = CheckedTarget {
            base_url: format!("http://cached.invalid:{}", second.address().port()),
            pinned_host: Some("cached.invalid".into()),
            addr: *second.address(),
            policy: OutboundPolicy::new(true),
        };
        stats.store_vet(id, &first.uri(), true, Some(cached), now);
        let (handle, _) = scripted_worker(table, Script::Fail(StreamErrorKind::Connect));
        let url = format!("{}/nodes/status", p2p_base_url(&id));
        let res = lax_client(handle.clone()).get(&url).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(http_hits(&second).await, 1);
        assert_eq!(http_hits(&first).await, 0);
        // A strict caller does not share the permissive caller's pass: it is checked itself and
        // refused (loopback).
        let strict = NodeClient::new()
            .with_stream(handle)
            .with_policy(OutboundPolicy::new(false));
        assert!(strict.get(&url).send().await.is_err());
        assert_eq!(http_hits(&second).await, 1);
        assert_eq!(http_hits(&first).await, 0);
    }

    /// A stream handle whose worker answers 200 and records every request it was handed.
    fn capturing_worker(
        peers: PeerTable,
    ) -> (
        StreamHandle,
        std::sync::Arc<std::sync::Mutex<Vec<NodeHttpRequest>>>,
    ) {
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
                    log.lock().unwrap().push(request);
                    let _ = respond_to.send(Ok(ok_answer()));
                }
            }
        });
        let handle = StreamHandle {
            commands: tx,
            peers: Some(peers),
            settings: NodeHttpSettings::default(),
        };
        (handle, seen)
    }

    fn signer() -> NodeSigner {
        NodeSigner::new(&identity::Keypair::generate_ed25519(), "net").unwrap()
    }

    fn credential(headers: &wiremock::http::HeaderMap) -> String {
        headers
            .get(NODE_REQUEST_HEADER)
            .expect("credential header")
            .to_str()
            .unwrap()
            .to_string()
    }

    fn verify_for(
        header: &str,
        path: &str,
        body: &[u8],
        recipient: &str,
    ) -> Result<avalon_protocol::node_request::NodeRequestAuth, ()> {
        let target = avalon_protocol::node_request::NodeRequestTarget {
            method: "POST",
            path,
            body,
            network_id: "net",
        };
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        avalon_protocol::node_request::verify_node_request_header(
            header,
            &target,
            &[recipient],
            now,
            60,
        )
        .map_err(|_| ())
    }

    #[tokio::test]
    async fn every_http_attempt_at_a_write_route_is_signed_afresh() {
        let live = http_server(200).await;
        let id = sample_peer_id();
        let table = table_with_connectivity(&id, &live.uri(), Some(Connectivity::Direct));
        let (handle, seen) = capturing_worker(table);
        let me = signer();
        let client = NodeClient::new()
            .with_stream(handle)
            .with_signer(me.clone());
        for path in CREDENTIAL_PATHS {
            for _ in 0..2 {
                client
                    .post(format!("{}{path}?x=1", live.uri()))
                    .body("payload")
                    .send()
                    .await
                    .unwrap();
            }
        }
        let received = live.received_requests().await.unwrap();
        assert_eq!(received.len(), 6);
        let mut nonces = std::collections::HashSet::new();
        for r in &received {
            let header = credential(&r.headers);
            // The peer table knows the libp2p id, so that is the recipient.
            let auth = verify_for(&header, r.url.path(), b"payload", &id.to_string()).unwrap();
            assert_eq!(auth.peer_id, me.peer_id());
            assert!(nonces.insert(auth.nonce), "a nonce was reused");
            // The signature binds the body and the route.
            assert!(verify_for(&header, r.url.path(), b"other", &id.to_string()).is_err());
            assert!(verify_for(&header, "/nodes/announce", b"payload", &id.to_string()).is_err());
        }
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_peer_without_a_known_id_is_addressed_by_its_normalized_url() {
        let live = http_server(200).await;
        let me = signer();
        let client = NodeClient::new().with_signer(me);
        client
            .post(format!("{}/mirror/notify", live.uri()))
            .body("{}")
            .send()
            .await
            .unwrap();
        let received = live.received_requests().await.unwrap();
        let header = credential(&received[0].headers);
        let origin = normalized_origin(&live.uri()).unwrap();
        assert!(verify_for(&header, "/mirror/notify", b"{}", &origin).is_ok());
        assert!(verify_for(&header, "/mirror/notify", b"{}", "http://other.test").is_err());
    }

    #[tokio::test]
    async fn no_other_request_carries_a_credential_and_a_callers_own_is_never_sent() {
        let live = http_server(200).await;
        let client = NodeClient::new().with_signer(signer());
        client
            .post(format!("{}/nodes/announce", live.uri()))
            .send()
            .await
            .unwrap();
        client
            .get(format!("{}/nodes/status", live.uri()))
            .send()
            .await
            .unwrap();
        client
            .post(format!("{}/nodes/relay/extra", live.uri()))
            .send()
            .await
            .unwrap();
        client
            .post(format!("{}/x/nodes/relay", live.uri()))
            .header(NODE_REQUEST_HEADER, "v1; forged")
            .send()
            .await
            .unwrap();
        for r in live.received_requests().await.unwrap() {
            assert!(!r.headers.contains_key(NODE_REQUEST_HEADER), "{}", r.url);
        }

        // A caller-supplied header is replaced, never duplicated, on a signed route.
        client
            .post(format!("{}/nodes/relay", live.uri()))
            .header(NODE_REQUEST_HEADER, "v1; forged")
            .send()
            .await
            .unwrap();
        let received = live.received_requests().await.unwrap();
        let last = received.last().unwrap();
        assert_eq!(last.headers.get_all(NODE_REQUEST_HEADER).iter().count(), 1);
        assert!(!credential(&last.headers).contains("forged"));
    }

    #[tokio::test]
    async fn without_a_signer_the_write_routes_are_sent_unsigned() {
        let live = http_server(200).await;
        NodeClient::new()
            .post(format!("{}/nodes/relay", live.uri()))
            .send()
            .await
            .unwrap();
        let received = live.received_requests().await.unwrap();
        assert!(!received[0].headers.contains_key(NODE_REQUEST_HEADER));
    }

    #[tokio::test]
    async fn a_stream_attempt_at_a_write_route_never_carries_a_credential() {
        let id = sample_peer_id();
        let me = signer();
        // Stream first: a p2p:// URL.
        let table = table_with_connectivity(&id, &p2p_base_url(&id), None);
        let (handle, seen) = capturing_worker(table);
        let client = NodeClient::new()
            .with_stream(handle)
            .with_signer(me.clone());
        for path in CREDENTIAL_PATHS {
            client
                .post(format!("{}{path}", p2p_base_url(&id)))
                .header(NODE_REQUEST_HEADER, "v1; forged")
                .body("x")
                .send()
                .await
                .unwrap();
        }
        // HTTP first, URL down: the retry over the stream is unsigned too.
        let base = dead_base();
        let table = table_with_connectivity(&id, &base, Some(Connectivity::Direct));
        let (handle2, seen2) = capturing_worker(table);
        let res = NodeClient::new()
            .with_stream(handle2)
            .with_signer(me)
            .post(format!("{base}/nodes/relay"))
            .body("x")
            .send()
            .await
            .unwrap();
        assert!(res.via_stream());
        for log in [seen, seen2] {
            let log = log.lock().unwrap();
            assert!(!log.is_empty());
            for req in log.iter() {
                assert!(req
                    .headers
                    .iter()
                    .all(|(k, _)| !k.eq_ignore_ascii_case(NODE_REQUEST_HEADER)));
            }
        }
    }

    #[tokio::test]
    async fn a_redirect_to_a_signed_post_is_returned_and_never_followed() {
        for status in [307u16, 308] {
            let elsewhere = http_server(200).await;
            let live = MockServer::start().await;
            Mock::given(wiremock::matchers::any())
                .respond_with(
                    ResponseTemplate::new(status)
                        .insert_header("location", format!("{}/nodes/relay", elsewhere.uri())),
                )
                .mount(&live)
                .await;
            for client in [NodeClient::new(), NodeClient::peer()] {
                let res = client
                    .with_signer(signer())
                    .post(format!("{}/nodes/relay", live.uri()))
                    .body("payload")
                    .send()
                    .await
                    .unwrap();
                assert_eq!(res.status().as_u16(), status);
            }
            assert_eq!(live.received_requests().await.unwrap().len(), 2);
            assert_eq!(http_hits(&elsewhere).await, 0, "{status} was followed");
        }
    }

    #[test]
    fn the_redirect_notice_names_target_and_location_without_secrets() {
        let n = redirect_notice(
            "http://user:pw@node.test:8080/nodes/relay?token=s",
            Some("https://node.test/nodes/relay?x=1"),
        );
        assert!(n.contains("http://node.test:8080/nodes/relay "), "{n}");
        assert!(n.contains("https://node.test/nodes/relay;"), "{n}");
        assert!(!n.contains("pw") && !n.contains("token") && !n.contains("x=1"));
        assert!(redirect_notice("http://a.test/x", None).contains("<none>"));
    }

    /// A listener that counts accepted connections and answers each with `reply`.
    async fn counting_listener(
        reply: &'static str,
    ) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(reply.as_bytes()).await;
            }
        });
        (port, hits)
    }

    const OK_REPLY: &str = "HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";

    fn hits_of(hits: &std::sync::atomic::AtomicUsize) -> usize {
        hits.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[tokio::test]
    async fn a_guarded_client_refuses_forbidden_literals_without_connecting() {
        let (port, hits) = counting_listener(OK_REPLY).await;
        let strict = NodeClient::guarded_with(OutboundPolicy::new(false));
        let lax = NodeClient::guarded_with(OutboundPolicy::new(true));
        for url in [
            format!("http://127.0.0.1:{port}/x"),
            format!("http://[::1]:{port}/x"),
            "http://169.254.169.254/latest/meta-data".to_string(),
            "http://10.0.0.1/x".to_string(),
        ] {
            assert!(strict.post(&url).send().await.is_err(), "{url}");
        }
        for url in ["http://169.254.169.254/x", "http://[fe80::1]/x"] {
            assert!(lax.post(url).send().await.is_err(), "{url}");
        }
        assert_eq!(hits_of(&hits), 0);
    }

    #[tokio::test]
    async fn a_guarded_client_dials_an_allowed_address() {
        let (port, hits) = counting_listener(OK_REPLY).await;
        let lax = NodeClient::guarded_with(OutboundPolicy::new(true));
        let res = lax
            .post(format!("http://127.0.0.1:{port}/x"))
            .send()
            .await
            .unwrap();
        assert!(res.status().is_success());
        assert_eq!(hits_of(&hits), 1);
    }

    #[tokio::test]
    async fn a_guarded_client_checks_what_a_hostname_resolves_to_at_connect_time() {
        let (port, hits) = counting_listener(OK_REPLY).await;
        let url = format!("http://localhost:{port}/x");
        let strict = NodeClient::guarded_with(OutboundPolicy::new(false));
        assert!(strict.post(&url).send().await.is_err());
        assert_eq!(hits_of(&hits), 0);
        let lax = NodeClient::guarded_with(OutboundPolicy::new(true));
        assert!(lax.post(&url).send().await.unwrap().status().is_success());
        assert_eq!(hits_of(&hits), 1);
    }

    #[tokio::test]
    async fn a_guarded_client_does_not_follow_redirects() {
        let (target_port, target_hits) = counting_listener(OK_REPLY).await;
        let redirect: &'static str = Box::leak(
            format!(
                "HTTP/1.1 302 Found\r\nlocation: http://127.0.0.1:{target_port}/x\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            )
            .into_boxed_str(),
        );
        let (port, hits) = counting_listener(redirect).await;
        let lax = NodeClient::guarded_with(OutboundPolicy::new(true));
        let res = lax
            .post(format!("http://127.0.0.1:{port}/x"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FOUND);
        assert_eq!(hits_of(&hits), 1);
        assert_eq!(hits_of(&target_hits), 0);
    }
}
