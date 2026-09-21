//! Shared "fresh signature" enforcement for #697's signature-required
//! account-action tier (#698) — see
//! `docs/architecture/identity.md`'s "Action-tier classification" section
//! for the endpoint-by-endpoint list this backs. Deliberately reuses
//! `auth::verify_event_signature` rather than inventing a new signing
//! scheme, mirroring `devices::device_grant_approval_signing_bytes`'s wire
//! shape: the caller supplies `signing_key_id` (one of their own
//! non-revoked `identity_signing_keys` rows) plus a base64 signature over a
//! canonical `avalon:<action_tag>:v1:<field>:<field>:...` byte string the
//! server reconstructs itself from the request's own fields, so a
//! signature minted for one action/target can never be replayed against a
//! different one.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use sqlx::Row;
use uuid::Uuid;

use crate::auth::verify_event_signature;
use crate::error::AppError;
use crate::state::AppState;

/// Builds the canonical byte string a fresh-signature action signs —
/// `avalon:<action_tag>:v1:<field1>:<field2>:...`, the same versioned-tag
/// shape `devices::device_grant_approval_signing_bytes` already
/// establishes, factored out here so every signature-required endpoint
/// builds it the same way.
pub fn canonical_message(action_tag: &str, fields: &[&str]) -> Vec<u8> {
    let mut message = format!("avalon:{action_tag}:v1");
    for field in fields {
        message.push(':');
        message.push_str(field);
    }
    message.into_bytes()
}

/// Whether `identity_id` has at least one currently-active signing key
/// registered at all — the split #698's invariants require between "you
/// have no key to sign with" ([`AppError::NoRegisteredSigningKey`]) and
/// "you have a key but didn't sign this request"
/// ([`AppError::FreshSignatureRequired`]).
pub async fn has_any_signing_key(state: &AppState, identity_id: Uuid) -> Result<bool, AppError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM identity_signing_keys WHERE identity_id = $1 AND revoked_at IS NULL",
    )
    .bind(identity_id)
    .fetch_one(&state.pool)
    .await?;
    Ok(count > 0)
}

/// The shared enforcement point every signature-required handler calls
/// before its mutation: verifies `signature_b64` is a valid Ed25519
/// signature over `canonical_message`, produced by `signing_key_id` — a
/// key that must belong to `identity_id` and not be revoked. `identity_id`
/// is always the already-authenticated caller (from `handlers::authenticate`),
/// never taken from the request body.
///
/// Missing `signing_key_id`/`signature_b64` is rejected with
/// [`AppError::NoRegisteredSigningKey`] only when the caller truly has no
/// active signing key at all; otherwise with
/// [`AppError::FreshSignatureRequired`] — an actionable "sign this" prompt,
/// not a generic auth failure, per #698's own invariant.
pub async fn require_fresh_signature(
    state: &AppState,
    identity_id: Uuid,
    canonical_message: &[u8],
    signing_key_id: Option<Uuid>,
    signature_b64: Option<&str>,
) -> Result<(), AppError> {
    let (signing_key_id, signature_b64) = match (signing_key_id, signature_b64) {
        (Some(key_id), Some(signature)) if !signature.is_empty() => (key_id, signature),
        _ => {
            return if has_any_signing_key(state, identity_id).await? {
                Err(AppError::FreshSignatureRequired)
            } else {
                Err(AppError::NoRegisteredSigningKey)
            };
        }
    };

    let key_row = sqlx::query(
        "SELECT public_key FROM identity_signing_keys \
         WHERE id = $1 AND identity_id = $2 AND revoked_at IS NULL",
    )
    .bind(signing_key_id)
    .bind(identity_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some(key_row) = key_row else {
        return Err(AppError::SigningKeyNotFound);
    };
    let public_key: Vec<u8> = key_row.try_get("public_key")?;

    let signature_bytes = BASE64
        .decode(signature_b64)
        .map_err(|_| AppError::InvalidFreshSignature)?;

    if !verify_event_signature(&public_key, canonical_message, &signature_bytes) {
        return Err(AppError::InvalidFreshSignature);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! `require_fresh_signature` itself needs live Postgres (it looks up
    //! `identity_signing_keys`) — covered by `crates/server/tests/*.rs`,
    //! gated `--ignored`, one per endpoint per the ticket. What's unit-
    //! testable without a database is `canonical_message`'s own shape.

    use super::*;

    #[test]
    fn canonical_message_is_stable_and_deterministic() {
        let a = canonical_message("guild.transfer_ownership", &["g1", "from1", "to1"]);
        let b = canonical_message("guild.transfer_ownership", &["g1", "from1", "to1"]);
        assert_eq!(a, b);
        assert_eq!(
            String::from_utf8(a).unwrap(),
            "avalon:guild.transfer_ownership:v1:g1:from1:to1"
        );
    }

    #[test]
    fn canonical_message_differs_by_action_tag() {
        let a = canonical_message("guild.transfer_ownership", &["g1"]);
        let b = canonical_message("guild.roles.create", &["g1"]);
        assert_ne!(a, b);
    }

    #[test]
    fn canonical_message_differs_by_fields() {
        let a = canonical_message("guild.transfer_ownership", &["g1", "to1"]);
        let b = canonical_message("guild.transfer_ownership", &["g1", "to2"]);
        assert_ne!(a, b);
    }
}
