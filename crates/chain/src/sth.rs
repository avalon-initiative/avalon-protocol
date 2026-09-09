//! Signed Tree Heads — issue #210, implementing #39's decision (STH-only
//! signing, matching Certificate Transparency precedent: no per-entry
//! signatures, only the tree head is signed). One [`SignedTreeHead`] is
//! produced per batch commit (`PostgresSettlementProvider::commit`),
//! always in the same transaction as the batch itself, and stored in the
//! `signed_tree_heads` table (`crates/server/db/migrations/0024_signed_tree_heads`).
//!
//! The private signing key is loaded from the environment — never stored
//! in Postgres, the same precedent every other key-handling note in this
//! repo follows:
//!
//! - `AVALON_SETTLEMENT_SIGNING_KEY` — a raw 32-byte Ed25519 seed,
//!   hex-encoded. Only needed by whatever process actually calls `commit`
//!   (`avalon-server`'s settlement worker, `crates/server/src/outbox.rs`).
//! - `AVALON_SETTLEMENT_VERIFY_KEY` — the corresponding 32-byte Ed25519
//!   public key, hex-encoded. This is all `avalon inspect-ledger` or a
//!   read-only mirror ever needs — verification must never require the
//!   private key ([#39](https://github.com/LunarVagabond/avalon-protocol/issues/39)'s
//!   "a mirror must be able to verify the log with the public key alone").
//!   If unset but `AVALON_SETTLEMENT_SIGNING_KEY` is, the verify key is
//!   derived from it as a single-operator dev convenience — see
//!   [`load_verify_key_from_env`].
//!
//! `signing_key_id` is a caller-chosen label distinguishing this
//! settlement-operator key domain from issuer keys (#80/#84) and player
//! keys (#73) — three separate lifecycles, per #39's own scoping. It is
//! not a foreign key into anything; nothing here enforces its shape beyond
//! "non-empty text."

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use time::OffsetDateTime;

/// Default `signing_key_id` when `AVALON_SETTLEMENT_SIGNING_KEY_ID` isn't
/// set — fine for a single-operator milestone-1 deployment. A real key
/// rotation should set this explicitly so historical STHs keep naming the
/// key that actually signed them, distinct from whatever key is active now.
pub const DEFAULT_SIGNING_KEY_ID: &str = "settlement-operator-1";

/// One row of `signed_tree_heads` — produced by [`sign_tree_head`] at
/// commit time, or read back from storage (`PostgresSettlementProvider::
/// list_signed_tree_heads`) for verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedTreeHead {
    pub tree_size: i64,
    /// Lowercase hex-encoded Merkle Tree Hash — same encoding
    /// `ledger_batches.batch_root`/`ledger_entries.entry_hash` already use.
    pub root_hash: String,
    pub network_id: String,
    pub signing_key_id: String,
    /// Lowercase hex-encoded Ed25519 signature (64 bytes).
    pub signature: String,
    pub created_at: OffsetDateTime,
}

/// The exact bytes an STH's signature covers: `(tree_size, root_hash,
/// network_id, timestamp)`, canonically encoded so there is exactly one
/// way to serialize a given tuple of those fields — a fixed domain tag (so
/// this can never be confused with a signature produced by some other
/// scheme in this codebase, the same motivation `postgres.rs::hash_entry`
/// has for hashing in `network_id`), `tree_size` and the timestamp as
/// fixed-width big-endian integers, and `root_hash` explicitly
/// length-prefixed ahead of its bytes. Only `network_id` is variable-length
/// with no length prefix of its own, but it's unambiguous anyway: it's the
/// last field before the fixed-width timestamp suffix, so two different
/// `network_id` values (of any length) can never produce identical
/// trailing bytes once that fixed-width suffix is accounted for.
fn signing_message(
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
/// `signing_key`, producing the [`SignedTreeHead`] `commit` stores.
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

#[derive(Debug, thiserror::Error)]
pub enum KeyLoadError {
    #[error("{0} must be set (hex-encoded 32-byte Ed25519 key material) — see .env.example")]
    Missing(&'static str),
    #[error("{0} is not valid hex: {1}")]
    InvalidHex(&'static str, hex::FromHexError),
    #[error("{0} must decode to exactly 32 bytes, got {1}")]
    WrongLength(&'static str, usize),
    #[error("{0} does not decode to a valid Ed25519 key")]
    InvalidKey(&'static str),
}

fn parse_key_bytes(var_name: &'static str, hex_value: &str) -> Result<[u8; 32], KeyLoadError> {
    let bytes = hex::decode(hex_value).map_err(|e| KeyLoadError::InvalidHex(var_name, e))?;
    let len = bytes.len();
    <[u8; 32]>::try_from(bytes).map_err(|_| KeyLoadError::WrongLength(var_name, len))
}

/// Loads the settlement operator's private signing key from
/// `AVALON_SETTLEMENT_SIGNING_KEY`, plus its `signing_key_id`
/// (`AVALON_SETTLEMENT_SIGNING_KEY_ID`, defaulting to
/// [`DEFAULT_SIGNING_KEY_ID`]) — the only place in this codebase that
/// should ever read the private half of this key domain. Called by
/// `PostgresSettlementProvider::commit`, never persisted.
pub fn load_signing_key_from_env() -> Result<(SigningKey, String), KeyLoadError> {
    let hex_value = std::env::var("AVALON_SETTLEMENT_SIGNING_KEY")
        .map_err(|_| KeyLoadError::Missing("AVALON_SETTLEMENT_SIGNING_KEY"))?;
    let seed = parse_key_bytes("AVALON_SETTLEMENT_SIGNING_KEY", &hex_value)?;
    let signing_key = SigningKey::from_bytes(&seed);
    let key_id = std::env::var("AVALON_SETTLEMENT_SIGNING_KEY_ID")
        .unwrap_or_else(|_| DEFAULT_SIGNING_KEY_ID.to_string());
    Ok((signing_key, key_id))
}

/// Loads only the settlement operator's *public* key
/// (`AVALON_SETTLEMENT_VERIFY_KEY`) — what `avalon inspect-ledger` and any
/// read-only mirror use, and all they should ever need. Falls back to
/// deriving the public key from `AVALON_SETTLEMENT_SIGNING_KEY` when the
/// verify-key variable isn't set, purely as a single-operator dev
/// convenience so a local `.env` with just the signing key still lets
/// `inspect-ledger` verify its own STHs; a deployment that wants to keep
/// the private key away from every reader should set
/// `AVALON_SETTLEMENT_VERIFY_KEY` explicitly instead of relying on this
/// fallback.
pub fn load_verify_key_from_env() -> Result<VerifyingKey, KeyLoadError> {
    if let Ok(hex_value) = std::env::var("AVALON_SETTLEMENT_VERIFY_KEY") {
        let bytes = parse_key_bytes("AVALON_SETTLEMENT_VERIFY_KEY", &hex_value)?;
        return VerifyingKey::from_bytes(&bytes)
            .map_err(|_| KeyLoadError::InvalidKey("AVALON_SETTLEMENT_VERIFY_KEY"));
    }
    let (signing_key, _) = load_signing_key_from_env()
        .map_err(|_| KeyLoadError::Missing("AVALON_SETTLEMENT_VERIFY_KEY"))?;
    Ok(signing_key.verifying_key())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_hash_fixture() -> String {
        "ab".repeat(32)
    }

    #[test]
    fn sign_then_verify_round_trip_succeeds() {
        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let verifying_key = signing_key.verifying_key();

        let sth = sign_tree_head(
            &signing_key,
            "test-key",
            42,
            &root_hash_fixture(),
            "avalon-test",
            OffsetDateTime::UNIX_EPOCH,
        );

        assert!(verify_tree_head(&verifying_key, &sth));
    }

    #[test]
    fn verify_rejects_signature_from_a_different_key() {
        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let other_key = SigningKey::generate(&mut rand::rngs::OsRng);

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
        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
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
        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
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

    #[test]
    fn parse_key_bytes_rejects_bad_hex_and_wrong_length() {
        assert!(matches!(
            parse_key_bytes("VAR", "not-hex"),
            Err(KeyLoadError::InvalidHex("VAR", _))
        ));
        assert!(matches!(
            parse_key_bytes("VAR", "aabb"),
            Err(KeyLoadError::WrongLength("VAR", 2))
        ));
        assert!(parse_key_bytes("VAR", &"ab".repeat(32)).is_ok());
    }
}
