//! Multi-passkey management for an already-authenticated identity
//! (issue #200) — the cheap, near-term mitigation for issue #99's identity-
//! recovery decision: losing one device's passkey shouldn't mean losing the
//! identity, as long as at least one other passkey was registered first.
//!
//! Deliberately its own module, separate from `handlers.rs`'s
//! `register_start`/`register_finish` (identity *creation*, unauthenticated)
//! and separate from `devices.rs`'s Ed25519 signing-key device list (#135) —
//! same "not the same concept" distinction `docs/architecture/identity.md`
//! draws between the two key domains. `identity_keys` (this module) is the
//! WebAuthn login credential; `identity_signing_keys` (`devices.rs`) is the
//! event-authorship key. Both happen to look like "a device with a label and
//! a revoke button" from the UI, but they are different rows in different
//! tables with different security properties, so they get different
//! endpoints rather than being folded into one.
//!
//! No protocol event is emitted for adding or revoking a passkey:
//! `docs/architecture/identity.md`'s durability table classifies WebAuthn
//! passkeys as "operational state, not an event" — same as the very first
//! passkey `handlers::register_finish` writes today, which has never emitted
//! anything of its own beyond `identity.created`.
//!
//! Revoking a passkey is a hard delete, not a soft `revoked_at` flag (unlike
//! `identity_signing_keys`): nothing else references a `identity_keys` row
//! by id (no event payload, no other table's foreign key), and
//! `handlers::fetch_passkeys` has no revoked-state filter to keep in sync —
//! a deleted row simply can no longer authenticate, which is exactly what
//! "revoke a login credential" should mean.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;
use webauthn_rs::prelude::*;

use crate::error::AppError;
use crate::handlers::authenticate;
use crate::state::AppState;

const CEREMONY_TTL_MINUTES: i64 = 5;
const ADD_PASSKEY_CEREMONY_KIND: &str = "add_passkey";

/// Ceremony state persisted between `passkeys/register/start` and
/// `passkeys/register/finish` — mirrors `handlers::RegistrationCeremonyState`
/// but deliberately carries no `display_name`: this ceremony is gated by an
/// existing session, not by the account-creation flow, so there's no
/// display name being chosen here at all.
#[derive(Serialize, Deserialize)]
struct AddPasskeyCeremonyState {
    identity_id: Uuid,
    webauthn_state: PasskeyRegistration,
}

#[derive(Serialize)]
pub struct AddPasskeyStartResponse {
    pub ticket_id: Uuid,
    pub challenge: CreationChallengeResponse,
}

/// `POST /me/passkeys/register/start` — begins a WebAuthn registration
/// ceremony for an *additional* passkey bound to the caller's already-
/// existing identity, gated by the caller's session rather than by an
/// unclaimed identity id the way `handlers::register_start` is. Existing
/// credential ids are passed as `exclude_credentials` so an authenticator
/// that already registered one of them (e.g. the same physical key) won't
/// silently re-register itself as a second, functionally duplicate
/// credential.
pub async fn register_start(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AddPasskeyStartResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let existing_credential_rows =
        sqlx::query("SELECT credential_id FROM identity_keys WHERE identity_id = $1")
            .bind(identity_id)
            .fetch_all(&state.pool)
            .await?;
    let exclude_credentials: Vec<CredentialID> = existing_credential_rows
        .iter()
        .map(|row| -> Result<CredentialID, AppError> {
            let bytes: Vec<u8> = row.try_get("credential_id")?;
            Ok(CredentialID::from(bytes))
        })
        .collect::<Result<_, _>>()?;

    let display_name_row = sqlx::query("SELECT display_name FROM profiles WHERE identity_id = $1")
        .bind(identity_id)
        .fetch_optional(&state.pool)
        .await?;
    // `user.name`/`user.displayName` are cosmetic here (see
    // `handlers::register_start`'s own comment on why identity id, not
    // display name, is `user.name` for login purposes) — this ceremony
    // never creates a new WebAuthn "user," just a second credential for one
    // that already exists, so falling back to the raw id if the profile
    // lookup somehow comes back empty is safe rather than fatal.
    let display_name: String = match display_name_row {
        Some(row) => row.try_get("display_name")?,
        None => identity_id.to_string(),
    };

    let (challenge, webauthn_state) = state
        .webauthn
        .start_passkey_registration(
            identity_id,
            &identity_id.to_string(),
            &display_name,
            Some(exclude_credentials),
        )
        .map_err(|_| AppError::WebauthnFailed)?;

    let ceremony = AddPasskeyCeremonyState {
        identity_id,
        webauthn_state,
    };
    let ticket_id = Uuid::new_v4();
    let expires_at = OffsetDateTime::now_utc() + time::Duration::minutes(CEREMONY_TTL_MINUTES);

    sqlx::query(
        "INSERT INTO webauthn_ceremonies (id, kind, state, expires_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(ticket_id)
    .bind(ADD_PASSKEY_CEREMONY_KIND)
    .bind(serde_json::to_value(&ceremony).expect("ceremony state should serialize"))
    .bind(expires_at)
    .execute(&state.pool)
    .await?;

    Ok(Json(AddPasskeyStartResponse {
        ticket_id,
        challenge,
    }))
}

#[derive(Deserialize)]
pub struct AddPasskeyFinishRequest {
    pub ticket_id: Uuid,
    pub webauthn_credential: RegisterPublicKeyCredential,
    /// A user-chosen label for the passkey being added (e.g. "Work
    /// laptop's fingerprint sensor") — purely descriptive, same convention
    /// as `identity_signing_keys.label` / `handlers::RegisterFinishRequest::device_label`.
    pub label: Option<String>,
}

#[derive(Serialize)]
pub struct PasskeyResponse {
    pub id: Uuid,
    pub label: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub added_at: OffsetDateTime,
}

/// `POST /me/passkeys/register/finish` — completes the ceremony
/// `register_start` began and inserts the new `identity_keys` row. Requires
/// the *same* authenticated session throughout (both `start` and `finish`
/// re-derive `identity_id` from the caller's own bearer token; the ceremony
/// row's `identity_id` is checked against it too) rather than trusting
/// whatever identity the stored ceremony state says — defense in depth
/// against a captured ticket id being replayed from a different identity's
/// session, on top of the ceremony `kind` already keeping this flow's
/// tickets out of `handlers::register_finish`'s unauthenticated
/// identity-creation path.
pub async fn register_finish(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AddPasskeyFinishRequest>,
) -> Result<Json<PasskeyResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let row = sqlx::query(
        "DELETE FROM webauthn_ceremonies WHERE id = $1 AND kind = $2 RETURNING state, expires_at",
    )
    .bind(body.ticket_id)
    .bind(ADD_PASSKEY_CEREMONY_KIND)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::CeremonyNotFound)?;

    let expires_at: OffsetDateTime = row.try_get("expires_at")?;
    if expires_at < OffsetDateTime::now_utc() {
        return Err(AppError::CeremonyExpired);
    }
    let state_json: serde_json::Value = row.try_get("state")?;
    let ceremony: AddPasskeyCeremonyState =
        serde_json::from_value(state_json).map_err(|_| AppError::CeremonyNotFound)?;

    if ceremony.identity_id != identity_id {
        return Err(AppError::CeremonyNotFound);
    }

    let passkey = state
        .webauthn
        .finish_passkey_registration(&body.webauthn_credential, &ceremony.webauthn_state)
        .map_err(|_| AppError::WebauthnFailed)?;

    let passkey_json = serde_json::to_value(&passkey).expect("Passkey should serialize");
    let credential_id: &[u8] = passkey.cred_id().as_ref();

    let inserted = sqlx::query(
        r#"
        INSERT INTO identity_keys (identity_id, credential_id, passkey_data, label)
        VALUES ($1, $2, $3, $4)
        RETURNING id, added_at
        "#,
    )
    .bind(identity_id)
    .bind(credential_id)
    .bind(&passkey_json)
    .bind(&body.label)
    .fetch_one(&state.pool)
    .await?;

    Ok(Json(PasskeyResponse {
        id: inserted.try_get("id")?,
        label: body.label,
        added_at: inserted.try_get("added_at")?,
    }))
}

/// `GET /me/passkeys` — every passkey registered to the caller's identity,
/// for the Hub's "your passkeys" list.
pub async fn list_passkeys(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<PasskeyResponse>>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let rows = sqlx::query(
        "SELECT id, label, added_at FROM identity_keys WHERE identity_id = $1 ORDER BY added_at",
    )
    .bind(identity_id)
    .fetch_all(&state.pool)
    .await?;

    let mut passkeys = Vec::with_capacity(rows.len());
    for row in rows {
        passkeys.push(PasskeyResponse {
            id: row.try_get("id")?,
            label: row.try_get("label")?,
            added_at: row.try_get("added_at")?,
        });
    }
    Ok(Json(passkeys))
}

#[derive(Deserialize)]
pub struct RenamePasskeyRequest {
    pub label: String,
}

/// `PATCH /me/passkeys/:id` — same unilateral-rename convention as
/// `devices::rename_device`.
pub async fn rename_passkey(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(passkey_id): Path<Uuid>,
    Json(body): Json<RenamePasskeyRequest>,
) -> Result<Json<PasskeyResponse>, AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let row = sqlx::query(
        r#"
        UPDATE identity_keys
        SET label = $3
        WHERE id = $1 AND identity_id = $2
        RETURNING id, label, added_at
        "#,
    )
    .bind(passkey_id)
    .bind(identity_id)
    .bind(&body.label)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::PasskeyNotFound)?;

    Ok(Json(PasskeyResponse {
        id: row.try_get("id")?,
        label: row.try_get("label")?,
        added_at: row.try_get("added_at")?,
    }))
}

#[derive(Deserialize)]
pub struct RevokePasskeyQuery {
    /// Explicit "I understand this may lock me out" acknowledgment (the
    /// ticket's own invariant), required only when revoking would leave
    /// zero passkeys — see [`guard_revoke_last_passkey`]. Any value other
    /// than the literal string `"true"` is treated as not confirmed, same
    /// fail-closed convention as everywhere else a boolean is read off a
    /// query string in this codebase.
    #[serde(default)]
    pub confirm: Option<String>,
}

fn is_confirmed(query: &RevokePasskeyQuery) -> bool {
    query.confirm.as_deref() == Some("true")
}

/// The revoke-last-passkey guard, factored out as a pure function so it's
/// unit-testable without a database: given how many passkeys the identity
/// has *before* this revocation and whether the caller explicitly
/// confirmed, decide whether the revoke may proceed. Never allows the
/// count to silently reach zero.
fn guard_revoke_last_passkey(
    remaining_before_revoke: i64,
    confirmed: bool,
) -> Result<(), AppError> {
    if remaining_before_revoke <= 1 && !confirmed {
        return Err(AppError::LastPasskeyRequiresConfirmation);
    }
    Ok(())
}

/// `POST /me/passkeys/:id/revoke?confirm=true` — deletes one passkey.
/// Revoking the identity's last remaining passkey requires `?confirm=true`
/// (the ticket's explicit-confirmation invariant); revoking one of several
/// never does. The count check and the delete happen inside one transaction
/// so a concurrent registration/revoke from another session can't race past
/// the guard.
pub async fn revoke_passkey(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(passkey_id): Path<Uuid>,
    Query(query): Query<RevokePasskeyQuery>,
) -> Result<(), AppError> {
    let identity_id = authenticate(&state, &headers).await?;

    let mut tx = state.pool.begin().await?;

    // Row-locks every passkey of this identity for the duration of the
    // transaction, so a concurrent revoke of a different passkey from
    // another session can't both read "2 remaining" and both proceed
    // unconfirmed, leaving zero. `FOR UPDATE` can't be combined with an
    // aggregate (`COUNT(*)`) — Postgres rejects that outright — so this
    // fetches the locked rows themselves and counts them in Rust.
    let locked_rows = sqlx::query("SELECT id FROM identity_keys WHERE identity_id = $1 FOR UPDATE")
        .bind(identity_id)
        .fetch_all(&mut *tx)
        .await?;
    let remaining_before_revoke: i64 = locked_rows.len() as i64;

    guard_revoke_last_passkey(remaining_before_revoke, is_confirmed(&query))?;

    let deleted = sqlx::query("DELETE FROM identity_keys WHERE id = $1 AND identity_id = $2")
        .bind(passkey_id)
        .bind(identity_id)
        .execute(&mut *tx)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(AppError::PasskeyNotFound);
    }

    tx.commit().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — `guard_revoke_last_passkey` is a
    //! pure function specifically so this invariant (never let a revoke
    //! silently reach zero remaining passkeys) is testable without one. The
    //! full register-second-passkey -> authenticate-with-either ->
    //! revoke-one -> still-authenticated round trip is covered by
    //! `crates/server/tests/passkeys.rs`, gated `--ignored`.

    use super::*;

    #[test]
    fn revoking_the_only_passkey_without_confirmation_is_rejected() {
        assert!(matches!(
            guard_revoke_last_passkey(1, false),
            Err(AppError::LastPasskeyRequiresConfirmation)
        ));
    }

    #[test]
    fn revoking_the_only_passkey_with_confirmation_is_allowed() {
        assert!(guard_revoke_last_passkey(1, true).is_ok());
    }

    #[test]
    fn revoking_one_of_several_passkeys_needs_no_confirmation() {
        assert!(guard_revoke_last_passkey(2, false).is_ok());
        assert!(guard_revoke_last_passkey(5, false).is_ok());
    }

    #[test]
    fn revoking_one_of_several_passkeys_still_allowed_when_confirmed_anyway() {
        assert!(guard_revoke_last_passkey(3, true).is_ok());
    }

    #[test]
    fn a_zero_count_is_treated_as_at_the_floor_too() {
        // Shouldn't be reachable in practice (an identity always has at
        // least one passkey after registration), but the guard fails
        // closed rather than open for it: `remaining_before_revoke <= 1`
        // covers zero the same as one, so a data inconsistency here still
        // demands confirmation rather than silently proceeding.
        // `revoke_passkey`'s own `rows_affected() == 0` check is what
        // actually reports "not found" once past this guard.
        assert!(matches!(
            guard_revoke_last_passkey(0, false),
            Err(AppError::LastPasskeyRequiresConfirmation)
        ));
    }

    #[test]
    fn confirm_query_param_only_accepts_the_literal_true() {
        assert!(is_confirmed(&RevokePasskeyQuery {
            confirm: Some("true".to_string())
        }));
        assert!(!is_confirmed(&RevokePasskeyQuery {
            confirm: Some("1".to_string())
        }));
        assert!(!is_confirmed(&RevokePasskeyQuery {
            confirm: Some("True".to_string())
        }));
        assert!(!is_confirmed(&RevokePasskeyQuery { confirm: None }));
    }
}
