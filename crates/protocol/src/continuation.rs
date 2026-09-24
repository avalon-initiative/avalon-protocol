//! Session-continuation tokens — issue #525, Part 2 of #521's decision.
//!
//! An existing session should survive its original node going offline: the
//! client mints a short-lived, self-signed assertion with the identity's
//! own Ed25519 event-signing key (the same key it already holds for
//! authoring events, `avalon-hub/apps/hub/src/crypto/signingKey.ts`), and *any* node —
//! including one the client never registered/logged into — can verify it
//! against that key's public half, read from `avalon_indexer::projections::identity_signing_keys`
//! (populated locally on an authoring node, reconstructed from replayed
//! history on a mirror-only one).
//!
//! **Never a login credential by itself** (#122's decided separation,
//! preserved here): a continuation token only extends an *already-
//! established* session. It proves "the holder of this identity's active
//! signing key wants to act now," not "a human just completed a WebAuthn
//! ceremony" — those are different claims, and this type only ever makes
//! the first one. Verifying a continuation token is entirely this crate's
//! (and the verifying node's) job; nothing here decides whether a caller
//! *should* be allowed to mint one — see `crates/server/src/continuation.rs`
//! for the actual verification path (signature check, revocation check,
//! anti-replay), which needs `ed25519-dalek` and Postgres access this crate
//! deliberately doesn't have.
//!
//! **Wire shape**: `AVCT1.<base64(json)>` — a distinct, unambiguous prefix
//! (`.` never appears in this codebase's opaque session tokens, which are
//! raw URL-safe-base64-no-pad, see `crate::auth::generate_session_token`)
//! so a bearer-token-shaped `Authorization` header can carry either kind
//! and a verifier can tell which one it's holding before doing any crypto.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// The wire prefix distinguishing a continuation token from an opaque
/// session token in the same `Authorization: Bearer <token>` slot.
pub const WIRE_PREFIX: &str = "AVCT1.";

/// How long a continuation token is valid for after `issued_at`, at mint
/// time — the verifying node independently re-checks `expires_at` itself
/// rather than trusting this, but a client should never mint one longer-
/// lived than this. Short on purpose: this is a reconnect mechanism, not a
/// session replacement — the underlying opaque session token (or a fresh
/// one from re-authenticating) is what actually persists.
pub const DEFAULT_TTL_SECONDS: i64 = 60;

/// A self-signed session-continuation assertion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContinuationToken {
    pub identity_id: Uuid,
    /// Which of the identity's (possibly several) signing keys minted
    /// this — same multi-key model `identity_signing_keys`/`devices.rs`
    /// already supports, so a continuation token names its key explicitly
    /// rather than assuming a single one.
    pub signing_key_id: Uuid,
    /// Anti-replay: unique per token, checked against
    /// `consumed_continuation_nonces` by the verifying node. Minting two
    /// tokens with the same nonce is a client bug, not something this type
    /// can prevent — uniqueness is enforced server-side, at verification
    /// time.
    pub nonce: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub issued_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    /// Lowercase hex-encoded Ed25519 signature over [`signing_bytes`].
    pub signature: String,
}

/// The exact bytes a continuation token's signature covers — a small,
/// explicit, versioned format, same convention
/// `handlers::identity_created_signing_bytes` already established:
/// deliberately excludes `signature` itself (obviously) and includes every
/// other field, so a signature can never be replayed against a different
/// identity, key, nonce, or validity window than the one it was actually
/// produced for.
pub fn signing_bytes(
    identity_id: Uuid,
    signing_key_id: Uuid,
    nonce: Uuid,
    issued_at: OffsetDateTime,
    expires_at: OffsetDateTime,
) -> Vec<u8> {
    format!(
        "avalon:continuation:v1:{identity_id}:{signing_key_id}:{nonce}:{}:{}",
        issued_at.unix_timestamp(),
        expires_at.unix_timestamp(),
    )
    .into_bytes()
}

impl ContinuationToken {
    /// The bytes this token's own `signature` field should cover — what
    /// both the minting client and the verifying node compute
    /// independently.
    pub fn signing_bytes(&self) -> Vec<u8> {
        signing_bytes(
            self.identity_id,
            self.signing_key_id,
            self.nonce,
            self.issued_at,
            self.expires_at,
        )
    }

    /// Encodes as `AVCT1.<base64(json)>` — what actually goes in an
    /// `Authorization: Bearer` header.
    pub fn to_wire(&self) -> String {
        let json = serde_json::to_vec(self).expect("ContinuationToken should serialize");
        format!(
            "{WIRE_PREFIX}{}",
            base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, json)
        )
    }

    /// Decodes a bearer-token string that has already been confirmed to
    /// start with [`WIRE_PREFIX`] (callers should check the prefix
    /// themselves before calling this, so a plain opaque session token
    /// never reaches JSON/base64 decoding at all). `None` for anything
    /// malformed — never a panic on attacker-controlled input.
    pub fn from_wire_body(body: &str) -> Option<Self> {
        let json =
            base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, body).ok()?;
        serde_json::from_slice(&json).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ContinuationToken {
        ContinuationToken {
            identity_id: Uuid::nil(),
            signing_key_id: Uuid::nil(),
            nonce: Uuid::nil(),
            issued_at: OffsetDateTime::from_unix_timestamp(1_000_000_000).unwrap(),
            expires_at: OffsetDateTime::from_unix_timestamp(1_000_000_060).unwrap(),
            signature: "deadbeef".to_string(),
        }
    }

    #[test]
    fn wire_round_trips() {
        let token = sample();
        let wire = token.to_wire();
        assert!(wire.starts_with(WIRE_PREFIX));
        let body = wire.strip_prefix(WIRE_PREFIX).unwrap();
        let decoded = ContinuationToken::from_wire_body(body).expect("should decode");
        assert_eq!(decoded, token);
    }

    #[test]
    fn from_wire_body_rejects_garbage_rather_than_panicking() {
        assert!(ContinuationToken::from_wire_body("not valid base64 at all!!!").is_none());
        assert!(ContinuationToken::from_wire_body("aGVsbG8=").is_none()); // valid base64, not JSON
    }

    #[test]
    fn signing_bytes_changes_if_any_field_changes() {
        let base = sample();
        let base_bytes = base.signing_bytes();

        let mut different_nonce = base.clone();
        different_nonce.nonce = Uuid::from_u128(1);
        assert_ne!(different_nonce.signing_bytes(), base_bytes);

        let mut different_identity = base.clone();
        different_identity.identity_id = Uuid::from_u128(1);
        assert_ne!(different_identity.signing_bytes(), base_bytes);

        let mut different_expiry = base.clone();
        different_expiry.expires_at = base.expires_at + time::Duration::seconds(1);
        assert_ne!(different_expiry.signing_bytes(), base_bytes);
    }

    #[test]
    fn wire_prefix_is_never_valid_base64_no_pad_alphabet_for_an_opaque_session_token() {
        // `.` (part of WIRE_PREFIX) never appears in URL-safe-no-pad
        // base64 output — the exact property `handlers::authenticate`
        // relies on to tell the two token kinds apart before doing any
        // crypto or JSON parsing.
        assert!(WIRE_PREFIX.contains('.'));
    }
}
