//! Target validation: the generator only talks to loopback addresses, so a
//! misconfigured run can never reach a shared node.

use std::net::IpAddr;

use anyhow::{bail, Context, Result};
use url::{Host, Url};

/// Setting this to `1` allows non-loopback targets.
pub const OVERRIDE_ENV: &str = "LOADTEST_ALLOW_NON_LOOPBACK";

fn host_is_loopback(host: &Host<&str>) -> bool {
    match host {
        Host::Ipv4(ip) => ip.is_loopback(),
        Host::Ipv6(ip) => ip.is_loopback(),
        Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
    }
}

/// Parses `raw` and refuses any host that is not a loopback literal or
/// `localhost`, unless `allow_non_loopback` is set.
pub fn check_target(raw: &str, allow_non_loopback: bool) -> Result<Url> {
    let url = Url::parse(raw).with_context(|| format!("invalid target url: {raw}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("unsupported target scheme: {}", url.scheme());
    }
    let host = url.host().context("target url has no host")?;
    if !allow_non_loopback && !host_is_loopback(&host) {
        bail!(
            "refusing non-loopback target {raw}; the harness only targets nodes it started \
             (set {OVERRIDE_ENV}=1 to override)"
        );
    }
    Ok(url)
}

pub fn override_enabled() -> bool {
    std::env::var(OVERRIDE_ENV).is_ok_and(|v| v == "1")
}

/// A loopback address other than 127.0.0.1, used to act as a second client
/// address against a per-IP limiter.
pub fn alt_loopback(n: u8) -> IpAddr {
    IpAddr::from([127, 0, 0, n.max(2)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_targets_are_accepted() {
        for t in [
            "http://127.0.0.1:19900",
            "http://127.0.0.7:8080/x",
            "http://localhost:8080",
            "http://[::1]:8080",
        ] {
            assert!(check_target(t, false).is_ok(), "{t}");
        }
    }

    #[test]
    fn non_loopback_targets_are_refused() {
        for t in [
            "http://192.168.7.113:8080",
            "http://10.0.0.1",
            "http://example.com",
            "http://127.0.0.1.example.com",
            "http://0.0.0.0:8080",
            "http://[::ffff:192.168.1.1]:80",
        ] {
            assert!(check_target(t, false).is_err(), "{t}");
        }
    }

    #[test]
    fn override_allows_non_loopback() {
        assert!(check_target("http://192.168.7.113:8080", true).is_ok());
    }

    #[test]
    fn bad_scheme_and_garbage_are_refused() {
        assert!(check_target("ftp://127.0.0.1", true).is_err());
        assert!(check_target("not a url", true).is_err());
    }

    #[test]
    fn alt_loopback_stays_in_127_slash_8() {
        assert!(alt_loopback(2).is_loopback());
        assert_eq!(alt_loopback(0), alt_loopback(2));
    }
}
