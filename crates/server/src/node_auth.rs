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
//! Over HTTP the signature is verified before the body is read (the header carries the body
//! hash, the signature covers it). Standing is free until per-route scope checks exist, so a
//! stranger cannot make the node read a body but any node with standing can; that read is
//! bounded by the route cap, a short read timeout, a per-signer in-flight cap, the signer's
//! request budget and a global limit on concurrent reads. A source IP that keeps sending
//! expensive failures (bad signature, wrong body hash, oversize body) is only answered 429 for
//! further failures: a valid credential is never refused for the address it came from, since
//! behind a proxy that is not in `AVALON_TRUSTED_PROXIES` every client shares one address.
//!
//! The replay cache stores a 128-bit truncated SHA-256 of (signer id, nonce) per live nonce,
//! so a collision can only reject a fresh nonce as a replay, with probability about 2^-128
//! per pair. It never evicts a live nonce: when full it refuses (503), a liveness gap only.
//! Budget windows are fixed minutes, allowing a 2x burst across a boundary.
//!
//! The replay cache lives in process memory: a restart, replicas sharing one identity key or a
//! forward clock step re-open up to [`MAX_SKEW_SECS`] for a captured request. Peer URLs with a
//! path prefix are not signed, so they are refused. `PeerTable::standing` scans the peer table
//! (at most `AVALON_NODE_MAX_KNOWN_PEERS` entries) once per request.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use avalon_protocol::node_request::{
    parse_node_request_header, verify_node_request_body, verify_node_request_head,
    NodeRequestError, NodeRequestHead, NODE_REQUEST_HEADER,
};
use axum::body::Body;
use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use libp2p::identity::{ed25519, PublicKey};
use libp2p::PeerId;
use sha2::{Digest, Sha256};

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

/// Hard ceiling on live nonces held in total; see the hosting docs for the measured memory.
pub const MAX_REPLAY_ENTRIES: usize = 1_048_576;

/// The largest number of standing keys: the peer table is their only source.
fn standing_bound() -> usize {
    crate::peer_admission::max_known_peers_from_env()
}

/// Live nonces the cache is sized for: every standing key at its full per-signer share,
/// capped at [`MAX_REPLAY_ENTRIES`]. Past the ceiling a full cache refuses with 503.
fn replay_capacity(standing_keys: usize, rate: u32) -> usize {
    standing_keys
        .saturating_mul(per_signer_cap(rate))
        .clamp(1, MAX_REPLAY_ENTRIES)
}

/// Nonces one key can hold: with fixed one-minute windows, the 121 s retention can span four
/// windows of its budget, plus slack.
fn per_signer_cap(rate: u32) -> usize {
    4 * rate as usize + 16
}

/// Expensive credential failures one source may cause per minute before further failures are
/// answered 429 without being counted, unless `AVALON_NODE_AUTH_FAILED_PER_MINUTE_PER_IP` says
/// otherwise. A valid credential is never refused for it.
pub const DEFAULT_FAILED_PER_MINUTE_PER_IP: u32 = 30;

/// Body reads that may run at once, unless `AVALON_NODE_AUTH_MAX_CONCURRENT_BODIES` says
/// otherwise.
pub const DEFAULT_MAX_CONCURRENT_BODIES: usize = 64;

/// Body reads one signer may have in flight at once.
pub const DEFAULT_MAX_INFLIGHT_PER_SIGNER: usize = 3;

/// Longest a body read may take, for the largest route body.
pub const DEFAULT_BODY_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Most sources tracked for failures; the table evicts when full.
const MAX_FAILURE_KEYS: usize = 16_384;

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
    /// The replay cache holds only live nonces and has no room; the request is refused unseen.
    ReplayCacheFull,
    BodyTooLarge,
    /// The body did not arrive within the read timeout.
    BodyReadTimeout,
    /// The source address caused too many expensive failures this minute.
    SourceThrottled,
    /// Too many body reads are already running.
    Busy,
    /// This signer already has its share of body reads in flight.
    SignerBusy,
}

impl NodeAuthError {
    /// Whether a refusal is worth a warning: only those that need a standing peer, since a
    /// stranger can cause every other one for free.
    fn warns(&self) -> bool {
        matches!(
            self,
            Self::Replay
                | Self::RateLimited
                | Self::ReplayCacheFull
                | Self::Busy
                | Self::SignerBusy
                | Self::BodyReadTimeout
        )
    }

    /// Whether the refusal counts against the source address: only failures that cost a
    /// signature verification or a body read. Cheaper rejections cost no more than a hash.
    fn is_expensive_failure(&self) -> bool {
        matches!(
            self,
            Self::Invalid(NodeRequestError::BadSignature | NodeRequestError::BodyMismatch)
                | Self::BodyTooLarge
        )
    }

    /// Whether the refusal counts against the signer's own failure budget: only a holder of
    /// the key can cause these.
    fn is_signer_failure(&self) -> bool {
        matches!(
            self,
            Self::Invalid(NodeRequestError::BodyMismatch)
                | Self::BodyTooLarge
                | Self::BodyReadTimeout
        )
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::Missing | Self::Invalid(_) | Self::PeerMismatch | Self::Replay => {
                StatusCode::UNAUTHORIZED
            }
            Self::NoStanding => StatusCode::FORBIDDEN,
            Self::RateLimited | Self::SourceThrottled | Self::SignerBusy => {
                StatusCode::TOO_MANY_REQUESTS
            }
            Self::BodyTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::BodyReadTimeout => StatusCode::REQUEST_TIMEOUT,
            Self::ReplayCacheFull | Self::Busy => StatusCode::SERVICE_UNAVAILABLE,
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
            Self::Invalid(NodeRequestError::BodyMismatch) => "node_auth_body_hash",
            Self::PeerMismatch => "node_auth_peer_mismatch",
            Self::Replay => "node_auth_replay",
            Self::NoStanding => "node_auth_no_standing",
            Self::RateLimited => "node_auth_rate_limited",
            Self::BodyTooLarge => "node_auth_body_too_large",
            Self::BodyReadTimeout => "node_auth_body_timeout",
            Self::ReplayCacheFull => "node_auth_replay_cache_full",
            Self::SourceThrottled => "node_auth_source_throttled",
            Self::Busy => "node_auth_busy",
            Self::SignerBusy => "node_auth_signer_busy",
        }
    }

    fn into_response(self) -> Response {
        let message = match self {
            Self::NoStanding => "this node has no standing with the receiver",
            Self::RateLimited | Self::SignerBusy => "too many requests from this node, retry later",
            Self::BodyTooLarge => "request body too large for this route",
            Self::BodyReadTimeout => "request body not received in time",
            Self::ReplayCacheFull | Self::Busy => "this node is busy, retry later",
            Self::SourceThrottled => "too many failed credentials from this address",
            _ => "node credential missing or invalid",
        };
        let mut error = TopologyError::new(self.status(), self.code(), message);
        if matches!(
            self,
            Self::RateLimited
                | Self::ReplayCacheFull
                | Self::Busy
                | Self::SourceThrottled
                | Self::SignerBusy
        ) {
            error.retry_after_secs = Some(30);
        }
        error.into_response()
    }
}

/// First 16 bytes of SHA-256 over the signer's id bytes and the nonce.
fn nonce_key(signer: &PeerId, nonce: &[u8; 16]) -> u128 {
    let digest = Sha256::new()
        .chain_update(signer.to_bytes())
        .chain_update(nonce)
        .finalize();
    u128::from_be_bytes(digest[..16].try_into().expect("a digest is 32 bytes"))
}

/// A short per-signer tag for the share counters: collisions only make two signers share one
/// counter, which can refuse early and never accepts more.
fn signer_tag(signer: &PeerId) -> u64 {
    let digest = Sha256::new()
        .chain_update(b"signer")
        .chain_update(signer.to_bytes())
        .finalize();
    u64::from_be_bytes(digest[..8].try_into().expect("a digest is 32 bytes"))
}

/// Nonces seen from each signer, kept for [`REPLAY_RETENTION_SECS`].
struct ReplayCache {
    seen: HashSet<u128>,
    /// Insertion order, which is expiry order: `(expires_at, key, signer tag)`.
    order: VecDeque<(i64, u128, u64)>,
    per_signer: HashMap<u64, u32>,
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
        let Some((_, key, tag)) = self.order.pop_front() else {
            return;
        };
        self.seen.remove(&key);
        if let Some(n) = self.per_signer.get_mut(&tag) {
            *n -= 1;
            if *n == 0 {
                self.per_signer.remove(&tag);
            }
        }
    }

    /// Drops expired entries, and gives the memory back once the cache has emptied.
    fn expire(&mut self, now: i64) {
        while self.order.front().is_some_and(|(exp, ..)| *exp <= now) {
            self.forget_front();
        }
        if self.order.is_empty() && self.seen.capacity() > 4096 {
            self.seen = HashSet::new();
            self.order = VecDeque::new();
            self.per_signer = HashMap::new();
        }
    }

    /// Whether `(signer, nonce)` is held and unexpired; drops expired entries first.
    fn is_live(&mut self, signer: &PeerId, nonce: &[u8; 16], now: i64) -> bool {
        self.expire(now);
        self.seen.contains(&nonce_key(signer, nonce))
    }

    /// Whether `(signer, nonce)` could be recorded now: refuses a repeat, a signer already
    /// holding its share, and a cache full of live nonces. Expired entries go first; a live
    /// nonce is never evicted, so a refusal is a liveness gap and never a replay window.
    fn admit(&mut self, signer: &PeerId, nonce: &[u8; 16], now: i64) -> Result<(), NodeAuthError> {
        if self.is_live(signer, nonce, now) {
            return Err(NodeAuthError::Replay);
        }
        let held = self
            .per_signer
            .get(&signer_tag(signer))
            .copied()
            .unwrap_or(0);
        if held as usize >= self.max_per_signer {
            return Err(NodeAuthError::RateLimited);
        }
        if self.seen.len() >= self.max_total {
            return Err(NodeAuthError::ReplayCacheFull);
        }
        Ok(())
    }

    /// Records a nonce [`Self::admit`] allowed.
    fn record(&mut self, signer: &PeerId, nonce: &[u8; 16], now: i64) {
        let (key, tag) = (nonce_key(signer, nonce), signer_tag(signer));
        self.seen.insert(key);
        self.order
            .push_back((now.saturating_add(REPLAY_RETENTION_SECS), key, tag));
        *self.per_signer.entry(tag).or_insert(0) += 1;
    }
}

/// Events per key in the current minute.
struct Budget<K> {
    windows: HashMap<K, (i64, u32)>,
    per_minute: u32,
    max_keys: usize,
    /// A full table drops an entry to admit a new key instead of refusing it.
    evict_when_full: bool,
}

impl<K: Hash + Eq + Copy> Budget<K> {
    fn new(per_minute: u32, max_keys: usize, evict_when_full: bool) -> Self {
        Self {
            windows: HashMap::new(),
            per_minute,
            max_keys,
            evict_when_full,
        }
    }

    /// Whether `key` has used up this minute, without counting anything.
    fn exhausted(&self, key: &K, now: i64) -> bool {
        self.windows
            .get(key)
            .is_some_and(|(m, n)| *m == now.div_euclid(60) && *n >= self.per_minute)
    }

    fn charge(&mut self, key: K, now: i64) -> bool {
        let minute = now.div_euclid(60);
        if self.windows.len() >= self.max_keys && !self.windows.contains_key(&key) {
            self.windows.retain(|_, (m, _)| *m == minute);
            if self.windows.len() >= self.max_keys {
                if !self.evict_when_full {
                    return false;
                }
                if let Some(victim) = self.windows.keys().next().copied() {
                    self.windows.remove(&victim);
                }
            }
        }
        let (window, count) = self.windows.entry(key).or_insert((minute, 0));
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

/// The key a source address is counted under: an IPv6 address by its /64, since one host
/// commonly controls a whole /64.
fn source_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => {
            let masked = u128::from(v6) & (u128::MAX << 64);
            IpAddr::V6(Ipv6Addr::from(masked))
        }
    }
}

struct Inner {
    peers: PeerTable,
    network_id: String,
    recipients: Vec<String>,
    replay: Mutex<ReplayCache>,
    budget: Mutex<Budget<PeerId>>,
    /// Expensive failures per source address.
    failures: Mutex<Budget<IpAddr>>,
    /// Body failures per signer.
    signer_failures: Mutex<Budget<PeerId>>,
    /// Body reads in flight per signer.
    inflight: Mutex<HashMap<PeerId, usize>>,
    body_stage: Arc<tokio::sync::Semaphore>,
    limits: StageLimits,
    proxies: crate::trusted_proxies::TrustedProxies,
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

/// Limits on the work done before a credential is fully verified and its body read.
#[derive(Debug, Clone, Copy)]
pub struct StageLimits {
    pub failed_per_minute_per_ip: u32,
    pub max_concurrent_bodies: usize,
    pub max_inflight_per_signer: usize,
    pub body_read_timeout: Duration,
}

impl Default for StageLimits {
    fn default() -> Self {
        Self {
            failed_per_minute_per_ip: DEFAULT_FAILED_PER_MINUTE_PER_IP,
            max_concurrent_bodies: DEFAULT_MAX_CONCURRENT_BODIES,
            max_inflight_per_signer: DEFAULT_MAX_INFLIGHT_PER_SIGNER,
            body_read_timeout: DEFAULT_BODY_READ_TIMEOUT,
        }
    }
}

impl StageLimits {
    fn from_env() -> Self {
        let get = |name: &str| {
            std::env::var(name)
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|n| *n > 0)
        };
        let d = Self::default();
        Self {
            failed_per_minute_per_ip: get("AVALON_NODE_AUTH_FAILED_PER_MINUTE_PER_IP")
                .map_or(d.failed_per_minute_per_ip, |n| {
                    n.min(u32::MAX as usize) as u32
                }),
            max_concurrent_bodies: get("AVALON_NODE_AUTH_MAX_CONCURRENT_BODIES")
                .unwrap_or(d.max_concurrent_bodies),
            ..d
        }
    }
}

/// A body read in flight: holds one global stage and one of the signer's in-flight slots.
pub struct ReadGuard {
    _stage: tokio::sync::OwnedSemaphorePermit,
    _slot: InflightSlot,
}

struct InflightSlot {
    inner: Arc<Inner>,
    signer: PeerId,
}

impl Drop for InflightSlot {
    fn drop(&mut self) {
        let mut map = self
            .inner
            .inflight
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(n) = map.get_mut(&self.signer) {
            *n -= 1;
            if *n == 0 {
                map.remove(&self.signer);
            }
        }
    }
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
            replay_capacity(standing_bound(), rate_per_minute_from_env()),
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
        Self::with_stage_limits(
            peers,
            network_id,
            own_peer_id,
            own_base_url,
            rate_per_minute,
            max_replay_entries,
            StageLimits::from_env(),
        )
    }

    /// [`Self::with_limits`] with explicit limits for the pre-body stages too.
    pub fn with_stage_limits(
        peers: PeerTable,
        network_id: &str,
        own_peer_id: Option<&str>,
        own_base_url: Option<&str>,
        rate_per_minute: u32,
        max_replay_entries: usize,
        stage: StageLimits,
    ) -> Self {
        let mut recipients: Vec<String> = own_peer_id.map(str::to_string).into_iter().collect();
        recipients.extend(own_base_url.and_then(normalized_origin));
        let per_signer = per_signer_cap(rate_per_minute);
        Self {
            inner: Arc::new(Inner {
                peers,
                network_id: network_id.to_string(),
                recipients,
                replay: Mutex::new(ReplayCache::new(max_replay_entries, per_signer)),
                budget: Mutex::new(Budget::new(rate_per_minute, MAX_BUDGET_KEYS, false)),
                failures: Mutex::new(Budget::new(
                    stage.failed_per_minute_per_ip,
                    MAX_FAILURE_KEYS,
                    true,
                )),
                signer_failures: Mutex::new(Budget::new(
                    stage.failed_per_minute_per_ip,
                    MAX_BUDGET_KEYS,
                    true,
                )),
                inflight: Mutex::new(HashMap::new()),
                body_stage: Arc::new(tokio::sync::Semaphore::new(stage.max_concurrent_bodies)),
                limits: stage,
                proxies: crate::trusted_proxies::TrustedProxies::from_env(),
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

    /// Sources currently tracked for failures.
    pub fn failure_keys(&self) -> usize {
        self.inner
            .failures
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .windows
            .len()
    }

    /// Fills the replay cache with `entries` synthetic live nonces spread over signers, for
    /// measuring its memory. Not for use outside tests and benchmarks.
    #[doc(hidden)]
    pub fn fill_replay_cache_for_measurement(&self, entries: usize, now: i64) {
        let mut cache = self.inner.replay.lock().unwrap_or_else(|p| p.into_inner());
        let per_signer = cache.max_per_signer.max(1);
        let mut signer = PeerId::random();
        for i in 0..entries {
            if i % per_signer == 0 {
                signer = PeerId::random();
            }
            let mut nonce = [0u8; 16];
            nonce[..8].copy_from_slice(&(i as u64).to_be_bytes());
            if cache.admit(&signer, &nonce, now).is_ok() {
                cache.record(&signer, &nonce, now);
            }
        }
    }

    /// Whether `ip` has used up its expensive-failure budget this minute.
    pub fn source_throttled(&self, ip: IpAddr, now: i64) -> bool {
        self.inner
            .failures
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .exhausted(&source_key(ip), now)
    }

    /// Counts an expensive failure against `ip` (its /64 for IPv6).
    pub fn note_source_failure(&self, ip: IpAddr, now: i64) {
        self.inner
            .failures
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .charge(source_key(ip), now);
    }

    /// Counts a body failure against `signer`.
    pub fn note_signer_failure(&self, signer: PeerId, now: i64) {
        self.inner
            .signer_failures
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .charge(signer, now);
    }

    /// Everything about an HTTP credential that can be checked without the body: header form,
    /// clock window, key-to-peer-id binding, standing, then the signature over the header's own
    /// body hash, then whether the nonce was already used. Nothing is recorded.
    pub fn verify_head(
        &self,
        headers: &HeaderMap,
        method: &str,
        path: &str,
        now: i64,
    ) -> Result<VerifiedHead, NodeAuthError> {
        let inner = &*self.inner;
        let auth =
            parse_node_request_header(header_text(headers)?).map_err(NodeAuthError::Invalid)?;
        if auth.timestamp < now.saturating_sub(MAX_SKEW_SECS) {
            return Err(NodeAuthError::Invalid(NodeRequestError::Stale));
        }
        if auth.timestamp > now.saturating_add(MAX_SKEW_SECS) {
            return Err(NodeAuthError::Invalid(NodeRequestError::Future));
        }
        let signer = peer_id_of_key(&auth.public_key)
            .filter(|id| id.to_string() == auth.peer_id)
            .ok_or(NodeAuthError::PeerMismatch)?;
        if !inner.peers.standing(&signer) {
            return Err(NodeAuthError::NoStanding);
        }
        let head = NodeRequestHead {
            method,
            path,
            network_id: &inner.network_id,
        };
        let recipients: Vec<&str> = inner.recipients.iter().map(String::as_str).collect();
        verify_node_request_head(&auth, &head, &recipients, now, MAX_SKEW_SECS)
            .map_err(NodeAuthError::Invalid)?;
        let replayed = inner
            .replay
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_live(&signer, &auth.nonce, now);
        if replayed {
            return Err(NodeAuthError::Replay);
        }
        Ok(VerifiedHead { signer, auth })
    }

    /// Admits the body read of a verified credential: the signer's failure budget and
    /// in-flight share, a global read stage, and one request of the signer's budget, which
    /// repeated slow or wrong reads therefore spend. The guard frees both on drop, however the
    /// read ends.
    pub fn begin_read(&self, head: &VerifiedHead, now: i64) -> Result<ReadGuard, NodeAuthError> {
        let inner = &self.inner;
        let signer = head.signer;
        if inner
            .signer_failures
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .exhausted(&signer, now)
        {
            return Err(NodeAuthError::RateLimited);
        }
        {
            let mut map = inner.inflight.lock().unwrap_or_else(|p| p.into_inner());
            let n = map.entry(signer).or_insert(0);
            if *n >= inner.limits.max_inflight_per_signer {
                return Err(NodeAuthError::SignerBusy);
            }
            *n += 1;
        }
        let slot = InflightSlot {
            inner: inner.clone(),
            signer,
        };
        let stage = inner
            .body_stage
            .clone()
            .try_acquire_owned()
            .map_err(|_| NodeAuthError::Busy)?;
        let charged = inner
            .budget
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .charge(signer, now);
        if !charged {
            return Err(NodeAuthError::RateLimited);
        }
        Ok(ReadGuard {
            _stage: stage,
            _slot: slot,
        })
    }

    /// Completes an HTTP credential once the body is read: the body must hash to the signed
    /// hash, then the nonce is checked and recorded under one lock (the budget was spent when
    /// the read began). A request with the wrong body records no nonce.
    pub fn finish(
        &self,
        head: VerifiedHead,
        body: &[u8],
        now: i64,
    ) -> Result<AuthenticatedNode, NodeAuthError> {
        verify_node_request_body(&head.auth, body).map_err(NodeAuthError::Invalid)?;
        let mut replay = self.inner.replay.lock().unwrap_or_else(|p| p.into_inner());
        replay.admit(&head.signer, &head.auth.nonce, now)?;
        replay.record(&head.signer, &head.auth.nonce, now);
        Ok(AuthenticatedNode(head.signer))
    }

    /// Admits a stream peer, whose handshake is its credential: standing, then one request
    /// of its budget. No body stage is taken, since a stream body is already in memory.
    pub fn authenticate_stream(
        &self,
        peer: PeerId,
        now: i64,
    ) -> Result<AuthenticatedNode, NodeAuthError> {
        let inner = &*self.inner;
        if !inner.peers.standing(&peer) {
            return Err(NodeAuthError::NoStanding);
        }
        let charged = inner
            .budget
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .charge(peer, now);
        if !charged {
            return Err(NodeAuthError::RateLimited);
        }
        Ok(AuthenticatedNode(peer))
    }

    /// Resolves the caller in one step, with the body already in hand. `remote` is the
    /// noise-authenticated peer of a stream request; when set, the header is not read at all.
    pub fn authenticate(
        &self,
        remote: Option<PeerId>,
        headers: &HeaderMap,
        method: &str,
        path: &str,
        body: &[u8],
        now: i64,
    ) -> Result<AuthenticatedNode, NodeAuthError> {
        match remote {
            Some(peer) => self.authenticate_stream(peer, now),
            None => {
                let head = self.verify_head(headers, method, path, now)?;
                let _read = self.begin_read(&head, now)?;
                self.finish(head, body, now)
            }
        }
    }
}

/// A credential whose signature, peer id and standing are verified and whose body is not yet
/// read.
pub struct VerifiedHead {
    signer: PeerId,
    auth: avalon_protocol::node_request::NodeRequestAuth,
}

/// State of [`require_node_auth`] for one route.
#[derive(Clone)]
pub struct NodeAuthRoute {
    auth: NodeAuth,
    max_body_bytes: usize,
}

/// One warning per reason per interval, with the count held back.
fn refusal_log() -> &'static crate::log_throttle::LogThrottle {
    static LOG: std::sync::OnceLock<crate::log_throttle::LogThrottle> = std::sync::OnceLock::new();
    LOG.get_or_init(|| crate::log_throttle::LogThrottle::new(Duration::from_secs(30)))
}

/// Middleware for the node-to-node write routes: refuses the request unless it resolves to an
/// [`AuthenticatedNode`], which it adds as a request extension. Over HTTP the signature is
/// verified before the body is read; the body read is bounded by the route's limit, a read
/// timeout, and the signer's in-flight share.
pub async fn require_node_auth(
    State(route): State<NodeAuthRoute>,
    request: Request,
    next: Next,
) -> Response {
    let (mut parts, body) = request.into_parts();
    let remote = parts.extensions.get::<RemotePeer>().map(|r| r.0);
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let auth = &route.auth;
    // Without connection info (never the case when served) all requests share one source.
    let source = parts
        .extensions
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ConnectInfo(addr)| auth.inner.proxies.client_ip(addr.ip(), &parts.headers))
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    let throttled = remote.is_none() && auth.source_throttled(source, now);
    let refuse = |e: NodeAuthError, signer: Option<PeerId>| {
        let (reason, path) = (e.code(), parts.uri.path());
        let transport = if remote.is_some() { "stream" } else { "http" };
        // Only refusals that need a standing peer can warn; a stranger can cause the rest.
        if e.warns() {
            if let Some(held_back) = refusal_log().permit(reason, std::time::Instant::now()) {
                tracing::warn!(
                    reason,
                    path,
                    transport,
                    held_back,
                    "node-auth: request refused"
                );
            }
        } else {
            tracing::debug!(reason, path, transport, "node-auth: request refused");
        }
        if remote.is_none() {
            if e.is_expensive_failure() && !throttled {
                auth.note_source_failure(source, now);
            }
            if let (true, Some(signer)) = (e.is_signer_failure(), signer) {
                auth.note_signer_failure(signer, now);
            }
        }
        // A throttled source's failures are answered cheaply; its valid requests never get here.
        if throttled && !matches!(e, NodeAuthError::Missing) {
            return NodeAuthError::SourceThrottled.into_response();
        }
        e.into_response()
    };

    let node = if let Some(peer) = remote {
        let bytes = match axum::body::to_bytes(body, route.max_body_bytes).await {
            Ok(bytes) => bytes,
            Err(_) => return refuse(NodeAuthError::BodyTooLarge, None),
        };
        match auth.authenticate_stream(peer, now) {
            Ok(node) => (node, bytes),
            Err(e) => return refuse(e, None),
        }
    } else {
        if !parts.headers.contains_key(NODE_REQUEST_HEADER) {
            return refuse(NodeAuthError::Missing, None);
        }
        let head =
            match auth.verify_head(&parts.headers, parts.method.as_str(), parts.uri.path(), now) {
                Ok(head) => head,
                Err(e) => return refuse(e, None),
            };
        let signer = head.signer;
        let _read = match auth.begin_read(&head, now) {
            Ok(guard) => guard,
            Err(e) => return refuse(e, Some(signer)),
        };
        let read = tokio::time::timeout(
            auth.inner.limits.body_read_timeout,
            axum::body::to_bytes(body, route.max_body_bytes),
        )
        .await;
        let bytes = match read {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(_)) => return refuse(NodeAuthError::BodyTooLarge, Some(signer)),
            Err(_) => return refuse(NodeAuthError::BodyReadTimeout, Some(signer)),
        };
        match auth.finish(head, &bytes, now) {
            Ok(node) => (node, bytes),
            Err(e) => return refuse(e, Some(signer)),
        }
    };
    let (node, bytes) = node;
    parts.extensions.insert(node);
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::PeerInfo;
    use avalon_protocol::node_request::NodeRequestTarget;
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
        assert_eq!(
            f.http(&ok(NOW, 6), b"{ }", NOW),
            invalid(NodeRequestError::BodyMismatch)
        );
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
        // Only the known key ever got a budget; no stranger did.
        assert_eq!(f.auth.replay_len(), 0);
        assert!(f.auth.budget_keys() <= 1);
    }

    #[test]
    fn the_nonce_cache_is_bounded_globally_and_per_signer() {
        // Global cap: a cache of live nonces refuses new ones and never evicts.
        let f = fixture_with(600, 5);
        let me = f.me.to_string();
        let nodes: Vec<Node> = (0..4).map(|_| node()).collect();
        for n in &nodes {
            f.grant(n);
        }
        let send = |i: u8, now: i64| {
            let n = &nodes[i as usize % 4];
            f.http(&header_at(n, b"{}", &me, now, [i; 16]), b"{}", now)
        };
        for i in 0..5u8 {
            assert!(send(i, NOW).is_ok());
        }
        for i in 5..40u8 {
            assert_eq!(send(i, NOW), Err(NodeAuthError::ReplayCacheFull));
            assert_eq!(f.auth.replay_len(), 5);
        }
        // Every live nonce still counts as used: nothing was evicted to make room.
        for i in 0..5u8 {
            assert_eq!(send(i, NOW + 1), Err(NodeAuthError::Replay), "nonce {i}");
        }
        // Expired entries are pruned before a request is refused.
        assert!(send(50, NOW + REPLAY_RETENTION_SECS).is_ok());
        assert_eq!(f.auth.replay_len(), 1);

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
        let mut b = Budget::new(5, 3, false);
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
            (ReplayCacheFull, 503, "node_auth_replay_cache_full"),
            (Busy, 503, "node_auth_busy"),
            (SourceThrottled, 429, "node_auth_source_throttled"),
            (
                Invalid(NodeRequestError::BodyMismatch),
                401,
                "node_auth_body_hash",
            ),
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
    fn the_head_check_refuses_everything_but_a_valid_standing_signature_without_a_body() {
        let f = fixture();
        let n = node();
        f.grant(&n);
        let me = f.me.to_string();
        let check = |h: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(NODE_REQUEST_HEADER, h.parse().unwrap());
            f.auth.verify_head(&headers, "POST", PATH, NOW).map(|_| ())
        };
        let good = header_at(&n, b"{}", &me, NOW, [1; 16]);
        assert_eq!(check(&good), Ok(()));
        let mal = |r: Result<(), NodeAuthError>| {
            assert!(matches!(
                r,
                Err(NodeAuthError::Invalid(NodeRequestError::Malformed(_)))
            ))
        };
        mal(check("garbage"));
        mal(check(&format!("{good}{}", "a".repeat(600))));
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
        let h = good.replacen(&n.id.to_string(), &stranger.id.to_string(), 1);
        assert_eq!(check(&h), Err(NodeAuthError::PeerMismatch));
        // Everything public about a standing peer (id and key) with a garbage signature.
        let sig_at = good.find("sig=").unwrap() + 4;
        let mut forged = good.clone();
        forged.replace_range(
            sig_at..sig_at + 1,
            if &good[sig_at..sig_at + 1] == "0" {
                "1"
            } else {
                "0"
            },
        );
        assert_eq!(
            check(&forged),
            Err(NodeAuthError::Invalid(NodeRequestError::BadSignature))
        );
        let elsewhere = header_at(&n, b"{}", "https://elsewhere.test", NOW, [5; 16]);
        assert_eq!(
            check(&elsewhere),
            Err(NodeAuthError::Invalid(NodeRequestError::BadSignature))
        );
        assert_eq!(f.auth.replay_len(), 0);
        // A header already used is refused at this stage too.
        assert!(f.http(&good, b"{}", NOW).is_ok());
        assert_eq!(check(&good), Err(NodeAuthError::Replay));
    }

    #[test]
    fn a_valid_signature_over_the_wrong_body_records_no_nonce_but_spends_the_signers_budget() {
        let f = fixture_with(2, MAX_REPLAY_ENTRIES);
        let n = node();
        f.grant(&n);
        let me = f.me.to_string();
        let h = header_at(&n, b"{}", &me, NOW, [1; 16]);
        let wrong = Err(NodeAuthError::Invalid(NodeRequestError::BodyMismatch));
        assert_eq!(f.http(&h, b"{ }", NOW), wrong);
        assert_eq!(f.http(&h, b"{ }", NOW), wrong);
        // Repeated wrong or slow reads cost budget, so they run out.
        assert_eq!(f.http(&h, b"{ }", NOW), Err(NodeAuthError::RateLimited));
        assert_eq!(f.auth.replay_len(), 0);
        // The next minute, the same header with its real body still works.
        assert!(f.http(&h, b"{}", NOW + 60).is_ok());
    }

    /// A body that records whether anything polled it.
    fn watched_body(polled: Arc<std::sync::atomic::AtomicBool>) -> Body {
        Body::from_stream(futures_util::stream::poll_fn(move |_| {
            polled.store(true, std::sync::atomic::Ordering::SeqCst);
            std::task::Poll::Ready(None::<Result<axum::body::Bytes, std::io::Error>>)
        }))
    }

    fn watched_request(h: &str, polled: &Arc<std::sync::atomic::AtomicBool>, ip: &str) -> Request {
        let mut req = Request::builder()
            .method("POST")
            .uri(PATH)
            .header(NODE_REQUEST_HEADER, h)
            .body(watched_body(polled.clone()))
            .unwrap();
        let addr: std::net::SocketAddr = format!("{ip}:1000").parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(addr));
        req
    }

    fn was_polled(polled: &Arc<std::sync::atomic::AtomicBool>) -> bool {
        polled.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[tokio::test]
    async fn only_a_valid_signature_from_a_standing_key_gets_its_body_read() {
        let f = fixture();
        let n = node();
        f.grant(&n);
        let me = f.me.to_string();
        let app = app(&f, 64);
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let good = header_at(&n, b"", &me, now, [9; 16]);
        let sig_at = good.find("sig=").unwrap() + 4;
        let mut forged = good.clone();
        forged.replace_range(
            sig_at..sig_at + 1,
            if &good[sig_at..sig_at + 1] == "0" {
                "1"
            } else {
                "0"
            },
        );
        let refused = [
            header_at(&n, b"", &me, now - 1000, [1; 16]),
            header_at(&node(), b"", &me, now, [2; 16]),
            header_at(&n, b"", "https://elsewhere.test", now, [3; 16]),
            forged,
            "garbage".to_string(),
        ];
        for h in &refused {
            let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let status = call(&app, watched_request(h, &polled, "198.51.100.1"))
                .await
                .0;
            assert!(
                status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN,
                "{h}"
            );
            assert!(!was_polled(&polled), "the body was read for {h}");
        }
        // A valid credential gets its body read, once; a replay of it is refused unread.
        let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert_eq!(
            call(&app, watched_request(&good, &polled, "198.51.100.1"))
                .await
                .0,
            StatusCode::OK
        );
        assert!(was_polled(&polled));
        let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let replay = call(&app, watched_request(&good, &polled, "198.51.100.1"))
            .await
            .0;
        assert_eq!(replay, StatusCode::UNAUTHORIZED);
        assert!(!was_polled(&polled));
        // A valid signature over another body is read, then refused.
        let other = header_at(&n, b"x", &me, now, [10; 16]);
        let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (status, body) = call(&app, watched_request(&other, &polled, "198.51.100.1")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.contains("node_auth_body_hash"), "{body}");
        assert!(was_polled(&polled));
    }

    fn fixture_stage(stage: StageLimits) -> Fixture {
        let peers = PeerTable::new();
        let me = PeerId::random();
        let auth = NodeAuth::with_stage_limits(
            peers.clone(),
            NET,
            Some(&me.to_string()),
            None,
            600,
            MAX_REPLAY_ENTRIES,
            stage,
        );
        Fixture { auth, peers, me }
    }

    fn forged_signature(h: &str) -> String {
        let at = h.find("sig=").unwrap() + 4;
        let mut forged = h.to_string();
        forged.replace_range(at..at + 1, if &h[at..at + 1] == "0" { "1" } else { "0" });
        forged
    }

    #[tokio::test]
    async fn a_throttled_source_still_gets_valid_credentials_through_and_failures_stop_counting() {
        let f = fixture_stage(StageLimits {
            failed_per_minute_per_ip: 3,
            ..StageLimits::default()
        });
        let n = node();
        f.grant(&n);
        let me = f.me.to_string();
        let app = app(&f, 64);
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let bad = |i: u8| forged_signature(&header_at(&n, b"", &me, now, [i; 16]));
        let ip = "198.51.100.7";
        for i in 0..3 {
            let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (status, _) = call(&app, watched_request(&bad(i), &polled, ip)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
        assert!(f.auth.source_throttled(ip.parse().unwrap(), now));
        // A failure from the throttled source is answered cheaply and read no further.
        let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (status, body) = call(&app, watched_request(&bad(10), &polled, ip)).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert!(body.contains("node_auth_source_throttled"), "{body}");
        assert!(!was_polled(&polled));
        // Counting stopped at the limit instead of growing.
        let counted = f
            .auth
            .inner
            .failures
            .lock()
            .unwrap()
            .windows
            .values()
            .next()
            .unwrap()
            .1;
        assert_eq!(counted, 3);
        // A valid credential from the same address is not refused for it.
        let good = header_at(&n, b"", &me, now, [50; 16]);
        let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert_eq!(
            call(&app, watched_request(&good, &polled, ip)).await.0,
            StatusCode::OK
        );
        assert!(was_polled(&polled));
    }

    #[tokio::test]
    async fn only_failures_that_cost_a_verification_or_a_read_count_against_a_source() {
        let f = fixture_stage(StageLimits {
            failed_per_minute_per_ip: 2,
            ..StageLimits::default()
        });
        let n = node();
        f.grant(&n);
        let me = f.me.to_string();
        let app = app(&f, 64);
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let ip = "198.51.100.9";
        let cheap = [
            header_at(&n, b"", &me, now - 1000, [1; 16]),
            header_at(&node(), b"", &me, now, [2; 16]),
            "garbage".to_string(),
            header_at(&n, b"", &me, now + 1000, [3; 16]),
        ];
        for h in cheap.iter().cycle().take(20) {
            let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let status = call(&app, watched_request(h, &polled, ip)).await.0;
            assert!(status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN);
        }
        assert!(!f.auth.source_throttled(ip.parse().unwrap(), now));
        assert_eq!(f.auth.failure_keys(), 0);
        // A wrong body is a body read, and it counts; replays do not.
        let good = header_at(&n, b"", &me, now, [4; 16]);
        let wrong = header_at(&n, b"y", &me, now, [5; 16]);
        let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert_eq!(
            call(&app, watched_request(&good, &polled, ip)).await.0,
            StatusCode::OK
        );
        for _ in 0..5 {
            let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let replay = call(&app, watched_request(&good, &polled, ip)).await.0;
            assert_eq!(replay, StatusCode::UNAUTHORIZED);
        }
        assert_eq!(f.auth.failure_keys(), 0);
        let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert_eq!(
            call(&app, watched_request(&wrong, &polled, ip)).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(f.auth.failure_keys(), 1);
    }

    fn take_stage(f: &Fixture) -> Result<tokio::sync::OwnedSemaphorePermit, ()> {
        f.auth
            .inner
            .body_stage
            .clone()
            .try_acquire_owned()
            .map_err(|_| ())
    }

    #[tokio::test]
    async fn body_stages_are_bounded_and_refused_with_503_when_busy() {
        let f = fixture_stage(StageLimits {
            max_concurrent_bodies: 2,
            ..StageLimits::default()
        });
        let n = node();
        f.grant(&n);
        let me = f.me.to_string();
        let app = app(&f, 64);
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let held = [take_stage(&f).unwrap(), take_stage(&f).unwrap()];
        assert!(take_stage(&f).is_err());
        let good = header_at(&n, b"", &me, now, [1; 16]);
        let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (status, body) = call(&app, watched_request(&good, &polled, "198.51.100.1")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.contains("node_auth_busy"), "{body}");
        assert!(!was_polled(&polled));
        // The refused read left nothing behind: no nonce, no in-flight slot.
        assert_eq!(f.auth.replay_len(), 0);
        assert!(f.auth.inner.inflight.lock().unwrap().is_empty());
        drop(held);
        let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert_eq!(
            call(&app, watched_request(&good, &polled, "198.51.100.1"))
                .await
                .0,
            StatusCode::OK
        );
        // The permit is returned once the request is done.
        assert!(take_stage(&f).is_ok());
    }

    #[tokio::test]
    async fn a_stream_request_takes_no_body_stage_and_a_stranger_cannot_contend_for_one() {
        let f = fixture_stage(StageLimits {
            max_concurrent_bodies: 1,
            ..StageLimits::default()
        });
        let (n, stranger) = (node(), node());
        f.grant(&n);
        let me = f.me.to_string();
        let app = app(&f, 64);
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let _held = take_stage(&f).unwrap();
        // Every stage is taken: a stream request still passes.
        assert_eq!(
            call(&app, post_req(None, b"", Some(n.id))).await.0,
            StatusCode::OK
        );
        // A stranger's refusal is the standing refusal, not the busy one.
        let h = header_at(&stranger, b"", &me, now, [1; 16]);
        let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (status, body) = call(&app, watched_request(&h, &polled, "198.51.100.1")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }

    #[test]
    fn one_signer_has_a_small_share_of_body_reads_and_gets_it_back() {
        let f = fixture_stage(StageLimits {
            max_inflight_per_signer: 2,
            ..StageLimits::default()
        });
        let (n, m) = (node(), node());
        f.grant(&n);
        f.grant(&m);
        let me = f.me.to_string();
        let head = |who: &Node, i: u8| {
            let mut headers = HeaderMap::new();
            headers.insert(
                NODE_REQUEST_HEADER,
                header_at(who, b"", &me, NOW, [i; 16]).parse().unwrap(),
            );
            f.auth.verify_head(&headers, "POST", PATH, NOW).unwrap()
        };
        let a = f.auth.begin_read(&head(&n, 1), NOW).unwrap();
        let _b = f.auth.begin_read(&head(&n, 2), NOW).unwrap();
        assert_eq!(
            f.auth.begin_read(&head(&n, 3), NOW).err(),
            Some(NodeAuthError::SignerBusy)
        );
        // Another signer is unaffected, and a finished read frees its slot.
        assert!(f.auth.begin_read(&head(&m, 4), NOW).is_ok());
        drop(a);
        assert!(f.auth.begin_read(&head(&n, 5), NOW).is_ok());
        // Only live reads are tracked.
        assert_eq!(f.auth.inner.inflight.lock().unwrap().len(), 1);
    }

    /// A body that never produces a byte.
    fn stalled_body() -> Body {
        Body::from_stream(futures_util::stream::pending::<
            Result<axum::body::Bytes, std::io::Error>,
        >())
    }

    fn stalled_request(h: &str, ip: &str) -> Request {
        let mut req = Request::builder()
            .method("POST")
            .uri(PATH)
            .header(NODE_REQUEST_HEADER, h)
            .body(stalled_body())
            .unwrap();
        let addr: std::net::SocketAddr = format!("{ip}:1000").parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(addr));
        req
    }

    #[tokio::test]
    async fn a_slow_body_times_out_frees_its_slots_and_counts_against_the_signer() {
        let f = fixture_stage(StageLimits {
            body_read_timeout: Duration::from_millis(100),
            failed_per_minute_per_ip: 2,
            ..StageLimits::default()
        });
        let n = node();
        f.grant(&n);
        let me = f.me.to_string();
        let app = app(&f, 64);
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        for i in 0..2u8 {
            let h = header_at(&n, b"", &me, now, [i; 16]);
            let (status, body) = call(&app, stalled_request(&h, "198.51.100.1")).await;
            assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
            assert!(body.contains("node_auth_body_timeout"), "{body}");
            // Everything the read held is back, and no nonce was spent.
            assert!(take_stage(&f).is_ok());
            assert!(f.auth.inner.inflight.lock().unwrap().is_empty());
            assert_eq!(f.auth.replay_len(), 0);
        }
        // Two timeouts used up the signer's failure budget: it is refused before reading.
        let h = header_at(&n, b"", &me, now, [9; 16]);
        let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (status, _) = call(&app, watched_request(&h, &polled, "198.51.100.2")).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert!(!was_polled(&polled));
        // The slow reads were not charged to the address.
        assert!(!f
            .auth
            .source_throttled("198.51.100.1".parse().unwrap(), now));
    }

    #[tokio::test]
    async fn a_dropped_connection_frees_the_stage_and_the_signers_slot() {
        let f = fixture();
        let n = node();
        f.grant(&n);
        let me = f.me.to_string();
        let app = app(&f, 64);
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let h = header_at(&n, b"", &me, now, [1; 16]);
        let task = tokio::spawn({
            let app = app.clone();
            async move { app.oneshot(stalled_request(&h, "198.51.100.1")).await }
        });
        for _ in 0..100 {
            if !f.auth.inner.inflight.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(f.auth.inner.inflight.lock().unwrap().len(), 1);
        assert!(f.auth.inner.body_stage.available_permits() < DEFAULT_MAX_CONCURRENT_BODIES);
        task.abort();
        let _ = task.await;
        assert!(f.auth.inner.inflight.lock().unwrap().is_empty());
        assert_eq!(
            f.auth.inner.body_stage.available_permits(),
            DEFAULT_MAX_CONCURRENT_BODIES
        );
    }

    #[test]
    fn each_refusal_is_classified_for_logging_and_failure_accounting() {
        use NodeAuthError::*;
        use NodeRequestError as E;
        // (error, warns, counts against the source, counts against the signer)
        let table = [
            (Missing, false, false, false),
            (Invalid(E::Malformed("x")), false, false, false),
            (Invalid(E::InvalidRequest("x")), false, false, false),
            (Invalid(E::InvalidKey), false, false, false),
            (Invalid(E::Stale), false, false, false),
            (Invalid(E::Future), false, false, false),
            (Invalid(E::BadSignature), false, true, false),
            (Invalid(E::BodyMismatch), false, true, true),
            (PeerMismatch, false, false, false),
            (NoStanding, false, false, false),
            (SourceThrottled, false, false, false),
            (BodyTooLarge, false, true, true),
            (BodyReadTimeout, true, false, true),
            (Replay, true, false, false),
            (RateLimited, true, false, false),
            (ReplayCacheFull, true, false, false),
            (Busy, true, false, false),
            (SignerBusy, true, false, false),
        ];
        for (e, warns, source, signer) in table {
            assert_eq!(
                (e.warns(), e.is_expensive_failure(), e.is_signer_failure()),
                (warns, source, signer),
                "{e:?}"
            );
        }
    }

    #[test]
    fn ipv6_sources_are_counted_per_64_and_the_failure_table_evicts_instead_of_failing_open() {
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:bbbb::9".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(source_key(a), source_key(b));
        assert_ne!(source_key(a), source_key(c));
        assert_eq!(
            source_key("203.0.113.9".parse().unwrap()),
            "203.0.113.9".parse::<IpAddr>().unwrap()
        );

        let mut t: Budget<u32> = Budget::new(2, 3, true);
        for k in 0..3 {
            assert!(t.charge(k, NOW));
        }
        // Full of current windows: a new key is counted by evicting another.
        assert!(t.charge(99, NOW));
        assert!(t.windows.len() <= 3);
        assert!(!t.exhausted(&99, NOW));
        assert!(t.charge(99, NOW));
        assert!(t.exhausted(&99, NOW));
    }

    #[test]
    fn without_connection_info_every_request_shares_one_source() {
        let f = fixture_stage(StageLimits {
            failed_per_minute_per_ip: 2,
            ..StageLimits::default()
        });
        let unspecified = IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED);
        f.auth.note_source_failure(unspecified, NOW);
        f.auth.note_source_failure(unspecified, NOW);
        assert!(f.auth.source_throttled(unspecified, NOW));
        assert!(!f
            .auth
            .source_throttled("198.51.100.1".parse().unwrap(), NOW));
    }

    #[test]
    fn the_replay_cache_gives_its_memory_back_once_it_has_emptied() {
        let mut c = ReplayCache::new(100_000, 100_000);
        let s = PeerId::random();
        for i in 0..20_000u32 {
            let mut nonce = [0u8; 16];
            nonce[..4].copy_from_slice(&i.to_be_bytes());
            assert!(c.admit(&s, &nonce, NOW).is_ok());
            c.record(&s, &nonce, NOW);
        }
        assert!(c.seen.capacity() > 4096);
        c.expire(NOW + REPLAY_RETENTION_SECS);
        assert_eq!(c.seen.len(), 0);
        assert!(c.seen.capacity() <= 4096);
        assert!(c.order.capacity() <= 4096);
    }

    #[test]
    fn the_replay_cache_is_sized_from_the_standing_bound_up_to_the_ceiling() {
        assert_eq!(replay_capacity(1, 3000), per_signer_cap(3000));
        assert_eq!(replay_capacity(10, 3000), 10 * per_signer_cap(3000));
        assert_eq!(replay_capacity(2000, 3000), MAX_REPLAY_ENTRIES);
        assert_eq!(replay_capacity(0, 3000), 1);
        assert_eq!(standing_bound(), 2000);
    }

    #[test]
    fn the_default_budget_matches_the_per_ip_limit_these_routes_were_under() {
        assert_eq!(DEFAULT_RATE_PER_MINUTE, 3000);
        assert_eq!(
            DEFAULT_RATE_PER_MINUTE as u64,
            crate::DEFAULT_RATE_LIMIT_PER_MINUTE
        );
    }

    #[test]
    fn a_victims_live_nonce_survives_a_flood_from_another_key_at_the_cap() {
        let f = fixture_with(600, 8);
        let (victim, flooder) = (node(), node());
        f.grant(&victim);
        f.grant(&flooder);
        let me = f.me.to_string();
        let captured = header_at(&victim, b"{}", &me, NOW, [200; 16]);
        assert!(f.http(&captured, b"{}", NOW).is_ok());
        for i in 0..100u8 {
            let h = header_at(&flooder, b"{}", &me, NOW, [i; 16]);
            let _ = f.http(&h, b"{}", NOW);
        }
        assert_eq!(f.auth.replay_len(), 8);
        assert_eq!(f.http(&captured, b"{}", NOW), Err(NodeAuthError::Replay));
    }

    #[test]
    fn a_replay_costs_no_budget_and_a_nonce_is_accepted_once_under_concurrency() {
        let f = fixture_with(3, MAX_REPLAY_ENTRIES);
        let n = node();
        f.grant(&n);
        let me = f.me.to_string();
        let first = header_at(&n, b"{}", &me, NOW, [1; 16]);
        assert!(f.http(&first, b"{}", NOW).is_ok());
        for _ in 0..20 {
            assert_eq!(f.http(&first, b"{}", NOW), Err(NodeAuthError::Replay));
        }
        // Two tokens of the three are left.
        for i in 2..4u8 {
            assert!(f
                .http(&header_at(&n, b"{}", &me, NOW, [i; 16]), b"{}", NOW)
                .is_ok());
        }
        let over = header_at(&n, b"{}", &me, NOW, [9; 16]);
        assert_eq!(f.http(&over, b"{}", NOW), Err(NodeAuthError::RateLimited));

        let g = fixture();
        g.grant(&n);
        let same = header_at(&n, b"{}", &g.me.to_string(), NOW, [7; 16]);
        let accepted = std::thread::scope(|scope| {
            let jobs: Vec<_> = (0..16)
                .map(|_| scope.spawn(|| g.http(&same, b"{}", NOW).is_ok()))
                .collect();
            jobs.into_iter()
                .map(|j| j.join().unwrap())
                .filter(|ok| *ok)
                .count()
        });
        assert_eq!(accepted, 1);
    }

    #[test]
    fn the_replay_capacity_follows_the_peer_cap_and_the_rate_up_to_a_ceiling() {
        assert_eq!(per_signer_cap(3000), 12_016);
        assert_eq!(replay_capacity(50, 3000), 50 * 12_016);
        assert_eq!(replay_capacity(10_000, 3000), MAX_REPLAY_ENTRIES);
        assert_eq!(replay_capacity(0, 3000), 1);
    }

    #[test]
    fn only_refusals_that_need_a_standing_peer_warn() {
        use NodeAuthError::*;
        for e in [Replay, RateLimited, ReplayCacheFull] {
            assert!(e.warns(), "{e:?}");
        }
        for e in [
            Missing,
            NoStanding,
            PeerMismatch,
            BodyTooLarge,
            Invalid(NodeRequestError::BadSignature),
            Invalid(NodeRequestError::Stale),
        ] {
            assert!(!e.warns(), "{e:?}");
        }
    }

    #[test]
    fn one_signer_cannot_hold_more_than_its_share_of_the_cache() {
        let mut c = ReplayCache::new(100, 3);
        let (a, b) = (PeerId::random(), PeerId::random());
        for i in 0..3u8 {
            assert_eq!(c.admit(&a, &[i; 16], NOW), Ok(()));
            c.record(&a, &[i; 16], NOW);
        }
        assert_eq!(c.admit(&a, &[9; 16], NOW), Err(NodeAuthError::RateLimited));
        assert_eq!(c.admit(&b, &[9; 16], NOW), Ok(()));
    }
}
