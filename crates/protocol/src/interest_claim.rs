//! Signed DHT interest claims — issue #610, closing the gap #608 left open:
//! nothing stopped an already-admitted, same-network node from registering
//! DHT interest (`crate::interest` in `avalon-server`, not this crate) in a
//! guild channel or conversation it has no real member in, and having
//! `realtime_relay`'s relay trust that registration enough to POST real
//! chat content to it.
//!
//! An [`InterestClaim`] is the same idea as [`crate::continuation::ContinuationToken`]
//! applied to a different problem: a short assertion self-signed with the
//! identity's own Ed25519 event-signing key (`apps/hub`'s
//! `packages/api-client/src/crypto/signingKey.ts`), verifiable by *any* node
//! against `avalon_indexer::projections::identity_signing_keys` — no new key
//! material, no new custody model, reusing exactly what #525 already
//! established for session continuation.
//!
//! **`base_url` is inside the signed bytes, not alongside them.** A DHT
//! record is, by construction, readable by any same-network node — if
//! `base_url` weren't signed, an attacker could observe a legitimate
//! member's own valid claim already sitting in the DHT and republish it
//! under the same key with `base_url` swapped to their own node, which
//! would pass every other check (real signature, real current membership)
//! and redirect that member's real chat traffic to the attacker. Binding
//! `base_url` into what's signed means a claim only ever vouches for
//! delivery to the one node its own identity actually authorized.
//!
//! **What this proves, and what it doesn't.** A valid claim proves "the
//! holder of this identity's active signing key currently wants channel/
//! conversation `scope`'s traffic delivered to `base_url`." It does *not*
//! prove that identity is still a member of `scope` by itself — that's a
//! separate, independent check the verifying node makes against its own
//! local (ledger-derived) membership projection at lookup time
//! (`avalon-server`'s `crate::interest::lookup_claimed`), the same
//! two-questions-not-one split `docs/projects/backend-server/architecture/achievements-and-attestations.md`'s
//! authenticity-vs-validity distinction already establishes for a
//! different kind of signed claim.
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// What an [`InterestClaim`] can vouch for — mirrors `avalon-server`'s own
/// `interest::InterestScope`, minus the `Network` variant (mirror-sync's
/// use of the same DHT mechanism is node-to-node, not
/// identity-scoped, and is deliberately out of scope here — see this
/// module's own doc comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClaimedScope {
    Channel { channel_id: Uuid },
    Conversation { conversation_id: Uuid },
}

/// Same cap `avalon_protocol::continuation::DEFAULT_TTL_SECONDS` documents
/// for the same reason, just much longer-lived: unlike a continuation
/// token (a one-shot reconnect assertion, re-minted every time it's used),
/// an interest claim is minted once per websocket subscription and then
/// re-`PutRecord`d verbatim by `avalon-server`'s `interest::run_worker` on
/// every refresh tick for as long as the subscription stays open — see
/// that module's own doc comment on why the DHT record's own liveness TTL
/// is a separate, much shorter concern from this claim's own validity
/// window. 24h comfortably outlives any real browser session; a client
/// that's still connected past that just needs to resubscribe (which any
/// reconnect already does) to mint a fresh one.
pub const DEFAULT_TTL_SECONDS: i64 = 24 * 60 * 60;

/// A self-signed assertion that `identity_id` (acting through
/// `signing_key_id`) currently wants `scope`'s realtime traffic delivered
/// to `base_url`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterestClaim {
    pub identity_id: Uuid,
    pub signing_key_id: Uuid,
    pub scope: ClaimedScope,
    pub base_url: String,
    /// Not anti-replay in the continuation-token sense (this claim is
    /// meant to be re-presented verbatim by the DHT for its whole validity
    /// window, not consumed once) — present only so two claims minted for
    /// the same identity/scope/base_url in the same second still produce
    /// distinct signed bytes, same defensive habit
    /// `ContinuationToken::nonce`'s own doc comment documents even where a
    /// collision would be harmless.
    pub nonce: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub issued_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    /// Lowercase hex-encoded Ed25519 signature over [`signing_bytes`].
    pub signature: String,
}

/// The exact bytes an [`InterestClaim`]'s signature covers — deliberately
/// excludes `signature` itself and includes every other field, same
/// convention `avalon_protocol::continuation::signing_bytes` already
/// establishes and for the same reason: a signature can never be replayed
/// against a different identity, key, scope, destination, or validity
/// window than the one it was actually produced for.
pub fn signing_bytes(
    identity_id: Uuid,
    signing_key_id: Uuid,
    scope: ClaimedScope,
    base_url: &str,
    nonce: Uuid,
    issued_at: OffsetDateTime,
    expires_at: OffsetDateTime,
) -> Vec<u8> {
    let scope_tag = match scope {
        ClaimedScope::Channel { channel_id } => format!("channel:{channel_id}"),
        ClaimedScope::Conversation { conversation_id } => {
            format!("conversation:{conversation_id}")
        }
    };
    format!(
        "avalon:interest-claim:v1:{identity_id}:{signing_key_id}:{scope_tag}:{base_url}:{nonce}:{}:{}",
        issued_at.unix_timestamp(),
        expires_at.unix_timestamp(),
    )
    .into_bytes()
}

impl InterestClaim {
    pub fn signing_bytes(&self) -> Vec<u8> {
        signing_bytes(
            self.identity_id,
            self.signing_key_id,
            self.scope,
            &self.base_url,
            self.nonce,
            self.issued_at,
            self.expires_at,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> InterestClaim {
        InterestClaim {
            identity_id: Uuid::nil(),
            signing_key_id: Uuid::nil(),
            scope: ClaimedScope::Channel {
                channel_id: Uuid::nil(),
            },
            base_url: "https://node-a.example".to_string(),
            nonce: Uuid::nil(),
            issued_at: OffsetDateTime::from_unix_timestamp(1_000_000_000).unwrap(),
            expires_at: OffsetDateTime::from_unix_timestamp(1_000_086_400).unwrap(),
            signature: "deadbeef".to_string(),
        }
    }

    #[test]
    fn signing_bytes_changes_if_base_url_changes() {
        let base = sample();
        let mut different_base_url = base.clone();
        different_base_url.base_url = "https://attacker.example".to_string();
        assert_ne!(different_base_url.signing_bytes(), base.signing_bytes());
    }

    #[test]
    fn signing_bytes_changes_if_scope_changes() {
        let base = sample();
        let mut different_scope = base.clone();
        different_scope.scope = ClaimedScope::Conversation {
            conversation_id: Uuid::nil(),
        };
        assert_ne!(different_scope.signing_bytes(), base.signing_bytes());
    }

    #[test]
    fn channel_and_conversation_scopes_of_the_same_uuid_never_collide() {
        let id = Uuid::new_v4();
        let channel = ClaimedScope::Channel { channel_id: id };
        let conversation = ClaimedScope::Conversation {
            conversation_id: id,
        };
        let base = sample();
        let a = signing_bytes(
            base.identity_id,
            base.signing_key_id,
            channel,
            &base.base_url,
            base.nonce,
            base.issued_at,
            base.expires_at,
        );
        let b = signing_bytes(
            base.identity_id,
            base.signing_key_id,
            conversation,
            &base.base_url,
            base.nonce,
            base.issued_at,
            base.expires_at,
        );
        assert_ne!(a, b);
    }

    #[test]
    fn signing_bytes_changes_if_any_field_changes() {
        let base = sample();
        let base_bytes = base.signing_bytes();

        let mut different_identity = base.clone();
        different_identity.identity_id = Uuid::from_u128(1);
        assert_ne!(different_identity.signing_bytes(), base_bytes);

        let mut different_expiry = base.clone();
        different_expiry.expires_at = base.expires_at + time::Duration::seconds(1);
        assert_ne!(different_expiry.signing_bytes(), base_bytes);
    }
}
