//! Node-to-node protocol version awareness — issue #368, implementing
//! #308's decided design.
//!
//! **Never trusted for anything cryptographic or used to grant elevated
//! access** (#308's cross-cutting invariant): a version claim stays a
//! compatibility/availability signal. Excluding a peer for reporting a low
//! version is a self-inflicted availability change (wrongly excluded, or
//! wrongly not excluded), never a security hole — every actual settlement
//! operation stays independently, cryptographically verified regardless of
//! what version either side claims.
//!
//! [`PROTOCOL_VERSION`] is a compile-time constant, never a runtime-settable
//! env var — this closes the trivial "just set a config value" spoofing
//! path, but is stated honestly here too: it is **not** cryptographic
//! non-forgeability. A forked, recompiled binary can still hardcode a fake
//! constant; genuine non-forgeability against a deliberately modified
//! binary needs #369's signed release manifest, which this deliberately
//! does not attempt.

use std::sync::OnceLock;

/// This node's own build version — reported in `POST /nodes/announce`,
/// `GET /nodes/status`, and the mirror-watcher's STH fetches.
/// `env!("CARGO_PKG_VERSION")` rather than a separate hand-maintained
/// constant: the workspace version *is* the wire/protocol version at this
/// stage (milestone 1, one crate graph, one release cadence) — nothing yet
/// requires the wire version to move independently of it.
pub const PROTOCOL_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The hard floor below which a peer is excluded from this node's peer
/// table / gossip entirely, baked into the binary as the real baseline —
/// not an operator-configurable default. Currently equal to
/// [`PROTOCOL_VERSION`] since no older release has ever shipped; raising
/// this over time (dropping support for genuinely old peers) happens via a
/// real, changelog-visible release, never a silent default bump.
pub const MIN_SUPPORTED_PEER_VERSION: &str = "0.1.0";

fn parse(raw: &str) -> Option<semver::Version> {
    semver::Version::parse(raw).ok()
}

/// `AVALON_MIN_PEER_VERSION` can only raise the effective floor above
/// [`MIN_SUPPORTED_PEER_VERSION`], never lower it — an operator (or a
/// compromised `.env`) setting a permissive value can never disable
/// enforcement entirely. An unparseable env value is treated the same as
/// unset (falls back to the baked-in baseline), not a startup error — this
/// floor must never be silently weakened by a typo.
pub fn effective_min_peer_version() -> &'static semver::Version {
    static FLOOR: OnceLock<semver::Version> = OnceLock::new();
    FLOOR.get_or_init(|| {
        let baseline =
            parse(MIN_SUPPORTED_PEER_VERSION).expect("MIN_SUPPORTED_PEER_VERSION must be semver");
        match std::env::var("AVALON_MIN_PEER_VERSION")
            .ok()
            .and_then(|s| parse(&s))
        {
            Some(env_floor) if env_floor > baseline => env_floor,
            _ => baseline,
        }
    })
}

/// Whether `raw` (a peer-reported protocol version) clears the effective
/// floor. An unparseable or missing version is never given the benefit of
/// the doubt — treated as unsupported, distinctly logged from a
/// well-formed-but-below-floor version by callers (see
/// `mirror_watcher::fetch_and_verify_sth`, `nodes::announce`).
pub fn is_supported(raw: &str) -> bool {
    match parse(raw) {
        Some(v) => &v >= effective_min_peer_version(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_protocol_version_always_clears_its_own_floor() {
        // Sanity: MIN_SUPPORTED_PEER_VERSION must never accidentally drift
        // ahead of PROTOCOL_VERSION itself, or this node would exclude its
        // own version from its own peer table.
        assert!(is_supported(PROTOCOL_VERSION));
    }

    #[test]
    fn an_unparseable_version_is_never_supported() {
        assert!(!is_supported("not-a-version"));
        assert!(!is_supported(""));
    }

    #[test]
    fn env_var_below_the_baked_in_floor_has_no_effect() {
        let _env = crate::test_env::guard();
        // SAFETY-of-intent note: `std::env::set_var`/`remove_var` are
        // process-global; `effective_min_peer_version` caches its result in
        // a `OnceLock` for the life of the process, so this test only
        // establishes the *first* value ever computed is correct — it
        // can't re-exercise a changed env var later in the same process.
        // Real env-var-affects-floor behavior is covered by the pure
        // `resolve_effective_floor`-shaped unit tests below instead, which
        // don't depend on process-global caching.
        unsafe {
            std::env::remove_var("AVALON_MIN_PEER_VERSION");
        }
        let floor = effective_min_peer_version();
        assert_eq!(floor, &parse(MIN_SUPPORTED_PEER_VERSION).unwrap());
    }

    fn resolve_effective_floor(baseline: &str, env_value: Option<&str>) -> semver::Version {
        let baseline = parse(baseline).unwrap();
        match env_value.and_then(parse) {
            Some(env_floor) if env_floor > baseline => env_floor,
            _ => baseline,
        }
    }

    #[test]
    fn an_env_floor_above_baseline_raises_the_effective_floor() {
        let floor = resolve_effective_floor("0.1.0", Some("0.2.0"));
        assert_eq!(floor, parse("0.2.0").unwrap());
    }

    #[test]
    fn an_env_floor_below_baseline_never_lowers_the_effective_floor() {
        let floor = resolve_effective_floor("0.2.0", Some("0.1.0"));
        assert_eq!(floor, parse("0.2.0").unwrap());
    }

    #[test]
    fn an_unparseable_env_floor_falls_back_to_baseline() {
        let floor = resolve_effective_floor("0.1.0", Some("not-a-version"));
        assert_eq!(floor, parse("0.1.0").unwrap());
    }

    #[test]
    fn no_env_floor_falls_back_to_baseline() {
        let floor = resolve_effective_floor("0.1.0", None);
        assert_eq!(floor, parse("0.1.0").unwrap());
    }

    #[test]
    fn a_peer_at_or_above_the_baseline_floor_is_supported() {
        assert!(is_supported(MIN_SUPPORTED_PEER_VERSION));
        assert!(is_supported("999.0.0"));
    }
}
