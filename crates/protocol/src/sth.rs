//! Signed Tree Heads — STH-only
//! signing, matching Certificate Transparency precedent: no per-entry
//! signatures, only the tree head is signed. One [`SignedTreeHead`] is
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
//!   private key: a mirror must be able to verify the log with the public
//!   key alone.
//!   If unset but `AVALON_SETTLEMENT_SIGNING_KEY` is, the verify key is
//!   derived from it as a single-operator dev convenience — see
//!   [`load_verify_key_from_env`].
//!
//! `signing_key_id` is a caller-chosen label distinguishing this
//! settlement-operator key domain from issuer keys and user
//! keys — three separate lifecycles. It is
//! not a foreign key into anything; nothing here enforces its shape beyond
//! "non-empty text."

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use time::OffsetDateTime;

/// Default `signing_key_id` when `AVALON_SETTLEMENT_SIGNING_KEY_ID` isn't
/// set — fine for a single-operator milestone-1 deployment. A real key
/// rotation should set this explicitly so historical STHs keep naming the
/// key that actually signed them, distinct from whatever key is active now.
pub const DEFAULT_SIGNING_KEY_ID: &str = "settlement-operator-1";

/// Issue #531 (managed hosting): the *unsigned* candidate tree head
/// `PostgresSettlementProvider::prepare` returns as a preview — the exact
/// fields an integrator using a managed host needs to sign locally
/// (`signing_message`'s inputs, minus the signature itself) before
/// calling `POST /ledger/finalize-batch`. A preview only, never persisted
/// — see `prepare`'s own doc comment for why finalize recomputes fresh
/// rather than trusting this back.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PreparedTreeHead {
    pub batch_id: uuid::Uuid,
    pub tree_size: i64,
    pub root_hash: String,
    pub network_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

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
/// Public since issue #531: a managed-hosting integrator needs to
/// construct these exact bytes themselves, outside this crate entirely
/// (they sign locally, with a key this crate/node never holds — see
/// `PostgresSettlementProvider::prepare`'s doc comment), not just internal
/// callers signing/verifying with an in-process key.
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
        let signing_key = SigningKey::generate(&mut rand::rng());
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

    /// Independent cross-implementation reference vector for issue #234
    /// (the `ed25519-dalek` 2.x -> 3.0.0 / `rand` 0.8 -> 0.10 bump) — the
    /// same rigor #210/#211 applied to the Merkle side
    /// (`crates/chain/src/merkle.rs`'s externally-sourced RFC 6962 vectors),
    /// applied here to the Ed25519 signing primitive this module wraps.
    /// `seed`/`public_key`/`signature` were produced independently by
    /// Python's `cryptography` library (OpenSSL-backed Ed25519, a
    /// completely separate implementation from `curve25519-dalek`) signing
    /// `message` — not derived from or copied out of this codebase. If the
    /// 3.0.0 bump had changed key derivation, signing, or the signature
    /// wire format in any way, this would fail even though every other test
    /// in this file (which only ever round-trips against itself) would
    /// still pass.
    #[test]
    fn sign_matches_independent_ed25519_implementation() {
        let seed_hex = "e7daaf365088407baa0fb9be13f67a04b59c04357ff7d02a22686aa7cbfb8271";
        let expected_public_key_hex =
            "ff096f4d891c5b05dcb85c076278226bc718f4767a9be75fb1d7abdaf0bc0d31";
        let message: &[u8] =
            b"avalon-settlement-sth-v1 cross-implementation reference vector for issue #234";
        let expected_signature_hex = "2726c91e79e546a24dc01d49a5552a3d4c4eeffe86d801943dcde26b8f9306325fc0db30965f70882c98ee826d7a5b1d9a3fe6eb15f70e4dad8ea8f7865d9607";

        let seed: [u8; 32] = hex::decode(seed_hex)
            .expect("reference seed should be valid hex")
            .try_into()
            .expect("reference seed should be 32 bytes");
        let signing_key = SigningKey::from_bytes(&seed);

        // Key derivation matches the independent implementation exactly.
        assert_eq!(
            hex::encode(signing_key.verifying_key().to_bytes()),
            expected_public_key_hex,
            "public key derived from the reference seed no longer matches the \
             independently-produced reference public key"
        );

        // Ed25519 is deterministic — signing the same message with the same
        // key must reproduce the exact same signature bytes the independent
        // implementation produced, not just "a valid-looking signature".
        let signature: Signature = signing_key.sign(message);
        assert_eq!(
            hex::encode(signature.to_bytes()),
            expected_signature_hex,
            "signature produced by ed25519-dalek no longer matches the \
             independently-produced reference signature"
        );

        // And the reverse direction: this crate's verifier must accept a
        // signature it did not itself produce.
        let signature_bytes: [u8; 64] = hex::decode(expected_signature_hex)
            .expect("reference signature should be valid hex")
            .try_into()
            .expect("reference signature should be 64 bytes");
        let externally_produced_signature = Signature::from_bytes(&signature_bytes);
        assert!(
            signing_key
                .verifying_key()
                .verify(message, &externally_produced_signature)
                .is_ok(),
            "failed to verify a signature produced by an independent Ed25519 implementation"
        );
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
