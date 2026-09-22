//! Cross-node login grant — issue #620 (decided), epic #623, this module is
//! #633.
//!
//! #620's decided problem: WebAuthn RP-ID scoping means a passkey literally
//! cannot be presented to a node the identity never registered a passkey
//! on — nothing in Avalon's own protocol design can route around that, it's
//! part of WebAuthn's security model. [`CrossNodeLoginGrant`] is the fix:
//! extends #307's cross-device approve/deny pattern to cover logging into a
//! node the identity has no local session or passkey for at all.
//!
//! Same idea as [`crate::continuation::ContinuationToken`] and
//! [`crate::interest_claim::InterestClaim`]: a short assertion self-signed
//! with the identity's own Ed25519 event-signing key
//! (`apps/hub/src/crypto/signingKey.ts`), verifiable by *any* node against
//! `avalon_indexer::projections::identity_signing_keys` — no new key
//! material, no new custody model.
//!
//! **What makes this different from a continuation token**: a continuation
//! token proves "the holder of this identity's active signing key wants to
//! act now" — #122's decided reason that alone can never be a login
//! credential, since it doesn't prove a human just completed a real
//! approval gesture. A [`CrossNodeLoginGrant`] is minted only after a human
//! is shown real context (identity, requesting integrator/node,
//! `issued_at`) and explicitly approves — that shown-context-then-approve
//! gesture is what supplies the "real human, right now" guarantee a bare
//! continuation token lacks. Verifying a grant is entirely this crate's
//! (and the verifying node's) job; nothing here decides whether a caller
//! *should* accept one — see `crates/server` (issue #634) for the actual
//! lifecycle and verification path.
//!
//! **`destination_base_url` is inside the signed bytes, not alongside
//! them**, same reasoning `interest_claim`'s own doc comment gives for
//! `base_url`: a grant approved for one destination must never verify
//! successfully against a different one, or an attacker could relay an
//! approved grant to a node the human never actually saw.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// How long a grant is valid for after `issued_at`, at mint time — the
/// verifying node independently re-checks `expires_at` itself. Short, same
/// reasoning as `continuation::DEFAULT_TTL_SECONDS`: this is a one-shot
/// approval consumed once by `POST /auth/cross-node/submit` (#634), not a
/// re-presented claim like `InterestClaim`.
pub const DEFAULT_TTL_SECONDS: i64 = 60;

/// A self-signed assertion that a human, shown `requesting_context`, just
/// approved logging `identity_id` (acting through `signing_key_id`) into
/// `destination_base_url`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CrossNodeLoginGrant {
    pub identity_id: Uuid,
    /// Which of the identity's (possibly several) signing keys approved
    /// this — same multi-key model `identity_signing_keys`/`devices.rs`
    /// already supports.
    pub signing_key_id: Uuid,
    /// The node this grant is good for logging into, and nowhere else —
    /// see this module's doc comment on why it must live inside the signed
    /// bytes.
    pub destination_base_url: String,
    /// The human-legible context shown to the approver before they
    /// approved (requesting integrator/node name, at minimum) — carried in
    /// the signed bytes so the grant itself is evidence of what was shown,
    /// not just a claim about it. #642 (decision, open) still owns what
    /// this minimally has to contain.
    pub requesting_context: String,
    /// Anti-replay: unique per grant, checked against a
    /// consumed-nonce table by the verifying node on
    /// `POST /auth/cross-node/submit`, same pattern
    /// `ContinuationToken::nonce`'s own doc comment establishes.
    pub nonce: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub issued_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = "date-time")]
    pub expires_at: OffsetDateTime,
    /// Lowercase hex-encoded Ed25519 signature over [`signing_bytes`].
    pub signature: String,
}

/// The exact bytes a [`CrossNodeLoginGrant`]'s signature covers —
/// deliberately excludes `signature` itself and includes every other
/// field, same convention `continuation::signing_bytes` and
/// `interest_claim::signing_bytes` already establish: a signature can never
/// be replayed against a different identity, key, destination, context, or
/// validity window than the one it was actually produced for.
pub fn signing_bytes(
    identity_id: Uuid,
    signing_key_id: Uuid,
    destination_base_url: &str,
    requesting_context: &str,
    nonce: Uuid,
    issued_at: OffsetDateTime,
    expires_at: OffsetDateTime,
) -> Vec<u8> {
    format!(
        "avalon:cross-node-login:v1:{identity_id}:{signing_key_id}:{destination_base_url}:{requesting_context}:{nonce}:{}:{}",
        issued_at.unix_timestamp(),
        expires_at.unix_timestamp(),
    )
    .into_bytes()
}

impl CrossNodeLoginGrant {
    /// The bytes this grant's own `signature` field should cover — what
    /// both the approving client and the verifying node compute
    /// independently.
    pub fn signing_bytes(&self) -> Vec<u8> {
        signing_bytes(
            self.identity_id,
            self.signing_key_id,
            &self.destination_base_url,
            &self.requesting_context,
            self.nonce,
            self.issued_at,
            self.expires_at,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CrossNodeLoginGrant {
        CrossNodeLoginGrant {
            identity_id: Uuid::nil(),
            signing_key_id: Uuid::nil(),
            destination_base_url: "https://node-b.example".to_string(),
            requesting_context: "SomeGame (node-b.example)".to_string(),
            nonce: Uuid::nil(),
            issued_at: OffsetDateTime::from_unix_timestamp(1_000_000_000).unwrap(),
            expires_at: OffsetDateTime::from_unix_timestamp(1_000_000_060).unwrap(),
            signature: "deadbeef".to_string(),
        }
    }

    #[test]
    fn signing_bytes_changes_if_destination_changes() {
        let base = sample();
        let mut different_destination = base.clone();
        different_destination.destination_base_url = "https://attacker.example".to_string();
        assert_ne!(different_destination.signing_bytes(), base.signing_bytes());
    }

    #[test]
    fn signing_bytes_changes_if_requesting_context_changes() {
        let base = sample();
        let mut different_context = base.clone();
        different_context.requesting_context = "DifferentGame (node-b.example)".to_string();
        assert_ne!(different_context.signing_bytes(), base.signing_bytes());
    }

    #[test]
    fn signing_bytes_changes_if_any_field_changes() {
        let base = sample();
        let base_bytes = base.signing_bytes();

        let mut different_identity = base.clone();
        different_identity.identity_id = Uuid::from_u128(1);
        assert_ne!(different_identity.signing_bytes(), base_bytes);

        let mut different_key = base.clone();
        different_key.signing_key_id = Uuid::from_u128(1);
        assert_ne!(different_key.signing_bytes(), base_bytes);

        let mut different_nonce = base.clone();
        different_nonce.nonce = Uuid::from_u128(1);
        assert_ne!(different_nonce.signing_bytes(), base_bytes);

        let mut different_expiry = base.clone();
        different_expiry.expires_at = base.expires_at + time::Duration::seconds(1);
        assert_ne!(different_expiry.signing_bytes(), base_bytes);
    }

    #[test]
    fn serde_round_trips() {
        let grant = sample();
        let json = serde_json::to_string(&grant).unwrap();
        let decoded: CrossNodeLoginGrant = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, grant);
    }
}
