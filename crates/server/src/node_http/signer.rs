//! Signing of the node-to-node write routes with this node's libp2p identity key.
//!
//! Only an HTTP attempt carries the credential; a libp2p stream is already authenticated by
//! its handshake and never carries one. Each attempt is signed afresh with its own nonce.

use std::sync::Arc;

use avalon_protocol::node_request::{
    encode_node_request_header, sign_node_request, NodeRequestError, NodeRequestTarget,
};
use ed25519_dalek::SigningKey;
use libp2p::identity;
use libp2p::PeerId;
use rand::Rng;

/// Signs requests as this node.
#[derive(Clone)]
pub struct NodeSigner {
    key: Arc<SigningKey>,
    peer_id: String,
    network_id: String,
}

impl NodeSigner {
    /// A signer for `identity`; `None` when the key is not Ed25519 or the network id is empty.
    pub fn new(identity: &identity::Keypair, network_id: &str) -> Option<Self> {
        let ed = identity.clone().try_into_ed25519().ok()?;
        let seed: [u8; 32] = ed.secret().as_ref().try_into().ok()?;
        (!network_id.is_empty()).then(|| Self {
            key: Arc::new(SigningKey::from_bytes(&seed)),
            peer_id: PeerId::from(identity.public()).to_string(),
            network_id: network_id.to_string(),
        })
    }

    pub fn peer_id(&self) -> &str {
        &self.peer_id
    }

    /// The header value for one HTTP attempt at `path` for `recipient`, with a fresh nonce.
    pub fn header(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        recipient: &str,
    ) -> Result<String, NodeRequestError> {
        let mut nonce = [0u8; 16];
        rand::rng().fill_bytes(&mut nonce);
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let target = NodeRequestTarget {
            method,
            path,
            body,
            network_id: &self.network_id,
        };
        let auth = sign_node_request(&self.key, &self.peer_id, &target, recipient, now, nonce)?;
        Ok(encode_node_request_header(&auth))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use avalon_protocol::node_request::verify_node_request_header;

    #[test]
    fn a_header_verifies_under_the_nodes_own_peer_id_and_never_repeats_a_nonce() {
        let key = identity::Keypair::generate_ed25519();
        let signer = NodeSigner::new(&key, "net").unwrap();
        assert_eq!(signer.peer_id(), PeerId::from(key.public()).to_string());
        let target = NodeRequestTarget {
            method: "POST",
            path: "/nodes/relay",
            body: b"{}",
            network_id: "net",
        };
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let a = signer
            .header("POST", "/nodes/relay", b"{}", "rcpt")
            .unwrap();
        let b = signer
            .header("POST", "/nodes/relay", b"{}", "rcpt")
            .unwrap();
        assert_ne!(a, b);
        let auth = verify_node_request_header(&a, &target, &["rcpt"], now, 60).unwrap();
        assert_eq!(auth.peer_id, signer.peer_id());
        let other = NodeRequestTarget {
            body: b"{ }",
            ..target
        };
        assert!(verify_node_request_header(&a, &other, &["rcpt"], now, 60).is_err());
    }

    #[test]
    fn an_unsignable_request_or_an_empty_network_yields_nothing() {
        let key = identity::Keypair::generate_ed25519();
        assert!(NodeSigner::new(&key, "").is_none());
        let signer = NodeSigner::new(&key, "net").unwrap();
        assert!(signer.header("POST", "/a?x=1", b"", "rcpt").is_err());
        assert!(signer.header("POST", "/a", b"", "").is_err());
    }
}
