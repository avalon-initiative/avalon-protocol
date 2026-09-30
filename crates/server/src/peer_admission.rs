//! Admission rules for peer table and shard registry entries that arrive over
//! the network: URL shape, outbound address policy, per-source and
//! per-exchange limits, and a bounded reachability check for new announcers.

use std::net::IpAddr;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use libp2p::multiaddr::Protocol;
use libp2p::{Multiaddr, PeerId};

use crate::outbound_policy::{CheckedTarget, OutboundPolicy, PolicyError};
use crate::topology_limits::{InFlightGate, IpRateLimiter, TopologyError};

/// Largest `/nodes/status` body read during a reachability check.
const MAX_STATUS_BODY: usize = 256 * 1024;
/// Timeout for one reachability check.
const REACHABILITY_TIMEOUT: Duration = Duration::from_secs(3);
const SOURCE_WINDOW: Duration = Duration::from_secs(60);
const MAX_SHARD_ID_LEN: usize = 128;

#[derive(Debug, Clone)]
pub struct AdmissionConfig {
    /// `AVALON_NODE_MAX_KNOWN_PEERS`: size cap of the peer table.
    pub max_known_peers: usize,
    /// `AVALON_NODE_MAX_URL_LENGTH`: longest accepted base URL.
    pub max_url_len: usize,
    /// `AVALON_ANNOUNCE_NEW_PEERS_PER_SOURCE_PER_MINUTE`: distinct new base
    /// URLs one source address may introduce per minute.
    pub new_urls_per_source: usize,
    /// `AVALON_GOSSIP_MAX_NEW_PEERS_PER_EXCHANGE`: new peer table entries
    /// accepted from one gossip response.
    pub max_new_per_exchange: usize,
    /// `AVALON_GOSSIP_MAX_NEW_SHARD_URLS_PER_EXCHANGE`: unseen shard URLs
    /// validated per exchange.
    pub max_new_shard_urls_per_exchange: usize,
    /// `AVALON_ANNOUNCE_VERIFY_REACHABILITY`: fetch `/nodes/status` of a new
    /// announcer before admitting it.
    pub verify_reachability: bool,
    /// `AVALON_ANNOUNCE_MAX_CONCURRENT_CHECKS`: concurrent admission checks.
    pub max_concurrent_checks: usize,
}

impl Default for AdmissionConfig {
    fn default() -> Self {
        Self {
            max_known_peers: 2000,
            max_url_len: 256,
            new_urls_per_source: 10,
            max_new_per_exchange: 20,
            max_new_shard_urls_per_exchange: 256,
            verify_reachability: true,
            max_concurrent_checks: 16,
        }
    }
}

fn env_usize(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

fn env_bool(var: &str, default: bool) -> bool {
    match std::env::var(var) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        ),
        Err(_) => default,
    }
}

impl AdmissionConfig {
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            max_known_peers: env_usize("AVALON_NODE_MAX_KNOWN_PEERS", d.max_known_peers),
            max_url_len: env_usize("AVALON_NODE_MAX_URL_LENGTH", d.max_url_len),
            new_urls_per_source: env_usize(
                "AVALON_ANNOUNCE_NEW_PEERS_PER_SOURCE_PER_MINUTE",
                d.new_urls_per_source,
            ),
            max_new_per_exchange: env_usize(
                "AVALON_GOSSIP_MAX_NEW_PEERS_PER_EXCHANGE",
                d.max_new_per_exchange,
            ),
            max_new_shard_urls_per_exchange: env_usize(
                "AVALON_GOSSIP_MAX_NEW_SHARD_URLS_PER_EXCHANGE",
                d.max_new_shard_urls_per_exchange,
            ),
            verify_reachability: env_bool(
                "AVALON_ANNOUNCE_VERIFY_REACHABILITY",
                d.verify_reachability,
            ),
            max_concurrent_checks: env_usize(
                "AVALON_ANNOUNCE_MAX_CONCURRENT_CHECKS",
                d.max_concurrent_checks,
            ),
        }
    }
}

/// Why an announced or gossiped address was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum AdmitError {
    TooLong,
    Policy(PolicyError),
    Unreachable,
    NetworkMismatch,
    TableFull,
}

impl From<PolicyError> for AdmitError {
    fn from(e: PolicyError) -> Self {
        AdmitError::Policy(e)
    }
}

impl From<AdmitError> for TopologyError {
    fn from(e: AdmitError) -> Self {
        match e {
            AdmitError::TooLong => TopologyError::new(
                StatusCode::BAD_REQUEST,
                "base_url_too_long",
                "base_url exceeds the maximum length",
            ),
            AdmitError::Policy(PolicyError::Forbidden(_)) => TopologyError::new(
                StatusCode::FORBIDDEN,
                "base_url_not_allowed",
                "base_url address is not allowed by this node's outbound policy",
            ),
            AdmitError::Policy(PolicyError::Resolve) => TopologyError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "base_url_unresolvable",
                "base_url host did not resolve",
            ),
            AdmitError::Policy(_) => TopologyError::new(
                StatusCode::BAD_REQUEST,
                "invalid_base_url",
                "base_url must be a plain http(s) base URL without credentials, query or fragment",
            ),
            AdmitError::Unreachable => TopologyError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "peer_unreachable",
                "base_url did not answer /nodes/status",
            ),
            AdmitError::NetworkMismatch => TopologyError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "peer_network_mismatch",
                "base_url answered with a different network_id",
            ),
            AdmitError::TableFull => TopologyError {
                retry_after_secs: Some(60),
                ..TopologyError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "peer_table_full",
                    "peer table is full and nothing can be evicted",
                )
            },
        }
    }
}

pub struct PeerAdmission {
    pub cfg: AdmissionConfig,
    pub policy: OutboundPolicy,
    source: IpRateLimiter,
    checks: InFlightGate,
}

/// Process-wide admission rules, read from the environment once.
pub fn admission() -> &'static PeerAdmission {
    static ADMISSION: OnceLock<PeerAdmission> = OnceLock::new();
    ADMISSION
        .get_or_init(|| PeerAdmission::new(AdmissionConfig::from_env(), OutboundPolicy::from_env()))
}

impl PeerAdmission {
    pub fn new(cfg: AdmissionConfig, policy: OutboundPolicy) -> Self {
        Self {
            source: IpRateLimiter::new(cfg.new_urls_per_source, SOURCE_WINDOW),
            checks: InFlightGate::new(cfg.max_concurrent_checks),
            cfg,
            policy,
        }
    }

    /// Length and URL-shape checks; returns the base URL without a trailing slash.
    pub fn check_shape(&self, raw: &str) -> Result<String, AdmitError> {
        if raw.len() > self.cfg.max_url_len {
            return Err(AdmitError::TooLong);
        }
        let url = OutboundPolicy::parse_base_url(raw)?;
        Ok(url.as_str().trim_end_matches('/').to_string())
    }

    /// Resolves the host and applies the outbound address policy.
    pub async fn check_address(&self, base_url: &str) -> Result<CheckedTarget, AdmitError> {
        Ok(self.policy.check_base_url(base_url).await?)
    }

    /// Counts one new base URL against `source`'s per-window budget.
    pub fn admit_new_url_from(&self, source: IpAddr) -> Result<(), TopologyError> {
        self.source.check(source, Instant::now()).map_err(|wait| {
            TopologyError::rate_limited(wait.as_secs() + u64::from(wait.subsec_nanos() > 0))
        })
    }

    /// Takes a slot for one resolution plus reachability check.
    pub fn enter_check(&self) -> Result<tokio::sync::OwnedSemaphorePermit, TopologyError> {
        self.checks.try_enter()
    }

    /// `GET /nodes/status` on the checked address must answer with this network's id. Returns the
    /// libp2p peer id that server reports for itself, if any.
    pub async fn verify_reachable(
        &self,
        target: &CheckedTarget,
        network_id: &str,
    ) -> Result<Option<String>, AdmitError> {
        let client = target.client(REACHABILITY_TIMEOUT);
        let mut response = client
            .get(format!("{}/nodes/status", target.base_url))
            .send()
            .await
            .map_err(|_| AdmitError::Unreachable)?;
        if !response.status().is_success() {
            return Err(AdmitError::Unreachable);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| AdmitError::Unreachable)?
        {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_STATUS_BODY {
                return Err(AdmitError::Unreachable);
            }
        }
        let status: serde_json::Value =
            serde_json::from_slice(&body).map_err(|_| AdmitError::Unreachable)?;
        match status.get("network_id").and_then(|v| v.as_str()) {
            Some(id) if id == network_id => Ok(status
                .get("libp2p_peer_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)),
            Some(_) => Err(AdmitError::NetworkMismatch),
            None => Err(AdmitError::Unreachable),
        }
    }

    /// Whether a shard id is acceptable in the registry.
    pub fn shard_id_ok(&self, shard_id: &str) -> bool {
        !shard_id.is_empty() && shard_id.len() <= MAX_SHARD_ID_LEN
    }
}

/// Longest accepted libp2p multiaddr string.
const MAX_LIBP2P_ADDR_LEN: usize = 256;
/// Most libp2p addresses kept per peer.
const MAX_LIBP2P_ADDRS_PER_PEER: usize = 8;

/// Keeps the announced libp2p addresses that are safe to dial for `peer_id`: direct ip4/ip6 tcp
/// addresses and relayed ones ending in the announced peer id, all passing `policy`.
pub fn sanitize_libp2p_addrs(
    peer_id: Option<&str>,
    addrs: &[String],
    policy: &OutboundPolicy,
) -> Vec<String> {
    let Some(peer_id) = peer_id.and_then(|p| p.parse::<PeerId>().ok()) else {
        return Vec::new();
    };
    let mut kept: Vec<String> = Vec::new();
    for raw in addrs {
        if kept.len() >= MAX_LIBP2P_ADDRS_PER_PEER {
            break;
        }
        if raw.len() > MAX_LIBP2P_ADDR_LEN {
            continue;
        }
        let Ok(addr) = raw.parse::<Multiaddr>() else {
            continue;
        };
        if addr_acceptable(&addr, &peer_id, policy) {
            let canonical = addr.to_string();
            if !kept.contains(&canonical) {
                kept.push(canonical);
            }
        }
    }
    kept
}

/// `/ip/tcp[/p2p/<id>]`, the part of an address dialed directly; a trailing id must equal
/// `expected` when given and is required when `require_id` is set.
fn direct_part_ok(
    parts: &[Protocol<'_>],
    policy: &OutboundPolicy,
    expected: Option<&PeerId>,
    require_id: bool,
) -> bool {
    let ip_ok = match parts.first() {
        Some(Protocol::Ip4(ip)) => policy.check_ip(IpAddr::V4(*ip)).is_ok(),
        Some(Protocol::Ip6(ip)) => policy.check_ip(IpAddr::V6(*ip)).is_ok(),
        _ => false,
    };
    if !ip_ok || !matches!(parts.get(1), Some(Protocol::Tcp(port)) if *port != 0) {
        return false;
    }
    match (parts.len(), parts.get(2)) {
        (2, _) => !require_id,
        (3, Some(Protocol::P2p(id))) => expected.is_none_or(|want| want == id),
        _ => false,
    }
}

fn addr_acceptable(addr: &Multiaddr, peer_id: &PeerId, policy: &OutboundPolicy) -> bool {
    let parts: Vec<Protocol<'_>> = addr.iter().collect();
    let circuits = parts
        .iter()
        .filter(|p| matches!(p, Protocol::P2pCircuit))
        .count();
    match circuits {
        0 => direct_part_ok(&parts, policy, Some(peer_id), false),
        1 => {
            let Some(at) = parts.iter().position(|p| matches!(p, Protocol::P2pCircuit)) else {
                return false;
            };
            let relay_part = &parts[..at];
            direct_part_ok(relay_part, policy, None, true)
                && matches!(&parts[at + 1..], [Protocol::P2p(id)] if id == peer_id)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn adm(allow_private: bool, tweak: impl FnOnce(&mut AdmissionConfig)) -> PeerAdmission {
        let mut cfg = AdmissionConfig::default();
        tweak(&mut cfg);
        PeerAdmission::new(cfg, OutboundPolicy::new(allow_private))
    }

    #[test]
    fn shape_rejects_bad_schemes_credentials_queries_and_long_urls() {
        let a = adm(false, |c| c.max_url_len = 40);
        assert_eq!(
            a.check_shape("http://example.com/").unwrap(),
            "http://example.com"
        );
        assert_eq!(
            a.check_shape("ftp://example.com"),
            Err(AdmitError::Policy(PolicyError::Scheme))
        );
        assert_eq!(
            a.check_shape("http://u:p@example.com"),
            Err(AdmitError::Policy(PolicyError::Userinfo))
        );
        assert_eq!(
            a.check_shape("http://example.com/?x=1"),
            Err(AdmitError::Policy(PolicyError::QueryOrFragment))
        );
        assert_eq!(
            a.check_shape("http://example.com/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            Err(AdmitError::TooLong)
        );
    }

    #[tokio::test]
    async fn address_policy_follows_the_private_peers_flag() {
        let strict = adm(false, |_| {});
        for u in [
            "http://127.0.0.1:9",
            "http://10.0.0.5",
            "http://169.254.169.254",
            "http://[::1]:9",
            "http://localhost:9",
        ] {
            assert!(
                matches!(
                    strict.check_address(u).await,
                    Err(AdmitError::Policy(PolicyError::Forbidden(_)))
                ),
                "{u}"
            );
        }
        let lax = adm(true, |_| {});
        assert!(lax.check_address("http://127.0.0.1:9").await.is_ok());
        assert!(lax.check_address("http://169.254.169.254").await.is_err());
    }

    #[test]
    fn a_source_may_introduce_only_so_many_new_urls_per_window() {
        let a = adm(false, |c| c.new_urls_per_source = 2);
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        let other: IpAddr = "203.0.113.8".parse().unwrap();
        assert!(a.admit_new_url_from(ip).is_ok());
        assert!(a.admit_new_url_from(ip).is_ok());
        let err = a.admit_new_url_from(ip).unwrap_err();
        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
        assert!(err.retry_after_secs.unwrap() >= 1);
        assert!(a.admit_new_url_from(other).is_ok());
    }

    #[test]
    fn errors_map_to_distinct_client_statuses() {
        let status = |e: AdmitError| TopologyError::from(e).status;
        assert_eq!(status(AdmitError::TooLong), StatusCode::BAD_REQUEST);
        assert_eq!(
            status(AdmitError::Policy(PolicyError::Forbidden(
                "127.0.0.1".parse().unwrap()
            ))),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status(AdmitError::Unreachable),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let full = TopologyError::from(AdmitError::TableFull);
        assert_eq!(full.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(full.retry_after_secs.is_some());
    }

    #[tokio::test]
    async fn reachability_requires_a_status_answer_on_the_same_network() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/nodes/status"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"network_id": "n1"})),
            )
            .mount(&server)
            .await;
        let a = adm(true, |_| {});
        let target = a.check_address(&server.uri()).await.unwrap();
        assert_eq!(a.verify_reachable(&target, "n1").await, Ok(None));
        assert_eq!(
            a.verify_reachable(&target, "n2").await,
            Err(AdmitError::NetworkMismatch)
        );

        let down = MockServer::start().await;
        let target = a.check_address(&down.uri()).await.unwrap();
        assert_eq!(
            a.verify_reachable(&target, "n1").await,
            Err(AdmitError::Unreachable)
        );
    }

    #[tokio::test]
    async fn reachability_returns_the_libp2p_id_the_server_reports() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/nodes/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"network_id": "n1", "libp2p_peer_id": "12D3KooWabc"}),
            ))
            .mount(&server)
            .await;
        let a = adm(true, |_| {});
        let target = a.check_address(&server.uri()).await.unwrap();
        assert_eq!(
            a.verify_reachable(&target, "n1").await,
            Ok(Some("12D3KooWabc".to_string()))
        );
    }

    #[test]
    fn check_slots_are_bounded() {
        let a = adm(false, |c| c.max_concurrent_checks = 1);
        let p = a.enter_check().unwrap();
        assert!(a.enter_check().is_err());
        drop(p);
        assert!(a.enter_check().is_ok());
    }

    fn pid(seed: u8) -> PeerId {
        let kp = libp2p::identity::Keypair::ed25519_from_bytes([seed; 32]).unwrap();
        PeerId::from(kp.public())
    }

    fn sanitize(id: &PeerId, addrs: &[String], allow_private: bool) -> Vec<String> {
        sanitize_libp2p_addrs(
            Some(&id.to_string()),
            addrs,
            &OutboundPolicy::new(allow_private),
        )
    }

    #[test]
    fn sanitize_keeps_direct_and_relayed_forms_for_the_announced_peer() {
        let (me, relay) = (pid(1), pid(2));
        let addrs = vec![
            "/ip4/203.0.113.7/tcp/4001".to_string(),
            format!("/ip4/203.0.113.7/tcp/4002/p2p/{me}"),
            "/ip6/2001:db8::1/tcp/4001".to_string(),
            format!("/ip4/198.51.100.9/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{me}"),
        ];
        assert_eq!(sanitize(&me, &addrs, false), addrs);
    }

    #[test]
    fn sanitize_drops_everything_without_a_peer_id() {
        let addrs = vec!["/ip4/203.0.113.7/tcp/4001".to_string()];
        let p = OutboundPolicy::new(false);
        assert!(sanitize_libp2p_addrs(None, &addrs, &p).is_empty());
        assert!(sanitize_libp2p_addrs(Some("not-a-peer-id"), &addrs, &p).is_empty());
    }

    #[test]
    fn sanitize_rejects_a_forged_peer_id() {
        let (me, other, relay) = (pid(1), pid(3), pid(2));
        let addrs = vec![
            format!("/ip4/203.0.113.7/tcp/4001/p2p/{other}"),
            format!("/ip4/198.51.100.9/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{other}"),
            "/ip4/203.0.113.7/tcp/4003".to_string(),
        ];
        assert_eq!(sanitize(&me, &addrs, false), vec![addrs[2].clone()]);
    }

    #[test]
    fn sanitize_applies_the_ip_policy_to_direct_and_relay_parts() {
        let (me, relay) = (pid(1), pid(2));
        let addrs = vec![
            "/ip4/10.0.0.5/tcp/4001".to_string(),
            "/ip6/fd00::1/tcp/4001".to_string(),
            format!("/ip4/192.168.1.2/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{me}"),
            "/ip4/169.254.169.254/tcp/4001".to_string(),
            "/ip4/127.0.0.1/tcp/4001".to_string(),
        ];
        assert!(sanitize(&me, &addrs, false).is_empty());
        let lax = sanitize(&me, &addrs, true);
        assert_eq!(lax, [&addrs[..3], &addrs[4..]].concat());
        assert!(sanitize_libp2p_addrs(
            Some(&me.to_string()),
            &["/ip4/169.254.169.254/tcp/1".to_string()],
            &OutboundPolicy::new(true)
        )
        .is_empty());
    }

    #[test]
    fn sanitize_rejects_dns_and_unsupported_transports() {
        let me = pid(1);
        let addrs = vec![
            "/dns4/example.com/tcp/4001".to_string(),
            "/dns/example.com/tcp/4001".to_string(),
            "/ip4/203.0.113.7/udp/4001/quic-v1".to_string(),
            "/ip4/203.0.113.7".to_string(),
            "/ip4/203.0.113.7/tcp/0".to_string(),
            "garbage".to_string(),
        ];
        assert!(sanitize(&me, &addrs, false).is_empty());
    }

    #[test]
    fn sanitize_rejects_malformed_circuits() {
        let (me, relay) = (pid(1), pid(2));
        let base = "/ip4/198.51.100.9/tcp/4001";
        let addrs = vec![
            format!("{base}/p2p-circuit/p2p/{me}"),
            format!("{base}/p2p/{relay}/p2p-circuit"),
            format!("{base}/p2p/{relay}/p2p-circuit/p2p/{me}/p2p-circuit/p2p/{me}"),
            format!("{base}/p2p/{relay}/p2p-circuit/p2p/{relay}/p2p-circuit/p2p/{me}"),
            format!("{base}/p2p/{relay}/p2p-circuit/tcp/1"),
            format!("{base}/p2p/{relay}/p2p-circuit/p2p/{me}/tcp/1"),
            format!("/p2p/{relay}/p2p-circuit/p2p/{me}"),
        ];
        assert!(sanitize(&me, &addrs, false).is_empty());
    }

    #[test]
    fn sanitize_bounds_length_count_and_duplicates() {
        let me = pid(1);
        let long = format!(
            "/ip4/203.0.113.7/tcp/4001/p2p/{me}/{}",
            "x".repeat(MAX_LIBP2P_ADDR_LEN)
        );
        assert!(long.len() > MAX_LIBP2P_ADDR_LEN);
        let mut addrs = vec![long, "/ip4/203.0.113.7/tcp/4001".to_string()];
        addrs.push(addrs[1].clone());
        assert_eq!(sanitize(&me, &addrs, false).len(), 1);

        let many: Vec<String> = (0..20)
            .map(|i| format!("/ip4/203.0.113.7/tcp/{}", 5000 + i))
            .collect();
        let kept = sanitize(&me, &many, false);
        assert_eq!(kept, many[..MAX_LIBP2P_ADDRS_PER_PEER].to_vec());
    }
}
