//! Client-address derivation for the per-IP rate limit. A forwarded address is
//! honored only when the direct peer is a configured trusted proxy, so a
//! client can never choose its own bucket by sending a header itself.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::http::{HeaderMap, Request};
use ipnet::IpNet;
use tower_governor::key_extractor::KeyExtractor;
use tower_governor::GovernorError;

const FORWARDED_FOR: &str = "x-forwarded-for";

/// IPs/CIDR ranges of the reverse proxies this node sits behind. Empty (the
/// default) trusts nothing, so the key is always the peer address.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedProxies {
    nets: Vec<IpNet>,
}

impl TrustedProxies {
    /// Parses a comma-separated list of IPs or CIDR ranges.
    pub fn parse(list: &str) -> Result<Self, String> {
        let mut nets = Vec::new();
        for entry in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let net = entry
                .parse::<IpNet>()
                .or_else(|_| entry.parse::<IpAddr>().map(IpNet::from))
                .map_err(|_| format!("invalid trusted proxy entry: {entry:?}"))?;
            nets.push(net);
        }
        Ok(Self { nets })
    }

    /// `AVALON_TRUSTED_PROXIES`; unset or empty trusts no proxy. An
    /// unparseable value aborts startup rather than silently trusting less
    /// or more than the operator intended.
    pub fn from_env() -> Self {
        match std::env::var("AVALON_TRUSTED_PROXIES") {
            Ok(list) => Self::parse(&list)
                .unwrap_or_else(|e| panic!("AVALON_TRUSTED_PROXIES is invalid: {e}")),
            Err(_) => Self::default(),
        }
    }

    fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        self.nets.iter().any(|net| {
            net.contains(&ip)
                || matches!(net, IpNet::V6(_)) && ip.is_ipv4() && net.contains(&to_mapped(ip))
        })
    }

    /// The rate-limit key address: the right-most `X-Forwarded-For` entry that
    /// is not itself a trusted proxy when `peer` is trusted, otherwise `peer`.
    /// Always in canonical form (IPv4-mapped IPv6 becomes IPv4).
    pub fn client_ip(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        let peer = peer.to_canonical();
        if !self.contains(peer) {
            return peer;
        }
        let mut entries = Vec::new();
        for value in headers.get_all(FORWARDED_FOR) {
            let Ok(value) = value.to_str() else {
                return peer;
            };
            entries.extend(value.split(','));
        }
        for entry in entries.into_iter().rev() {
            let Some(ip) = parse_forwarded_entry(entry) else {
                return peer;
            };
            if !self.contains(ip) {
                return ip.to_canonical();
            }
        }
        peer
    }
}

fn to_mapped(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => IpAddr::V6(v4.to_ipv6_mapped()),
        v6 => v6,
    }
}

fn parse_forwarded_entry(entry: &str) -> Option<IpAddr> {
    let entry = entry.trim();
    entry
        .parse::<IpAddr>()
        .ok()
        .or_else(|| entry.parse::<SocketAddr>().ok().map(|a| a.ip()))
        .or_else(|| {
            entry
                .strip_prefix('[')
                .and_then(|e| e.strip_suffix(']'))
                .and_then(|e| e.parse::<IpAddr>().ok())
        })
}

/// `tower_governor` key extractor for the per-IP ceiling. Requires the server
/// to be served with `into_make_service_with_connect_info::<SocketAddr>()`.
#[derive(Debug, Clone)]
pub(crate) struct ClientIpKeyExtractor {
    pub(crate) proxies: Arc<TrustedProxies>,
}

impl KeyExtractor for ClientIpKeyExtractor {
    type Key = IpAddr;

    fn extract<T>(&self, req: &Request<T>) -> Result<Self::Key, GovernorError> {
        req.extensions()
            .get::<axum::extract::ConnectInfo<SocketAddr>>()
            .map(|axum::extract::ConnectInfo(addr)| {
                self.proxies.client_ip(addr.ip(), req.headers())
            })
            .ok_or(GovernorError::UnableToExtractKey)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(xff: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in xff {
            h.append(FORWARDED_FOR, v.parse().unwrap());
        }
        h
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn proxies(list: &str) -> TrustedProxies {
        TrustedProxies::parse(list).unwrap()
    }

    #[test]
    fn untrusted_peer_ignores_the_header() {
        let p = proxies("10.0.0.1");
        assert_eq!(
            p.client_ip(ip("203.0.113.9"), &headers(&["1.2.3.4"])),
            ip("203.0.113.9")
        );
    }

    #[test]
    fn nothing_trusted_by_default() {
        assert_eq!(
            TrustedProxies::default().client_ip(ip("10.0.0.1"), &headers(&["1.2.3.4"])),
            ip("10.0.0.1")
        );
    }

    #[test]
    fn trusted_peer_yields_rightmost_untrusted_entry() {
        let p = proxies("10.0.0.0/8");
        // A client-prepended forged entry sits left of the real one.
        let h = headers(&["6.6.6.6, 198.51.100.7"]);
        assert_eq!(p.client_ip(ip("10.0.0.1"), &h), ip("198.51.100.7"));
    }

    #[test]
    fn chain_with_trusted_hops_resolves_past_them() {
        let p = proxies("10.0.0.0/8, 192.168.1.1");
        let h = headers(&["6.6.6.6", "198.51.100.7, 10.2.2.2, 192.168.1.1"]);
        assert_eq!(p.client_ip(ip("10.0.0.1"), &h), ip("198.51.100.7"));
    }

    #[test]
    fn missing_or_malformed_header_falls_back_to_peer() {
        let p = proxies("10.0.0.1");
        assert_eq!(
            p.client_ip(ip("10.0.0.1"), &HeaderMap::new()),
            ip("10.0.0.1")
        );
        assert_eq!(
            p.client_ip(ip("10.0.0.1"), &headers(&["not-an-ip"])),
            ip("10.0.0.1")
        );
        assert_eq!(
            p.client_ip(ip("10.0.0.1"), &headers(&["1.2.3.4, garbage"])),
            ip("10.0.0.1")
        );
        assert_eq!(
            p.client_ip(ip("10.0.0.1"), &headers(&["1.2.3.4,"])),
            ip("10.0.0.1")
        );
    }

    #[test]
    fn all_trusted_chain_falls_back_to_peer() {
        let p = proxies("10.0.0.0/8");
        assert_eq!(
            p.client_ip(ip("10.0.0.1"), &headers(&["10.1.1.1, 10.2.2.2"])),
            ip("10.0.0.1")
        );
    }

    #[test]
    fn mapped_and_plain_forms_share_one_key() {
        let p = proxies("10.0.0.1");
        assert_eq!(
            p.client_ip(ip("::ffff:203.0.113.9"), &HeaderMap::new()),
            ip("203.0.113.9")
        );
        // A mapped peer matches a plain IPv4 trusted entry, and a mapped
        // forwarded entry keys as its IPv4 form.
        assert_eq!(
            p.client_ip(ip("::ffff:10.0.0.1"), &headers(&["::ffff:198.51.100.7"])),
            ip("198.51.100.7")
        );
    }

    #[test]
    fn ipv6_clients_and_ports_are_parsed() {
        let p = proxies("10.0.0.1");
        assert_eq!(
            p.client_ip(ip("10.0.0.1"), &headers(&["2001:db8::1"])),
            ip("2001:db8::1")
        );
        assert_eq!(
            p.client_ip(ip("10.0.0.1"), &headers(&["198.51.100.7:4711"])),
            ip("198.51.100.7")
        );
    }

    #[test]
    fn invalid_config_is_rejected() {
        assert!(TrustedProxies::parse("10.0.0.1, nope").is_err());
        assert_eq!(
            TrustedProxies::parse("").unwrap(),
            TrustedProxies::default()
        );
    }
}
