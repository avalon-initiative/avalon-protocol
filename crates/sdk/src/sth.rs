//! Signed Tree Heads, client side (issue #774).
//!
//! Two clients in this crate need real STH logic, not just a wire shape:
//! [`crate::network`] verifies a node's latest Signed Tree Head against a
//! pinned trust anchor before trusting the network it claims to be, and
//! [`crate::managed_hosting`] signs a prepared tree head locally with a key
//! the managed host never holds. Both of those are signature operations, so
//! this module carries its own implementation of the canonical
//! signing-message format rather than reaching into the server's crates —
//! exactly the posture the C# and TypeScript SDKs already take for every
//! signed payload they construct.
//!
//! This is the same category of duplication as
//! `crate::achievements`'s attestation signing bytes: the format is a
//! documented contract, and `conformance/vectors/` is what keeps the two
//! implementations honest rather than the compiler. Nothing about key
//! *loading* lives here — a settlement operator's private key is a
//! server-side concern, and this crate never reads one from the
//! environment.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use time::OffsetDateTime;

/// The *unsigned* candidate tree head a managed host returns as a preview
/// (`POST /ledger/prepare-batch`, issue #531) — the exact fields an
/// integrator needs in order to sign locally before calling
/// `POST /ledger/finalize-batch`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PreparedTreeHead {
    /// The batch this candidate head covers.
    pub batch_id: uuid::Uuid,
    /// How many entries the log would hold once this batch commits.
    pub tree_size: i64,
    /// Lowercase hex-encoded candidate Merkle Tree Hash.
    pub root_hash: String,
    /// Which network the host claims to be committing to.
    pub network_id: String,
    /// The timestamp folded into the signed bytes.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// One Signed Tree Head — produced by [`sign_tree_head`], or read back from
/// a node's `GET /ledger/sth/latest` for verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedTreeHead {
    /// How many entries the log held when this head was signed.
    pub tree_size: i64,
    /// Lowercase hex-encoded Merkle Tree Hash.
    pub root_hash: String,
    /// The network this head claims to belong to — never sufficient on its
    /// own; see [`crate::network`].
    pub network_id: String,
    /// A caller-chosen label naming the settlement-operator key that signed
    /// this, distinct from issuer keys and user keys.
    pub signing_key_id: String,
    /// Lowercase hex-encoded Ed25519 signature (64 bytes).
    pub signature: String,
    /// The timestamp folded into the signed bytes.
    pub created_at: OffsetDateTime,
}

/// The exact bytes an STH's signature covers: `(tree_size, root_hash,
/// network_id, timestamp)`, canonically encoded so there is exactly one way
/// to serialize a given tuple of those fields — a fixed domain tag,
/// `tree_size` and the timestamp as fixed-width big-endian integers, and
/// `root_hash` explicitly length-prefixed ahead of its bytes. Only
/// `network_id` is variable-length with no length prefix of its own, but
/// it's unambiguous anyway: it's the last field before the fixed-width
/// timestamp suffix, so two different `network_id` values can never produce
/// identical trailing bytes once that suffix is accounted for.
///
/// Must stay byte-for-byte identical to the server's own construction
/// (`avalon_protocol::sth::signing_message`) — the shared conformance
/// vectors under `conformance/vectors/` are what prove it still is.
pub fn signing_message(
    tree_size: i64,
    root_hash_hex: &str,
    network_id: &str,
    created_at: OffsetDateTime,
) -> Vec<u8> {
    let mut message = Vec::new();
    message.extend_from_slice(b"avalon-settlement-sth-v1");
    message.extend_from_slice(&tree_size.to_be_bytes());
    message.extend_from_slice(&(root_hash_hex.len() as u32).to_be_bytes());
    message.extend_from_slice(root_hash_hex.as_bytes());
    message.extend_from_slice(network_id.as_bytes());
    message.extend_from_slice(&created_at.unix_timestamp().to_be_bytes());
    message
}

/// Signs `(tree_size, root_hash_hex, network_id, created_at)` with
/// `signing_key`.
pub fn sign_tree_head(
    signing_key: &SigningKey,
    signing_key_id: &str,
    tree_size: i64,
    root_hash_hex: &str,
    network_id: &str,
    created_at: OffsetDateTime,
) -> SignedTreeHead {
    let message = signing_message(tree_size, root_hash_hex, network_id, created_at);
    let signature: Signature = signing_key.sign(&message);
    SignedTreeHead {
        tree_size,
        root_hash: root_hash_hex.to_string(),
        network_id: network_id.to_string(),
        signing_key_id: signing_key_id.to_string(),
        signature: hex::encode(signature.to_bytes()),
        created_at,
    }
}

/// Verifies `sth`'s signature against `verifying_key` — `false` for any
/// malformed signature (bad hex, wrong length) as well as an
/// outright-invalid one; never panics on attacker-controlled input.
pub fn verify_tree_head(verifying_key: &VerifyingKey, sth: &SignedTreeHead) -> bool {
    let message = signing_message(
        sth.tree_size,
        &sth.root_hash,
        &sth.network_id,
        sth.created_at,
    );
    let Ok(signature_bytes) = hex::decode(&sth.signature) else {
        return false;
    };
    let Ok(signature_array) = <[u8; 64]>::try_from(signature_bytes.as_slice()) else {
        return false;
    };
    let signature = Signature::from_bytes(&signature_array);
    verifying_key.verify(&message, &signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_hash_fixture() -> String {
        "ab".repeat(32)
    }

    #[test]
    fn sign_then_verify_round_trip_succeeds() {
        let signing_key = SigningKey::generate(&mut rand::rng());
        let sth = sign_tree_head(
            &signing_key,
            "test-key",
            42,
            &root_hash_fixture(),
            "avalon-test",
            OffsetDateTime::UNIX_EPOCH,
        );
        assert!(verify_tree_head(&signing_key.verifying_key(), &sth));
    }

    #[test]
    fn verify_rejects_signature_from_a_different_key() {
        let signing_key = SigningKey::generate(&mut rand::rng());
        let other_key = SigningKey::generate(&mut rand::rng());
        let sth = sign_tree_head(
            &signing_key,
            "test-key",
            1,
            &root_hash_fixture(),
            "avalon-test",
            OffsetDateTime::UNIX_EPOCH,
        );
        assert!(!verify_tree_head(&other_key.verifying_key(), &sth));
    }

    #[test]
    fn verify_rejects_a_tampered_field() {
        let signing_key = SigningKey::generate(&mut rand::rng());
        let verifying_key = signing_key.verifying_key();
        let sth = sign_tree_head(
            &signing_key,
            "test-key",
            7,
            &root_hash_fixture(),
            "avalon-test",
            OffsetDateTime::UNIX_EPOCH,
        );

        let mut tampered_size = sth.clone();
        tampered_size.tree_size += 1;
        assert!(!verify_tree_head(&verifying_key, &tampered_size));

        let mut tampered_root = sth.clone();
        tampered_root.root_hash = "cd".repeat(32);
        assert!(!verify_tree_head(&verifying_key, &tampered_root));

        let mut tampered_network = sth.clone();
        tampered_network.network_id = "avalon-mainnet-1".to_string();
        assert!(!verify_tree_head(&verifying_key, &tampered_network));

        let mut tampered_timestamp = sth.clone();
        tampered_timestamp.created_at = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1);
        assert!(!verify_tree_head(&verifying_key, &tampered_timestamp));
    }

    #[test]
    fn verify_rejects_malformed_signature_without_panicking() {
        let signing_key = SigningKey::generate(&mut rand::rng());
        let mut sth = sign_tree_head(
            &signing_key,
            "test-key",
            1,
            &root_hash_fixture(),
            "avalon-test",
            OffsetDateTime::UNIX_EPOCH,
        );

        sth.signature = "not-hex".to_string();
        assert!(!verify_tree_head(&signing_key.verifying_key(), &sth));

        sth.signature = "ab".to_string(); // valid hex, wrong length
        assert!(!verify_tree_head(&signing_key.verifying_key(), &sth));
    }
}
