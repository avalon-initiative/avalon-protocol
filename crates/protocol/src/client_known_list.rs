//! Client-side known-list rules: the diversity prefix derived from a node's URL and the
//! deterministic selection of a bounded list from an ordered candidate sequence. Pure logic,
//! shared verbatim by every SDK through `conformance/vectors/known-list-selection.json`.
//!
//! A client cannot resolve DNS (a browser SDK has no such API), so the prefix comes from the
//! URL's host text alone. Many domains that point at one machine therefore look diverse; the
//! bundled anchors (`docs/trusted-networks.json` `seed_nodes`) are the floor against that.

use std::net::{Ipv4Addr, Ipv6Addr};

use crate::known_list::{KnownList, WitnessSlot};

/// Default capacity of a client's known list.
pub const DEFAULT_CAPACITY: usize = 5;
/// Default number of slots reserved for anchors.
pub const DEFAULT_ANCHOR_CAPACITY: usize = 2;
/// Default cap on slots sharing one diversity prefix.
pub const DEFAULT_MAX_PER_PREFIX: usize = 2;

/// One discovered witness, keyed by the witness key its advert proved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub witness_key_id: String,
    pub base_url: String,
    pub is_anchor: bool,
}

fn valid_port(port: &str) -> bool {
    !port.is_empty()
        && port.len() <= 5
        && port.bytes().all(|b| b.is_ascii_digit())
        && port.parse::<u32>().is_ok_and(|p| (1..=65535).contains(&p))
}

fn valid_hostname(host: &str) -> bool {
    !host.is_empty()
        && host.split('.').all(|label| {
            let bytes = label.as_bytes();
            !bytes.is_empty()
                && bytes
                    .iter()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
                && bytes[0] != b'-'
                && bytes[bytes.len() - 1] != b'-'
        })
}

/// The diversity prefix for `base_url`, or `None` when the URL is not an acceptable node URL.
///
/// Accepted shape: `http://` or `https://` (any letter case), an authority, an optional
/// `:port` (1 to 65535) and an optional single trailing `/`; no userinfo, path, query or
/// fragment. The host is lowercased and a trailing dot removed, then:
/// - dotted-quad IPv4 (no leading zeros) gives `v4:a.b.c.0/24`;
/// - bracketed IPv6 (no embedded dotted quad) gives `v6:g1:g2:g3::/48` with the first three
///   16-bit groups in lowercase hex without leading zeros;
/// - a hostname of two or more labels gives `host:` plus its last two labels;
/// - a single-label hostname gives `host:` plus that label.
///
/// Hostnames must be lowercase ASCII letters, digits and hyphens (internationalized names must
/// be given in punycode).
pub fn diversity_prefix_for_url(base_url: &str) -> Option<String> {
    if !base_url.is_ascii()
        || base_url
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return None;
    }
    let lower = base_url.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))?;
    let authority = match rest.find(['/', '?', '#']) {
        Some(i) => {
            if &rest[i..] != "/" {
                return None;
            }
            &rest[..i]
        }
        None => rest,
    };
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let (host, port) = if let Some(after) = authority.strip_prefix('[') {
        let close = after.find(']')?;
        let host = &after[..close];
        let tail = &after[close + 1..];
        let port = match tail {
            "" => None,
            t => Some(t.strip_prefix(':')?),
        };
        (format!("[{host}]"), port)
    } else {
        let mut parts = authority.split(':');
        let host = parts.next()?;
        let port = parts.next();
        if parts.next().is_some() {
            return None;
        }
        (host.to_string(), port)
    };
    if let Some(port) = port {
        if !valid_port(port) {
            return None;
        }
    }

    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        if inner.contains('.') {
            return None;
        }
        let addr: Ipv6Addr = inner.parse().ok()?;
        let s = addr.segments();
        return Some(format!("v6:{:x}:{:x}:{:x}::/48", s[0], s[1], s[2]));
    }

    let host = host.strip_suffix('.').unwrap_or(&host).to_string();
    if host.bytes().all(|b| b.is_ascii_digit() || b == b'.') && host.contains('.') {
        let addr: Ipv4Addr = host.parse().ok()?;
        let o = addr.octets();
        return Some(format!("v4:{}.{}.{}.0/24", o[0], o[1], o[2]));
    }
    if !valid_hostname(&host) {
        return None;
    }
    let labels: Vec<&str> = host.split('.').collect();
    let keep = if labels.len() >= 2 {
        &labels[labels.len() - 2..]
    } else {
        &labels[..]
    };
    Some(format!("host:{}", keep.join(".")))
}

/// Builds a known list from `candidates`, returning the admitted witness key ids in admission
/// order. Anchors are considered first, in the order given, then the rest in the order given.
/// A candidate whose URL has no diversity prefix is skipped. Admission follows
/// [`KnownList::try_admit`]: already present, anchor cap, capacity, then the per-prefix cap.
///
/// The order of non-anchor candidates is the caller's to randomize: a fixed order such as key
/// id order would let an operator grind keys that always sort first.
pub fn select_known_list(
    candidates: &[Candidate],
    capacity: usize,
    anchor_capacity: usize,
    max_per_prefix: usize,
) -> Vec<String> {
    let mut list = KnownList::new(capacity, anchor_capacity.min(capacity), max_per_prefix);
    for pass_anchor in [true, false] {
        for candidate in candidates.iter().filter(|c| c.is_anchor == pass_anchor) {
            let Some(prefix) = diversity_prefix_for_url(&candidate.base_url) else {
                continue;
            };
            list.try_admit(WitnessSlot {
                witness_key_id: candidate.witness_key_id.clone(),
                prefix,
                is_anchor: candidate.is_anchor,
            });
        }
    }
    list.witness_key_ids()
        .into_iter()
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_covers_the_documented_shapes() {
        assert_eq!(
            diversity_prefix_for_url("http://192.168.7.174:8080").as_deref(),
            Some("v4:192.168.7.0/24")
        );
        assert_eq!(
            diversity_prefix_for_url("HTTPS://Node.Example.COM/").as_deref(),
            Some("host:example.com")
        );
        assert_eq!(
            diversity_prefix_for_url("http://[2001:DB8:1:2::1]:8080").as_deref(),
            Some("v6:2001:db8:1::/48")
        );
        assert_eq!(
            diversity_prefix_for_url("http://localhost:8080").as_deref(),
            Some("host:localhost")
        );
        assert_eq!(diversity_prefix_for_url("ftp://a.example"), None);
        assert_eq!(diversity_prefix_for_url("http://a.example/path"), None);
        assert_eq!(diversity_prefix_for_url("http://u@a.example"), None);
        assert_eq!(diversity_prefix_for_url("http://010.0.0.1"), None);
    }

    #[test]
    fn selection_applies_anchor_capacity_and_prefix_cap() {
        let c = |id: &str, url: &str, anchor: bool| Candidate {
            witness_key_id: id.to_string(),
            base_url: url.to_string(),
            is_anchor: anchor,
        };
        let candidates = vec![
            c("n1", "http://a.example.com", false),
            c("n2", "http://b.example.com", false),
            c("n3", "http://c.example.com", false),
            c("a1", "http://10.0.0.1:1", true),
        ];
        assert_eq!(
            select_known_list(&candidates, 5, 2, 2),
            vec!["a1", "n1", "n2"]
        );
    }
}
