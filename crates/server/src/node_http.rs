//! Node-to-node HTTP that also works for peers with no reachable URL.
//!
//! [`NodeClient`] is a drop-in for the slice of `reqwest` the node-to-node call sites use.
//! `http(s)://` URLs go through the wrapped `reqwest::Client` untouched (including the pinned
//! clients `crate::outbound_policy` builds); `p2p://<libp2p peer id>` URLs are carried as one
//! request-response exchange on a libp2p stream, so relays and hole punching apply to them.
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
        }
    }

    /// The base URL to use for `peer`: `p2p://<id>` when it has a libp2p id and either only
    /// dials out or is reached through a relay or hole punch, or its `base_url` is not a usable http(s) URL;
    /// otherwise its `base_url`.
    pub fn url_for(peer: &PeerInfo) -> String {
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
        if prefer_stream || !url_usable {
            p2p_base_url(&id)
        } else {
            peer.base_url.clone()
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

    fn http_builder(&self) -> reqwest::RequestBuilder {
        let mut rb = self.client.http.request(self.method.clone(), &self.url);
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
        if self.url.starts_with("p2p://") {
            let (peer, path) = split_p2p_url(&self.url)
                .ok_or_else(|| NodeHttpError::Invalid("bad p2p url".into()))?;
            return self.send_stream(peer, path).await;
        }
        let fallback = self.client.fallback_peer(&self.url);
        match self.http_builder().send().await {
            Ok(r) => Ok(NodeResponse::from_http(r)),
            Err(e) => {
                // A duplicate write after a timeout could be applied twice, so only reads retry then.
                let retry = e.is_connect() || (e.is_timeout() && self.method == Method::GET);
                let Some(peer) = fallback.filter(|_| retry) else {
                    return Err(e.into());
                };
                let path = http_path_and_query(&self.url).ok_or(e)?;
                tracing::debug!(%peer, "node-http: HTTP failed, retrying over a libp2p stream");
                self.send_stream(peer, path).await
            }
        }
    }
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

    /// A stream handle whose worker answers every request with `answer`, recording the paths.
    fn fake_worker(
        peers: PeerTable,
        answer: NodeHttpResponse,
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
                    let _ = respond_to.send(Ok(answer.clone()));
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
}
