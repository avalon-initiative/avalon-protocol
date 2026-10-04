//! Outbound address policy for requests this node makes to a peer-supplied URL.
//!
//! Peer table entries arrive through gossip, so a base URL is attacker
//! influenced. Before contacting one, the URL is parsed and every address its
//! host resolves to is checked; the connection is then pinned to a checked
//! address so a second, different DNS answer cannot redirect it.
//!
//! Always refused: non-http(s) schemes, URLs with userinfo, query or fragment,
//! unspecified, multicast, broadcast, reserved (`240.0.0.0/4`, `0.0.0.0/8`)
//! and link-local addresses (`169.254.0.0/16` including cloud metadata,
//! `fe80::/10`). Loopback, RFC 1918, shared address space (`100.64.0.0/10`),
//! unique local (`fc00::/7`) and deprecated site-local (`fec0::/10`) addresses
//! are refused unless `AVALON_ALLOW_PRIVATE_PEERS` is true. IPv4-mapped IPv6
//! addresses are judged as the IPv4 address they carry.
//!
//! NAT64-embedded addresses (`64:ff9b::/96`, `64:ff9b:1::/48`) and 6to4
//! addresses (`2002::/16`) are unwrapped to their embedded IPv4 address the
//! same way. Redirects are never followed by clients built here.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use url::{Host, Url};

const PEER_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
pub(crate) const PEER_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest a hostname lookup may take before the host counts as unresolvable.
const DNS_LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);

/// Most lookups running at once for the node's own fetches.
const MAX_CRITICAL_LOOKUPS: usize = 32;
/// Most lookups running at once for names an unauthenticated caller can supply.
pub(crate) const MAX_UNTRUSTED_LOOKUPS: usize = 8;

/// Who a hostname came from, which decides the slot pool its lookup runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupPurpose {
    /// The node's own outbound fetches to peers it already chose.
    Critical,
    /// A name a remote caller supplied: announces, gossip, probes.
    Untrusted,
}

/// Separate slot pools, so abandoned lookups of caller-supplied names cannot starve the node's own.
struct LookupPools {
    critical: std::sync::Arc<tokio::sync::Semaphore>,
    untrusted: std::sync::Arc<tokio::sync::Semaphore>,
}

impl LookupPools {
    fn new(critical: usize, untrusted: usize) -> Self {
        Self {
            critical: std::sync::Arc::new(tokio::sync::Semaphore::new(critical)),
            untrusted: std::sync::Arc::new(tokio::sync::Semaphore::new(untrusted)),
        }
    }

    fn slots(&self, purpose: LookupPurpose) -> &std::sync::Arc<tokio::sync::Semaphore> {
        match purpose {
            LookupPurpose::Critical => &self.critical,
            LookupPurpose::Untrusted => &self.untrusted,
        }
    }
}

/// How long a host that failed to resolve is refused without a new lookup.
const NEGATIVE_TTL: Duration = Duration::from_secs(45);
/// Most failed hosts remembered at once.
const MAX_NEGATIVE_HOSTS: usize = 512;

type LookupAnswer = Result<Vec<IpAddr>, PolicyError>;
type LookupFn = std::sync::Arc<
    dyn Fn(String) -> futures_util::future::BoxFuture<'static, std::io::Result<Vec<IpAddr>>>
        + Send
        + Sync,
>;

/// Hostname lookups with bounded slot pools. One lookup runs per host at a time and later
/// callers wait on it; a host that failed is refused for [`NEGATIVE_TTL`] without a lookup, so
/// one bad name cannot take a fresh slot per retry. A lookup's slot is freed at its deadline.
struct Resolver {
    pools: LookupPools,
    flights: std::sync::Mutex<
        std::collections::HashMap<String, tokio::sync::watch::Receiver<Option<LookupAnswer>>>,
    >,
    failed: std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
    lookup: LookupFn,
    limit: Duration,
    negative_ttl: Duration,
}

impl Resolver {
    fn new(pools: LookupPools, lookup: LookupFn, limit: Duration, negative_ttl: Duration) -> Self {
        Self {
            pools,
            flights: Default::default(),
            failed: Default::default(),
            lookup,
            limit,
            negative_ttl,
        }
    }

    fn remember_failure(&self, host: &str) {
        let now = std::time::Instant::now();
        let mut failed = self.failed.lock().unwrap_or_else(|e| e.into_inner());
        if failed.len() >= MAX_NEGATIVE_HOSTS && !failed.contains_key(host) {
            failed.retain(|_, until| *until > now);
            if failed.len() >= MAX_NEGATIVE_HOSTS {
                if let Some(oldest) = failed
                    .iter()
                    .min_by_key(|(_, u)| **u)
                    .map(|(h, _)| h.clone())
                {
                    failed.remove(&oldest);
                }
            }
        }
        failed.insert(host.to_string(), now + self.negative_ttl);
    }

    fn recently_failed(&self, host: &str) -> bool {
        let failed = self.failed.lock().unwrap_or_else(|e| e.into_inner());
        failed
            .get(host)
            .is_some_and(|until| *until > std::time::Instant::now())
    }

    async fn resolve(
        self: &std::sync::Arc<Self>,
        name: &str,
        port: u16,
        purpose: LookupPurpose,
    ) -> Result<Vec<SocketAddr>, PolicyError> {
        let host = name.to_ascii_lowercase();
        if self.recently_failed(&host) {
            return Err(PolicyError::Resolve);
        }
        let mut rx =
            {
                let mut flights = self.flights.lock().unwrap_or_else(|e| e.into_inner());
                match flights.get(&host) {
                    Some(rx) => rx.clone(),
                    None => {
                        let permit = self
                            .pools
                            .slots(purpose)
                            .clone()
                            .try_acquire_owned()
                            .map_err(|_| PolicyError::Resolve)?;
                        let (tx, rx) = tokio::sync::watch::channel(None);
                        flights.insert(host.clone(), rx.clone());
                        let (this, key) = (self.clone(), host.clone());
                        tokio::spawn(async move {
                            let answer =
                                match tokio::time::timeout(this.limit, (this.lookup)(key.clone()))
                                    .await
                                {
                                    Ok(Ok(addrs)) if !addrs.is_empty() => Ok(addrs),
                                    _ => Err(PolicyError::Resolve),
                                };
                            if answer.is_err() {
                                this.remember_failure(&key);
                            }
                            this.flights
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .remove(&key);
                            drop(permit);
                            let _ = tx.send(Some(answer));
                        });
                        rx
                    }
                }
            };
        let answer = rx
            .wait_for(Option::is_some)
            .await
            .map_err(|_| PolicyError::Resolve)?
            .clone()
            .ok_or(PolicyError::Resolve)??;
        Ok(answer
            .into_iter()
            .map(|ip| SocketAddr::new(ip, port))
            .collect())
    }
}

async fn resolve_host(
    name: &str,
    port: u16,
    purpose: LookupPurpose,
) -> Result<Vec<SocketAddr>, PolicyError> {
    static RESOLVER: std::sync::OnceLock<std::sync::Arc<Resolver>> = std::sync::OnceLock::new();
    let resolver = RESOLVER.get_or_init(|| {
        std::sync::Arc::new(Resolver::new(
            LookupPools::new(MAX_CRITICAL_LOOKUPS, MAX_UNTRUSTED_LOOKUPS),
            std::sync::Arc::new(|name: String| {
                Box::pin(async move {
                    Ok(tokio::net::lookup_host((name.as_str(), 0))
                        .await?
                        .map(|a| a.ip())
                        .collect())
                })
            }),
            DNS_LOOKUP_TIMEOUT,
            NEGATIVE_TTL,
        ))
    });
    resolver.resolve(name, port, purpose).await
}

/// HTTP client for requests to other nodes, which follows no redirects and uses no proxy so a
/// signed request never reaches a host other than the one named: a peer that accepts the connection but never answers
/// costs one bounded attempt instead of stalling the caller.
pub fn peer_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(PEER_CONNECT_TIMEOUT)
        .timeout(PEER_REQUEST_TIMEOUT)
        .build()
        .expect("static reqwest client configuration is valid")
}

/// Whether `err` is a refusal only `AVALON_ALLOW_PRIVATE_PEERS` would lift.
fn lifted_by_private_peers(policy: OutboundPolicy, err: &PolicyError) -> bool {
    matches!(err, PolicyError::Forbidden(ip)
        if !policy.allow_private && !always_forbidden(*ip) && is_private(*ip))
}

/// Warns at most once per `target` per interval when the policy refused a private address that
/// `AVALON_ALLOW_PRIVATE_PEERS=true` would allow, so private deployments are not silent.
pub(crate) fn note_refusal(policy: OutboundPolicy, target: &str, err: &PolicyError) {
    static LOG: std::sync::OnceLock<crate::log_throttle::LogThrottle> = std::sync::OnceLock::new();
    if !lifted_by_private_peers(policy, err) {
        return;
    }
    let log = LOG.get_or_init(|| crate::log_throttle::LogThrottle::new(Duration::from_secs(300)));
    if let Some(held_back) = log.permit(target, std::time::Instant::now()) {
        tracing::warn!(
            event = "private_peer_refused",
            target = %target,
            held_back,
            "outbound request to a private address refused; set AVALON_ALLOW_PRIVATE_PEERS=true \
             if this node's peers are on a private network",
        );
    }
}

/// Resolver that refuses a host when any address it resolves to fails the policy, so the check
/// runs on the answer the connection actually uses.
struct GuardedResolver(OutboundPolicy);

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let policy = self.0;
        Box::pin(async move {
            let addrs = check_answer(
                policy,
                resolve_host(name.as_str(), 0, LookupPurpose::Critical).await?,
            )
            .inspect_err(|e| note_refusal(policy, name.as_str(), e))?;
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// The resolver's answer when every address passes `policy`; one forbidden address refuses all.
fn check_answer(
    policy: OutboundPolicy,
    addrs: Vec<SocketAddr>,
) -> Result<Vec<SocketAddr>, PolicyError> {
    if addrs.is_empty() {
        return Err(PolicyError::Resolve);
    }
    for a in &addrs {
        policy.check_ip(a.ip())?;
    }
    Ok(addrs)
}

/// [`peer_client`] whose hostname lookups are checked against `policy` at connect time. IP
/// literals skip the resolver, so callers check those with [`OutboundPolicy::check_url_literal`].
pub fn guarded_peer_client(policy: OutboundPolicy) -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(PEER_CONNECT_TIMEOUT)
        .timeout(PEER_REQUEST_TIMEOUT)
        .dns_resolver(std::sync::Arc::new(GuardedResolver(policy)))
        .build()
        .expect("static reqwest client configuration is valid")
}

/// Why a URL or address was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    InvalidUrl,
    Scheme,
    Userinfo,
    QueryOrFragment,
    Forbidden(IpAddr),
    Resolve,
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PolicyError::InvalidUrl => write!(f, "not a valid http(s) base URL"),
            PolicyError::Scheme => write!(f, "only http and https are allowed"),
            PolicyError::Userinfo => write!(f, "URLs with credentials are not allowed"),
            PolicyError::QueryOrFragment => write!(f, "query and fragment are not allowed"),
            PolicyError::Forbidden(_) => write!(f, "address is not allowed by outbound policy"),
            PolicyError::Resolve => write!(f, "host did not resolve"),
        }
    }
}

impl std::error::Error for PolicyError {}

/// Whether private and loopback addresses may be contacted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboundPolicy {
    pub allow_private: bool,
}

/// A base URL that passed the policy, with the address the connection must use.
#[derive(Debug, Clone)]
pub struct CheckedTarget {
    /// Base URL without a trailing slash.
    pub base_url: String,
    /// Host name to pin, `None` when the URL host is an IP literal.
    pub pinned_host: Option<String>,
    pub addr: SocketAddr,
    /// The policy that vetted this target; failover to a peer-table URL is held to it too.
    pub policy: OutboundPolicy,
}

/// A node URL that passed the policy: an address-pinned http(s) target, or a `p2p://` peer that
/// has no address to check and is only ever reached through a [`crate::node_http::NodeClient`].
#[derive(Debug, Clone)]
pub struct NodeTarget {
    /// The base URL to build request URLs from.
    pub base_url: String,
    http: Option<CheckedTarget>,
    policy: OutboundPolicy,
}

impl NodeTarget {
    pub fn node_client(&self, timeout: Duration) -> crate::node_http::NodeClient {
        match &self.http {
            Some(checked) => checked.node_client(timeout),
            // Never dialed for a p2p:// target; bounded anyway so it cannot follow or proxy.
            None => crate::node_http::NodeClient::from(
                reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .no_proxy()
                    .timeout(timeout)
                    .build()
                    .unwrap_or_default(),
            )
            .with_timeout(timeout)
            .with_policy(self.policy),
        }
    }
}

impl From<CheckedTarget> for NodeTarget {
    fn from(checked: CheckedTarget) -> Self {
        Self {
            base_url: checked.base_url.clone(),
            policy: checked.policy,
            http: Some(checked),
        }
    }
}

impl CheckedTarget {
    /// [`Self::client`] as a [`crate::node_http::NodeClient`].
    pub fn node_client(&self, timeout: Duration) -> crate::node_http::NodeClient {
        crate::node_http::NodeClient::from(self.client(timeout))
            .with_timeout(timeout)
            .with_policy(self.policy)
    }

    /// A client that talks only to the checked address, follows no redirects
    /// and gives up after `timeout`.
    pub fn client(&self, timeout: Duration) -> reqwest::Client {
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .no_proxy();
        if let Some(host) = &self.pinned_host {
            builder = builder.resolve(host, self.addr);
        }
        builder.build().unwrap_or_default()
    }
}

/// Unwraps IPv4-mapped, NAT64 (`64:ff9b::/96`, `64:ff9b:1::/48`) and 6to4
/// (`2002::/16`) addresses to the IPv4 address they embed.
fn canonicalize(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => unwrap_embedded_v4(v6),
        v4 => v4,
    }
}

fn unwrap_embedded_v4(v6: Ipv6Addr) -> IpAddr {
    let o = v6.octets();
    if o[0..12] == [0, 0x64, 0xff, 0x9b, 0, 0, 0, 0, 0, 0, 0, 0] {
        return IpAddr::V4(Ipv4Addr::new(o[12], o[13], o[14], o[15]));
    }
    if o[0..6] == [0, 0x64, 0xff, 0x9b, 0, 1] {
        return IpAddr::V4(Ipv4Addr::new(o[6], o[7], o[9], o[10]));
    }
    if o[0] == 0x20 && o[1] == 0x02 {
        return IpAddr::V4(Ipv4Addr::new(o[2], o[3], o[4], o[5]));
    }
    IpAddr::V6(v6)
}

/// Addresses refused regardless of configuration.
pub fn always_forbidden(ip: IpAddr) -> bool {
    match canonicalize(ip) {
        IpAddr::V4(v4) => {
            v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_link_local()
                || v4.octets()[0] == 0
                || v4.octets()[0] >= 240
        }
        IpAddr::V6(v6) => {
            v6.is_unspecified() || v6.is_multicast() || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Loopback and private-range addresses, refused unless private peers are allowed.
pub fn is_private(ip: IpAddr) -> bool {
    match canonicalize(ip) {
        IpAddr::V4(v4) => is_private_v4(v4),
        IpAddr::V6(v6) => is_private_v6(v6),
    }
}

fn is_private_v4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    v4.is_loopback() || v4.is_private() || (o[0] == 100 && (o[1] & 0xc0) == 64)
}

fn is_private_v6(v6: Ipv6Addr) -> bool {
    let s = v6.segments()[0];
    v6.is_loopback() || (s & 0xfe00) == 0xfc00 || (s & 0xffc0) == 0xfec0
}

impl OutboundPolicy {
    pub fn new(allow_private: bool) -> Self {
        Self { allow_private }
    }

    /// `AVALON_ALLOW_PRIVATE_PEERS` (default false).
    pub fn from_env() -> Self {
        let allow = std::env::var("AVALON_ALLOW_PRIVATE_PEERS")
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "true" | "1" | "yes" | "on"
                )
            })
            .unwrap_or(false);
        Self::new(allow)
    }

    pub fn check_ip(&self, ip: IpAddr) -> Result<(), PolicyError> {
        if always_forbidden(ip) || (!self.allow_private && is_private(ip)) {
            return Err(PolicyError::Forbidden(ip.to_canonical()));
        }
        Ok(())
    }

    /// Refuses a URL whose host is an IP literal the policy forbids; a hostname passes, since
    /// the connect-time resolver of [`guarded_peer_client`] checks its addresses.
    pub fn check_url_literal(&self, url: &str) -> Result<(), PolicyError> {
        match Url::parse(url.trim())
            .map_err(|_| PolicyError::InvalidUrl)?
            .host()
        {
            Some(Host::Ipv4(ip)) => self.check_ip(IpAddr::V4(ip)),
            Some(Host::Ipv6(ip)) => self.check_ip(IpAddr::V6(ip)),
            Some(Host::Domain(_)) => Ok(()),
            None => Err(PolicyError::InvalidUrl),
        }
    }

    /// Parses `base_url` without resolving anything.
    pub fn parse_base_url(base_url: &str) -> Result<Url, PolicyError> {
        let url = Url::parse(base_url.trim()).map_err(|_| PolicyError::InvalidUrl)?;
        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(PolicyError::Scheme);
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(PolicyError::Userinfo);
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(PolicyError::QueryOrFragment);
        }
        if url.host().is_none() {
            return Err(PolicyError::InvalidUrl);
        }
        Ok(url)
    }

    /// Resolves the URL's host, requires every resolved address to pass the
    /// policy, and returns the target pinned to the first one.
    pub async fn check_base_url(&self, base_url: &str) -> Result<CheckedTarget, PolicyError> {
        self.check_base_url_for(base_url, LookupPurpose::Untrusted)
            .await
    }

    /// [`Self::check_base_url`] with its lookup in the pool for `purpose`.
    pub async fn check_base_url_for(
        &self,
        base_url: &str,
        purpose: LookupPurpose,
    ) -> Result<CheckedTarget, PolicyError> {
        let url = Self::parse_base_url(base_url)?;
        let port = url.port_or_known_default().ok_or(PolicyError::InvalidUrl)?;
        let base = url.as_str().trim_end_matches('/').to_string();
        match url.host().ok_or(PolicyError::InvalidUrl)? {
            Host::Ipv4(ip) => self.literal(base, IpAddr::V4(ip), port),
            Host::Ipv6(ip) => self.literal(base, IpAddr::V6(ip), port),
            Host::Domain(name) => {
                let addrs = resolve_host(name, port, purpose).await?;
                for a in &addrs {
                    self.check_ip(a.ip())?;
                }
                let addr = *addrs.first().ok_or(PolicyError::Resolve)?;
                Ok(CheckedTarget {
                    base_url: base,
                    pinned_host: Some(name.to_string()),
                    addr,
                    policy: *self,
                })
            }
        }
    }

    /// Like [`Self::check_base_url`], but also accepts a `p2p://<peer id>` node URL, which is
    /// reached over a libp2p stream and so has no address to check.
    pub async fn check_node_url(&self, url: &str) -> Result<NodeTarget, PolicyError> {
        self.check_node_url_for(url, LookupPurpose::Untrusted).await
    }

    /// [`Self::check_node_url`] with its lookup in the pool for `purpose`.
    pub async fn check_node_url_for(
        &self,
        url: &str,
        purpose: LookupPurpose,
    ) -> Result<NodeTarget, PolicyError> {
        if let Some(peer) = crate::node_http::parse_p2p_base(url) {
            return Ok(NodeTarget {
                base_url: crate::node_http::p2p_base_url(&peer),
                http: None,
                policy: *self,
            });
        }
        Ok(self.check_base_url_for(url, purpose).await?.into())
    }

    fn literal(
        &self,
        base_url: String,
        ip: IpAddr,
        port: u16,
    ) -> Result<CheckedTarget, PolicyError> {
        self.check_ip(ip)?;
        Ok(CheckedTarget {
            base_url,
            pinned_host: None,
            addr: SocketAddr::new(ip, port),
            policy: *self,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A resolver over a fake lookup: `slow.test` answers after 80 ms, `dead.test` fails,
    /// `hang.test` never answers, anything else answers at once. Counts lookups per name.
    fn fake_resolver(
        pools: LookupPools,
        limit: Duration,
        ttl: Duration,
    ) -> (Arc<Resolver>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let lookup: LookupFn = Arc::new(move |name: String| {
            counter.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                match name.as_str() {
                    "slow.test" => {
                        tokio::time::sleep(Duration::from_millis(80)).await;
                        Ok(vec![ip("93.184.216.34")])
                    }
                    "dead.test" => Err(std::io::Error::other("no such host")),
                    "hang.test" => std::future::pending().await,
                    _ => Ok(vec![ip("93.184.216.35")]),
                }
            })
        });
        (Arc::new(Resolver::new(pools, lookup, limit, ttl)), calls)
    }

    const LONG: Duration = Duration::from_secs(30);

    #[tokio::test]
    async fn concurrent_lookups_of_one_host_share_one_lookup_and_one_slot() {
        let (r, calls) = fake_resolver(LookupPools::new(4, 1), LONG, LONG);
        let got = futures_util::future::join_all(
            (0..6).map(|_| r.resolve("slow.test", 80, LookupPurpose::Untrusted)),
        )
        .await;
        assert!(got.iter().all(|g| g.as_ref().is_ok_and(|a| a.len() == 1)));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one lookup per host");
    }

    #[tokio::test]
    async fn a_failed_host_is_refused_without_a_new_lookup_until_the_ttl_ends() {
        let (r, calls) = fake_resolver(LookupPools::new(4, 4), LONG, Duration::from_millis(150));
        for _ in 0..5 {
            assert_eq!(
                r.resolve("dead.test", 80, LookupPurpose::Critical).await,
                Err(PolicyError::Resolve)
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "retries cost no lookup");
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = r.resolve("dead.test", 80, LookupPurpose::Critical).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2, "tried again after the ttl");
    }

    #[tokio::test]
    async fn the_failed_host_memory_is_bounded() {
        let (r, _) = fake_resolver(LookupPools::new(1, 1), LONG, LONG);
        for i in 0..MAX_NEGATIVE_HOSTS + 20 {
            r.remember_failure(&format!("h{i}.test"));
        }
        assert!(r.failed.lock().unwrap().len() <= MAX_NEGATIVE_HOSTS);
    }

    #[tokio::test]
    async fn a_lookup_that_never_answers_frees_its_slot_at_the_deadline() {
        let (r, _) = fake_resolver(
            LookupPools::new(1, 1),
            Duration::from_millis(60),
            Duration::from_millis(1),
        );
        let started = std::time::Instant::now();
        assert_eq!(
            r.resolve("hang.test", 80, LookupPurpose::Untrusted).await,
            Err(PolicyError::Resolve)
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            r.resolve("other.test", 80, LookupPurpose::Untrusted)
                .await
                .is_ok(),
            "the slot came back with the deadline"
        );
    }

    #[tokio::test]
    async fn caller_supplied_lookups_cannot_starve_the_nodes_own() {
        let (r, _) = fake_resolver(LookupPools::new(2, 1), LONG, LONG);
        let held = tokio::spawn({
            let r = r.clone();
            async move { r.resolve("slow.test", 80, LookupPurpose::Untrusted).await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            r.resolve("a.test", 80, LookupPurpose::Untrusted).await,
            Err(PolicyError::Resolve),
            "the untrusted pool is full"
        );
        assert!(r
            .resolve("b.test", 80, LookupPurpose::Critical)
            .await
            .is_ok());
        assert!(held.await.unwrap().is_ok());
    }
    #[test]
    fn only_a_private_address_refusal_is_reported_as_liftable() {
        let strict = OutboundPolicy::new(false);
        let lax = OutboundPolicy::new(true);
        let refused = |a: &str| PolicyError::Forbidden(ip(a));
        assert!(lifted_by_private_peers(strict, &refused("10.0.0.1")));
        assert!(lifted_by_private_peers(strict, &refused("127.0.0.1")));
        assert!(!lifted_by_private_peers(
            strict,
            &refused("169.254.169.254")
        ));
        assert!(!lifted_by_private_peers(lax, &refused("10.0.0.1")));
        assert!(!lifted_by_private_peers(strict, &PolicyError::Resolve));
    }

    #[test]
    fn junk_targets_cannot_silence_the_private_peers_hint_for_good() {
        let log = crate::log_throttle::LogThrottle::new(Duration::from_secs(300));
        let now = std::time::Instant::now();
        for i in 0..5000 {
            log.permit(&format!("junk{i}"), now);
        }
        assert!(log.permit("http://seed.lan", now).is_some());
    }

    #[test]
    fn one_forbidden_address_among_several_refuses_the_whole_answer() {
        let sa = |a: &str| SocketAddr::new(ip(a), 80);
        let p = OutboundPolicy::new(true);
        assert!(check_answer(p, vec![sa("93.184.216.34"), sa("169.254.169.254")]).is_err());
        assert!(check_answer(p, vec![sa("169.254.169.254"), sa("93.184.216.34")]).is_err());
        assert!(check_answer(p, vec![sa("93.184.216.34"), sa("10.0.0.1")]).is_ok());
        assert!(check_answer(
            OutboundPolicy::new(false),
            vec![sa("93.184.216.34"), sa("10.0.0.1")]
        )
        .is_err());
        assert_eq!(check_answer(p, vec![]), Err(PolicyError::Resolve));
    }

    #[test]
    fn odd_ipv4_forms_and_mapped_ipv6_literals_are_refused() {
        let strict = OutboundPolicy::new(false);
        let lax = OutboundPolicy::new(true);
        for u in [
            "http://2130706433/x",
            "http://0x7f.1/x",
            "http://127.1/x",
            "http://[::ffff:127.0.0.1]/x",
        ] {
            assert!(strict.check_url_literal(u).is_err(), "{u}");
        }
        for u in [
            "http://[::ffff:a9fe:a9fe]/x",
            "http://0xa9fea9fe/x",
            "http://2852039166/x",
        ] {
            assert!(strict.check_url_literal(u).is_err(), "{u}");
            assert!(lax.check_url_literal(u).is_err(), "{u}");
        }
    }

    #[test]
    fn link_local_and_metadata_are_always_refused() {
        for allow in [false, true] {
            let p = OutboundPolicy::new(allow);
            for a in ["169.254.169.254", "169.254.0.1", "fe80::1", "febf::1"] {
                assert!(p.check_ip(ip(a)).is_err(), "{a} allow={allow}");
            }
        }
    }

    #[test]
    fn unspecified_multicast_broadcast_reserved_are_always_refused() {
        let p = OutboundPolicy::new(true);
        for a in [
            "0.0.0.0",
            "0.1.2.3",
            "224.0.0.1",
            "239.1.1.1",
            "255.255.255.255",
            "240.0.0.1",
            "::",
            "ff02::1",
        ] {
            assert!(p.check_ip(ip(a)).is_err(), "{a}");
        }
    }

    #[test]
    fn private_ranges_need_the_flag() {
        let strict = OutboundPolicy::new(false);
        let lax = OutboundPolicy::new(true);
        for a in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "100.64.0.1",
            "100.127.255.255",
            "::1",
            "fd00::1",
            "fc00::1",
            "fec0::1",
        ] {
            assert!(strict.check_ip(ip(a)).is_err(), "{a}");
            assert!(lax.check_ip(ip(a)).is_ok(), "{a}");
        }
    }

    #[test]
    fn public_addresses_pass_and_range_edges_are_exact() {
        let p = OutboundPolicy::new(false);
        for a in [
            "8.8.8.8",
            "172.32.0.1",
            "172.15.255.255",
            "100.128.0.1",
            "100.63.255.255",
            "2606:4700::1",
            "169.253.0.1",
        ] {
            assert!(p.check_ip(ip(a)).is_ok(), "{a}");
        }
    }

    #[test]
    fn mapped_ipv6_is_judged_as_the_embedded_ipv4() {
        let p = OutboundPolicy::new(true);
        assert!(p.check_ip(ip("::ffff:169.254.169.254")).is_err());
        assert!(OutboundPolicy::new(false)
            .check_ip(ip("::ffff:10.0.0.1"))
            .is_err());
        assert!(OutboundPolicy::new(false)
            .check_ip(ip("::ffff:8.8.8.8"))
            .is_ok());
    }

    #[test]
    fn nat64_embedded_ipv4_is_judged_as_the_embedded_address() {
        let strict = OutboundPolicy::new(false);
        let lax = OutboundPolicy::new(true);
        // 64:ff9b::/96 (well-known), embedding 169.254.169.254, 127.0.0.1, 10.0.0.1, 8.8.8.8.
        assert!(strict.check_ip(ip("64:ff9b::a9fe:a9fe")).is_err());
        assert!(lax.check_ip(ip("64:ff9b::a9fe:a9fe")).is_err());
        assert!(strict.check_ip(ip("64:ff9b::7f00:1")).is_err());
        assert!(lax.check_ip(ip("64:ff9b::7f00:1")).is_ok());
        assert!(strict.check_ip(ip("64:ff9b::a00:1")).is_err());
        assert!(lax.check_ip(ip("64:ff9b::a00:1")).is_ok());
        assert!(strict.check_ip(ip("64:ff9b::808:808")).is_ok());

        // 64:ff9b:1::/48 (local-use), same embedded addresses (RFC 6052 skips
        // the reserved `u` octet at bits 64-71 for this prefix length).
        assert!(strict.check_ip(ip("64:ff9b:1:a9fe:a9:fe00::")).is_err());
        assert!(lax.check_ip(ip("64:ff9b:1:a9fe:a9:fe00::")).is_err());
        assert!(strict.check_ip(ip("64:ff9b:1:7f00:0:100::")).is_err());
        assert!(lax.check_ip(ip("64:ff9b:1:7f00:0:100::")).is_ok());
        assert!(strict.check_ip(ip("64:ff9b:1:a00:0:100::")).is_err());
        assert!(lax.check_ip(ip("64:ff9b:1:a00:0:100::")).is_ok());
        assert!(strict.check_ip(ip("64:ff9b:1:808:8:800::")).is_ok());
    }

    #[test]
    fn six_to_four_embedded_ipv4_is_judged_as_the_embedded_address() {
        let strict = OutboundPolicy::new(false);
        let lax = OutboundPolicy::new(true);
        // 2002::/16 embeds the IPv4 address in the next 32 bits.
        assert!(strict.check_ip(ip("2002:a9fe:a9fe::")).is_err());
        assert!(lax.check_ip(ip("2002:a9fe:a9fe::")).is_err());
        assert!(strict.check_ip(ip("2002:7f00:1::")).is_err());
        assert!(lax.check_ip(ip("2002:7f00:1::")).is_ok());
        assert!(strict.check_ip(ip("2002:808:808::")).is_ok());
    }

    #[tokio::test]
    async fn a_hostname_resolving_to_a_nat64_address_is_refused() {
        // check_base_url exercises literal IPv6 hosts through the same path.
        let strict = OutboundPolicy::new(false);
        assert!(matches!(
            strict
                .check_base_url("http://[64:ff9b::a9fe:a9fe]:9000")
                .await,
            Err(PolicyError::Forbidden(_))
        ));
    }

    #[test]
    fn url_shape_is_validated() {
        let parse = OutboundPolicy::parse_base_url;
        assert!(parse("http://example.com").is_ok());
        assert!(parse("https://example.com:8443/base").is_ok());
        assert_eq!(parse("ftp://example.com"), Err(PolicyError::Scheme));
        assert_eq!(parse("file:///etc/passwd"), Err(PolicyError::Scheme));
        assert_eq!(
            parse("http://user:pw@example.com"),
            Err(PolicyError::Userinfo)
        );
        assert_eq!(parse("http://user@example.com"), Err(PolicyError::Userinfo));
        assert_eq!(
            parse("http://example.com/?a=b"),
            Err(PolicyError::QueryOrFragment)
        );
        assert_eq!(
            parse("http://example.com/#f"),
            Err(PolicyError::QueryOrFragment)
        );
        assert_eq!(parse("nonsense"), Err(PolicyError::InvalidUrl));
    }

    #[tokio::test]
    async fn literal_hosts_are_checked_and_pinned() {
        let strict = OutboundPolicy::new(false);
        assert_eq!(
            strict
                .check_base_url("http://169.254.169.254")
                .await
                .unwrap_err(),
            PolicyError::Forbidden(ip("169.254.169.254"))
        );
        assert!(strict
            .check_base_url("http://127.0.0.1:9000")
            .await
            .is_err());
        assert!(strict.check_base_url("http://[::1]:9000").await.is_err());
        let t = OutboundPolicy::new(true)
            .check_base_url("http://127.0.0.1:9000/")
            .await
            .unwrap();
        assert_eq!(t.addr, "127.0.0.1:9000".parse().unwrap());
        assert_eq!(t.base_url, "http://127.0.0.1:9000");
        assert!(t.pinned_host.is_none());
    }

    #[tokio::test]
    async fn a_hostname_resolving_to_a_private_address_is_refused() {
        let strict = OutboundPolicy::new(false);
        assert!(matches!(
            strict.check_base_url("http://localhost:9000").await,
            Err(PolicyError::Forbidden(_))
        ));
        let t = OutboundPolicy::new(true)
            .check_base_url("http://localhost:9000")
            .await
            .unwrap();
        assert!(t.addr.ip().is_loopback());
        assert_eq!(t.pinned_host.as_deref(), Some("localhost"));
    }
}

#[cfg(test)]
mod peer_client_tests {
    use super::*;
    use std::time::Instant;

    #[tokio::test(flavor = "current_thread")]
    async fn peer_client_gives_up_on_a_peer_that_never_answers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _held = tokio::spawn(async move {
            let mut conns = Vec::new();
            while let Ok((c, _)) = listener.accept().await {
                conns.push(c);
            }
        });
        let started = Instant::now();
        let res = peer_client().get(format!("http://{addr}/x")).send().await;
        assert!(res.is_err());
        assert!(started.elapsed() < PEER_REQUEST_TIMEOUT + Duration::from_secs(5));
    }

    fn pinned_to(addr: SocketAddr, host: &str) -> CheckedTarget {
        CheckedTarget {
            base_url: format!("http://{host}:{}", addr.port()),
            pinned_host: Some(host.to_string()),
            addr,
            policy: OutboundPolicy::new(true),
        }
    }

    #[tokio::test]
    async fn the_client_dials_the_checked_address_not_a_fresh_resolution() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        // The name does not resolve at all: only the pinned address can answer.
        let target = pinned_to(*server.address(), "pinned.invalid");
        let res = target
            .client(Duration::from_secs(5))
            .get(&target.base_url)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
    }
}
