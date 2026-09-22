//! Bundled trust-anchor data — the canonical `docs/trusted-networks.json`
//! list, embedded and parsed for `crates/server`'s own use (bootstrap-peer
//! resolution, cross-node-login anchor checks, mirror-watcher STH
//! verification).
//!
//! This is a deliberate, independent duplicate of `crates/sdk/src/network.rs`'s
//! own copy of the same shape (found via #780, filed while executing epic
//! #771's #775): `crates/sdk` is decoupled from every in-workspace crate as
//! of #774, including this one, so it cannot depend on `avalon-protocol` for
//! this. Both copies parse the same `docs/trusted-networks.json` — the Rust
//! SDK's copy travels with it once #775 physically moves `crates/sdk` out of
//! this workspace, vendored and kept in sync by hand, the same convention
//! the C#/TS SDKs' own generated files already use.

use std::sync::OnceLock;

use serde::Deserialize;

const TRUSTED_NETWORKS_JSON: &str = include_str!("../../../docs/trusted-networks.json");

/// Which deployment tier a [`TrustAnchorEntry`] pins, matching
/// `docs/trusted-networks.json`'s `environment` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkEnvironment {
    /// No real deployment — a freely-generated key checked in only to
    /// exercise the trust-anchor mechanism end to end.
    LocalDev,
    /// A real, non-production, single-node deployment.
    Dev,
    /// A real, non-production, 1-5 node interconnected test bed used to
    /// verify changes actually integrate across nodes before mainnet.
    Int,
    /// A real mainnet deployment.
    Prod,
}

/// One entry of `docs/trusted-networks.json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TrustAnchorEntry {
    /// Human-readable name for the network.
    pub label: String,
    /// The exact string a server sets `AVALON_NETWORK_ID` to.
    pub network_id: String,
    /// Lowercase hex-encoded Ed25519 public key — the settlement
    /// operator's STH verify key for this network (`crates/protocol/src/sth.rs`).
    pub verify_key: String,
    /// Which key generation this is, matching `SignedTreeHead::signing_key_id`.
    pub signing_key_id: String,
    /// The server URL this network is reachable at, if published.
    #[serde(default)]
    pub server_url: Option<String>,
    /// Which deployment tier this is.
    pub environment: NetworkEnvironment,
    /// Base URLs of this network's always-on anchor node(s) — the default
    /// bootstrap peers a node configured for this `network_id` announces to
    /// when it has no `AVALON_BOOTSTRAP_PEERS` of its own set.
    #[serde(default)]
    pub seed_nodes: Vec<String>,
    /// Free-text notes.
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TrustedNetworksFile {
    networks: Vec<TrustAnchorEntry>,
}

/// Every network this build was bundled with a pinned key for — parsed once
/// from the embedded `docs/trusted-networks.json` and cached.
pub fn bundled_trust_anchors() -> &'static [TrustAnchorEntry] {
    static ANCHORS: OnceLock<Vec<TrustAnchorEntry>> = OnceLock::new();
    ANCHORS
        .get_or_init(|| {
            let file: TrustedNetworksFile = serde_json::from_str(TRUSTED_NETWORKS_JSON).expect(
                "docs/trusted-networks.json must be valid JSON matching TrustedNetworksFile",
            );
            file.networks
        })
        .as_slice()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_trust_anchors_parses_the_real_checked_in_file() {
        let anchors = bundled_trust_anchors();
        assert!(anchors.iter().any(|a| a.network_id == "avalon-dev-local"));
    }
}
