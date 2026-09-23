//! Startup check that a node authoring the reserved `core` shard does so with
//! the network's pinned settlement key.
//!
//! A node whose `AVALON_OWN_SHARD_ID` is `core` and whose signing key differs
//! from the key pinned for its network in `docs/trusted-networks.json` produces
//! tree heads that clients pinned to that network report as a mismatch.

use std::sync::OnceLock;

use avalon_protocol::network_trust::{NetworkEnvironment, TrustAnchorEntry};
use avalon_protocol::shard::CORE_SHARD_ID;

/// Everything the decision depends on, with no environment access.
#[derive(Debug, Clone)]
pub struct CoreAuthorInputs<'a> {
    pub own_shard_id: &'a str,
    pub network_id: &'a str,
    /// Lowercase hex of this node's settlement verify key.
    pub node_verify_key_hex: &'a str,
    pub node_signing_key_id: &'a str,
    /// `AVALON_BOOTSTRAP_PEERS` or `AVALON_MIRROR_PEERS` names at least one peer.
    pub peers_configured: bool,
}

/// What startup should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreAuthorDecision {
    /// This node does not author `core`, or its network has no pinned anchor.
    NotApplicable,
    /// This node's key equals the pinned key.
    Pinned,
    /// Key differs; continue but warn.
    WarnUnpinned(String),
    /// Key differs; refuse to start with this message.
    Refuse(String),
}

impl CoreAuthorDecision {
    /// Value reported as `core_author_pinned` in `GET /nodes/status`.
    pub fn pinned(&self) -> Option<bool> {
        match self {
            CoreAuthorDecision::NotApplicable => None,
            CoreAuthorDecision::Pinned => Some(true),
            CoreAuthorDecision::WarnUnpinned(_) | CoreAuthorDecision::Refuse(_) => Some(false),
        }
    }
}

fn mismatch_message(inputs: &CoreAuthorInputs<'_>, anchor: &TrustAnchorEntry) -> String {
    format!(
        "this node authors the reserved `core` shard on network `{network}` but its settlement \
         key does not match the key pinned for that network. Pinned key: id `{pinned_id}`, \
         verify key {pinned_key}. This node's key: id `{node_id}`, verify key {node_key}. \
         Clients pinned to `{network}` will report this node's tree heads as a key mismatch. \
         Fix by either (1) setting AVALON_SETTLEMENT_SIGNING_KEY to the pinned key, if this node \
         IS the network's core authority, or (2) authoring a named shard: set \
         AVALON_OWN_SHARD_ID to a registered shard (for example `game:<integrator-slug>`) and \
         use that shard's registered shard_settlement key",
        network = inputs.network_id,
        pinned_id = anchor.signing_key_id,
        pinned_key = anchor.verify_key,
        node_id = inputs.node_signing_key_id,
        node_key = inputs.node_verify_key_hex,
    )
}

/// Decides whether a node may start as a `core` author with the given key.
pub fn evaluate(inputs: &CoreAuthorInputs<'_>, anchors: &[TrustAnchorEntry]) -> CoreAuthorDecision {
    if inputs.own_shard_id != CORE_SHARD_ID {
        return CoreAuthorDecision::NotApplicable;
    }
    let Some(anchor) = anchors.iter().find(|a| a.network_id == inputs.network_id) else {
        return CoreAuthorDecision::NotApplicable;
    };
    if anchor
        .verify_key
        .eq_ignore_ascii_case(inputs.node_verify_key_hex)
    {
        return CoreAuthorDecision::Pinned;
    }
    let message = mismatch_message(inputs, anchor);
    if anchor.environment != NetworkEnvironment::LocalDev || inputs.peers_configured {
        CoreAuthorDecision::Refuse(message)
    } else {
        CoreAuthorDecision::WarnUnpinned(message)
    }
}

static OUTCOME: OnceLock<Option<bool>> = OnceLock::new();

/// Records the startup outcome for `GET /nodes/status`.
pub fn record_outcome(decision: &CoreAuthorDecision) {
    let _ = OUTCOME.set(decision.pinned());
}

/// The recorded `core_author_pinned` value; `None` when not a core author
/// with a pinned anchor, or before startup recorded it.
pub fn recorded_outcome() -> Option<bool> {
    OUTCOME.get().copied().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(environment: NetworkEnvironment) -> TrustAnchorEntry {
        TrustAnchorEntry {
            label: "t".to_string(),
            network_id: "net-1".to_string(),
            verify_key: "aa".repeat(32),
            signing_key_id: "pinned-1".to_string(),
            server_url: None,
            environment,
            seed_nodes: vec![],
            notes: None,
        }
    }

    fn inputs<'a>(shard: &'a str, key: &'a str, peers: bool) -> CoreAuthorInputs<'a> {
        CoreAuthorInputs {
            own_shard_id: shard,
            network_id: "net-1",
            node_verify_key_hex: key,
            node_signing_key_id: "node-key",
            peers_configured: peers,
        }
    }

    #[test]
    fn matching_key_is_pinned() {
        let key = "aa".repeat(32);
        for env in [NetworkEnvironment::LocalDev, NetworkEnvironment::Prod] {
            let d = evaluate(&inputs("core", &key, true), &[anchor(env)]);
            assert_eq!(d, CoreAuthorDecision::Pinned);
            assert_eq!(d.pinned(), Some(true));
        }
    }

    #[test]
    fn matching_key_comparison_ignores_hex_case() {
        let key = "AA".repeat(32);
        let d = evaluate(
            &inputs("core", &key, false),
            &[anchor(NetworkEnvironment::Prod)],
        );
        assert_eq!(d, CoreAuthorDecision::Pinned);
    }

    #[test]
    fn mismatch_on_non_local_dev_refuses_even_without_peers() {
        let key = "bb".repeat(32);
        for env in [
            NetworkEnvironment::Dev,
            NetworkEnvironment::Int,
            NetworkEnvironment::Prod,
        ] {
            let d = evaluate(&inputs("core", &key, false), &[anchor(env)]);
            let CoreAuthorDecision::Refuse(msg) = &d else {
                panic!("expected refuse, got {d:?}");
            };
            assert!(msg.contains("net-1"));
            assert!(msg.contains("pinned-1"));
            assert!(msg.contains("node-key"));
            assert!(msg.contains("AVALON_OWN_SHARD_ID"));
            assert!(msg.contains("AVALON_SETTLEMENT_SIGNING_KEY"));
            assert_eq!(d.pinned(), Some(false));
        }
    }

    #[test]
    fn mismatch_on_local_dev_with_peers_refuses() {
        let key = "bb".repeat(32);
        let d = evaluate(
            &inputs("core", &key, true),
            &[anchor(NetworkEnvironment::LocalDev)],
        );
        assert!(matches!(d, CoreAuthorDecision::Refuse(_)));
    }

    #[test]
    fn mismatch_on_lone_local_dev_only_warns() {
        let key = "bb".repeat(32);
        let d = evaluate(
            &inputs("core", &key, false),
            &[anchor(NetworkEnvironment::LocalDev)],
        );
        assert!(matches!(d, CoreAuthorDecision::WarnUnpinned(_)));
        assert_eq!(d.pinned(), Some(false));
    }

    #[test]
    fn network_without_anchor_is_not_applicable() {
        let key = "bb".repeat(32);
        let mut a = anchor(NetworkEnvironment::Prod);
        a.network_id = "other".to_string();
        let d = evaluate(&inputs("core", &key, true), &[a]);
        assert_eq!(d, CoreAuthorDecision::NotApplicable);
        assert_eq!(d.pinned(), None);
    }

    #[test]
    fn named_shard_is_not_applicable() {
        let key = "bb".repeat(32);
        let d = evaluate(
            &inputs("game:wow/2", &key, true),
            &[anchor(NetworkEnvironment::Prod)],
        );
        assert_eq!(d, CoreAuthorDecision::NotApplicable);
    }
}
