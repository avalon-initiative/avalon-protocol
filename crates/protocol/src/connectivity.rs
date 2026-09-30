//! Node connectivity states, pure types with no I/O. Connectivity describes how a node
//! can be reached; it is never authority, a role, a capability or trust (see `nodes.md`).

use serde::{Deserialize, Serialize};

/// How a node is reachable, listed in preference order (most preferred first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Connectivity {
    /// Peers connect straight to a routable address of this node.
    Direct,
    /// Peers reach this node through a hole-punched direct connection.
    NatTraversed,
    /// Peers reach this node through a relay that only carries bytes.
    Relayed,
    /// This node only dials out; peers never open a connection to it.
    OutboundOnly,
}

impl Connectivity {
    /// All states, most preferred first.
    pub const PREFERENCE_ORDER: [Connectivity; 4] = [
        Connectivity::Direct,
        Connectivity::NatTraversed,
        Connectivity::Relayed,
        Connectivity::OutboundOnly,
    ];

    /// Preference rank, 0 is most preferred.
    pub fn rank(self) -> u8 {
        match self {
            Connectivity::Direct => 0,
            Connectivity::NatTraversed => 1,
            Connectivity::Relayed => 2,
            Connectivity::OutboundOnly => 3,
        }
    }

    /// The more preferred of two states.
    pub fn best(self, other: Connectivity) -> Connectivity {
        if other.rank() < self.rank() {
            other
        } else {
            self
        }
    }
}

/// The kind of path a measurement was taken over. A relayed measurement is never reported as
/// direct: it includes the relay's hop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PathType {
    /// A direct connection to a routable address.
    Direct,
    /// A direct connection established by hole punching.
    Traversed,
    /// A connection through a relay.
    Relayed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_are_snake_case_and_round_trip() {
        for (state, name) in [
            (Connectivity::Direct, "direct"),
            (Connectivity::NatTraversed, "nat_traversed"),
            (Connectivity::Relayed, "relayed"),
            (Connectivity::OutboundOnly, "outbound_only"),
        ] {
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(json, format!("\"{name}\""));
            assert_eq!(serde_json::from_str::<Connectivity>(&json).unwrap(), state);
        }
    }

    #[test]
    fn path_types_use_snake_case_wire_names() {
        for (path, name) in [
            (PathType::Direct, "direct"),
            (PathType::Traversed, "traversed"),
            (PathType::Relayed, "relayed"),
        ] {
            assert_eq!(serde_json::to_string(&path).unwrap(), format!("\"{name}\""));
        }
    }

    #[test]
    fn unknown_state_is_rejected() {
        assert!(serde_json::from_str::<Connectivity>("\"tunnelled\"").is_err());
    }

    #[test]
    fn preference_order_matches_rank() {
        for (i, state) in Connectivity::PREFERENCE_ORDER.iter().enumerate() {
            assert_eq!(state.rank() as usize, i);
        }
        assert_eq!(
            Connectivity::Relayed.best(Connectivity::NatTraversed),
            Connectivity::NatTraversed
        );
        assert_eq!(
            Connectivity::Direct.best(Connectivity::OutboundOnly),
            Connectivity::Direct
        );
    }

    #[test]
    fn embeds_additively_in_a_struct_that_lacks_the_field() {
        #[derive(Deserialize)]
        struct Status {
            #[serde(default)]
            connectivity: Option<Connectivity>,
        }
        let old: Status = serde_json::from_str("{}").unwrap();
        assert_eq!(old.connectivity, None);
        let new: Status = serde_json::from_str(r#"{"connectivity":"relayed"}"#).unwrap();
        assert_eq!(new.connectivity, Some(Connectivity::Relayed));
    }
}
