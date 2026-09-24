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
//! Redirects are never followed by clients built here.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use url::{Host, Url};

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
}

impl CheckedTarget {
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

/// Addresses refused regardless of configuration.
pub fn always_forbidden(ip: IpAddr) -> bool {
    match ip.to_canonical() {
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
    match ip.to_canonical() {
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
        let url = Self::parse_base_url(base_url)?;
        let port = url.port_or_known_default().ok_or(PolicyError::InvalidUrl)?;
        let base = url.as_str().trim_end_matches('/').to_string();
        match url.host().ok_or(PolicyError::InvalidUrl)? {
            Host::Ipv4(ip) => self.literal(base, IpAddr::V4(ip), port),
            Host::Ipv6(ip) => self.literal(base, IpAddr::V6(ip), port),
            Host::Domain(name) => {
                let addrs: Vec<SocketAddr> = tokio::net::lookup_host((name, port))
                    .await
                    .map_err(|_| PolicyError::Resolve)?
                    .collect();
                for a in &addrs {
                    self.check_ip(a.ip())?;
                }
                let addr = *addrs.first().ok_or(PolicyError::Resolve)?;
                Ok(CheckedTarget {
                    base_url: base,
                    pinned_host: Some(name.to_string()),
                    addr,
                })
            }
        }
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
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
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
