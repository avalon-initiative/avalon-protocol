//! Session-continuation token verification (issue #525, Part 2 of #521's
//! decision) — the server-side half of `avalon_protocol::continuation`.
//! See that module's doc comment for the wire shape and the "never a login
//! credential by itself" invariant this preserves.
//!
//! [`verify`] works identically whether this node ever locally ran the
//! WebAuthn/signing-key ceremony that created `signing_key_id` or only
//! ever learned about it via mirrored history —
//! `avalon_indexer::projections::identity_signing_keys` is populated both
//! ways (see that module's own doc comment), so there is no "authoring
//! node only" special case here at all.

use avalon_indexer::projections::identity_signing_keys;
use avalon_protocol::continuation::ContinuationToken;
use time::{Duration, OffsetDateTime};

use crate::auth::verify_event_signature;
use crate::error::AppError;
use crate::state::AppState;

/// Small allowance for clock drift between the minting client and this
/// node — a token whose `issued_at` is up to this far in the future is
/// still accepted, rather than failing every request for a client whose
/// clock is a few seconds fast.
const CLOCK_SKEW_ALLOWANCE: Duration = Duration::seconds(5);

/// Verifies `wire_body` (the token string with [`avalon_protocol::continuation::WIRE_PREFIX`]
/// already stripped by the caller) and returns the identity it authenticates,
/// exactly like [`crate::handlers::authenticate_token`] does for an opaque
/// session token. Every failure — malformed token, expired, unknown/revoked
/// key, bad signature, replayed nonce — is [`AppError::Unauthorized`],
/// never distinguished, same posture `authenticate_token`'s own doc comment
/// already states for session tokens.
pub async fn verify(state: &AppState, wire_body: &str) -> Result<uuid::Uuid, AppError> {
    let token = ContinuationToken::from_wire_body(wire_body).ok_or(AppError::Unauthorized)?;

    let now = OffsetDateTime::now_utc();
    if token.expires_at < now || token.issued_at > now + CLOCK_SKEW_ALLOWANCE {
        return Err(AppError::Unauthorized);
    }
    // Never trust a client-claimed validity window longer than the
    // protocol's own documented cap — a verifier that only checked
    // `expires_at < now` would accept a token a client minted with an
    // arbitrarily distant `expires_at`.
    if token.expires_at - token.issued_at
        > Duration::seconds(avalon_protocol::continuation::DEFAULT_TTL_SECONDS)
    {
        return Err(AppError::Unauthorized);
    }

    let key = identity_signing_keys::find_active_by_id(&state.pool, token.signing_key_id)
        .await?
        .ok_or(AppError::Unauthorized)?;

    let signature_bytes = hex::decode(&token.signature).map_err(|_| AppError::Unauthorized)?;
    if !verify_event_signature(&key.public_key, &token.signing_bytes(), &signature_bytes) {
        return Err(AppError::Unauthorized);
    }

    // Anti-replay: this token's nonce may only ever be accepted once.
    // Opportunistic sweep first — cheap, keeps the table from growing
    // unbounded without needing a separate scheduled job (see migration
    // 0066's own comment).
    sqlx::query("DELETE FROM consumed_continuation_nonces WHERE expires_at < now()")
        .execute(&state.pool)
        .await?;
    let inserted = sqlx::query(
        "INSERT INTO consumed_continuation_nonces (nonce, expires_at) VALUES ($1, $2) \
         ON CONFLICT DO NOTHING",
    )
    .bind(token.nonce)
    .bind(token.expires_at)
    .execute(&state.pool)
    .await?;
    if inserted.rows_affected() == 0 {
        return Err(AppError::Unauthorized);
    }

    // The verified key's own `identity_id`, never the token's claimed one
    // — `signing_key_id` already univocally names one identity via
    // `indexer_identity_signing_keys`' primary key, so this is the
    // authoritative answer regardless of what the token's own
    // `identity_id` field says.
    Ok(key.identity_id)
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — the expiry/skew/TTL-cap checks
    //! are the only pure logic in this module worth unit-testing directly;
    //! everything else needs a real `identity_signing_keys` row and is
    //! covered by `crates/server/tests/session_continuation.rs`, gated
    //! `--ignored`.

    use avalon_protocol::continuation::{signing_bytes, ContinuationToken};
    use ed25519_dalek::{Signer, SigningKey};
    use uuid::Uuid;

    use super::*;

    fn signed_token(
        signing_key: &SigningKey,
        issued_at: OffsetDateTime,
        expires_at: OffsetDateTime,
    ) -> (ContinuationToken, Uuid) {
        let identity_id = Uuid::new_v4();
        let signing_key_id = Uuid::new_v4();
        let nonce = Uuid::new_v4();
        let bytes = signing_bytes(identity_id, signing_key_id, nonce, issued_at, expires_at);
        let signature = signing_key.sign(&bytes);
        (
            ContinuationToken {
                identity_id,
                signing_key_id,
                nonce,
                issued_at,
                expires_at,
                signature: hex::encode(signature.to_bytes()),
            },
            identity_id,
        )
    }

    #[test]
    fn a_freshly_signed_token_verifies_against_its_own_signing_key() {
        let signing_key = SigningKey::generate(&mut rand::rng());
        let now = OffsetDateTime::now_utc();
        let (token, _identity_id) = signed_token(&signing_key, now, now + Duration::seconds(30));

        let signature_bytes = hex::decode(&token.signature).unwrap();
        assert!(verify_event_signature(
            signing_key.verifying_key().as_bytes(),
            &token.signing_bytes(),
            &signature_bytes,
        ));
    }

    #[test]
    fn tampering_with_any_field_breaks_the_signature() {
        let signing_key = SigningKey::generate(&mut rand::rng());
        let now = OffsetDateTime::now_utc();
        let (mut token, _identity_id) =
            signed_token(&signing_key, now, now + Duration::seconds(30));
        token.identity_id = Uuid::new_v4(); // tampered after signing

        let signature_bytes = hex::decode(&token.signature).unwrap();
        assert!(!verify_event_signature(
            signing_key.verifying_key().as_bytes(),
            &token.signing_bytes(),
            &signature_bytes,
        ));
    }
}
