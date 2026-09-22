//! Multi-passkey management (issue #200) on [`super::AccountSession`] — WebAuthn
//! login credentials, distinct from `devices` (event-signing keys). See
//! `crates/server/src/passkeys.rs`.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::SdkError;

use super::{webauthn, AccountSession, SignatureFields};

/// One of this identity's registered WebAuthn passkeys.
#[derive(Debug, Clone)]
pub struct Passkey {
    /// The `identity_keys.id` this passkey is stored under.
    pub id: Uuid,
    /// User-chosen label, if any (e.g. "Work laptop's fingerprint sensor").
    pub label: Option<String>,
    /// When this passkey was registered.
    pub added_at: OffsetDateTime,
}

impl TryFrom<crate::generated::PasskeyResponse> for Passkey {
    type Error = SdkError;

    fn try_from(body: crate::generated::PasskeyResponse) -> Result<Self, SdkError> {
        Ok(Passkey {
            id: body.id,
            label: body.label,
            added_at: super::parse_rfc3339(&body.added_at)?,
        })
    }
}

#[derive(Deserialize)]
struct AddPasskeyStartResponse {
    ticket_id: Uuid,
    challenge: passkey_types::webauthn::CredentialCreationOptions,
}

#[derive(Serialize)]
struct AddPasskeyFinishRequest {
    ticket_id: Uuid,
    webauthn_credential: passkey_types::webauthn::CreatedPublicKeyCredential,
    label: Option<String>,
}

#[derive(Serialize, Default)]
struct RevokePasskeyRequest {
    #[serde(flatten)]
    signature: SignatureFields,
}

impl AccountSession {
    /// `GET /me/passkeys` — every passkey registered to this identity.
    pub async fn list_passkeys(&self) -> Result<Vec<Passkey>, SdkError> {
        let raw: Vec<crate::generated::PasskeyResponse> = self.get("/me/passkeys").await?;
        raw.into_iter().map(Passkey::try_from).collect()
    }

    /// Registers an *additional* passkey for this identity, driving a real
    /// WebAuthn registration ceremony against a fresh virtual authenticator
    /// (`POST /me/passkeys/register/start` -> ceremony ->
    /// `POST /me/passkeys/register/finish`) — issue #200. Note this new
    /// passkey is not the one [`AccountSession::credentials`] carries; this
    /// session keeps using whichever passkey it originally logged in with.
    pub async fn add_passkey(&self, label: Option<&str>) -> Result<Passkey, SdkError> {
        let start: AddPasskeyStartResponse = self.post_empty("/me/passkeys/register/start").await?;
        let (webauthn_credential, _stored) =
            webauthn::registration_ceremony(start.challenge).await?;
        let raw: crate::generated::PasskeyResponse = self
            .post(
                "/me/passkeys/register/finish",
                &AddPasskeyFinishRequest {
                    ticket_id: start.ticket_id,
                    webauthn_credential,
                    label: label.map(str::to_string),
                },
            )
            .await?;
        raw.try_into()
    }

    /// `PATCH /me/passkeys/{id}` — relabels a passkey. Not
    /// signature-required.
    pub async fn rename_passkey(&self, passkey_id: Uuid, label: &str) -> Result<Passkey, SdkError> {
        let raw: crate::generated::PasskeyResponse = self
            .patch(
                &format!("/me/passkeys/{passkey_id}"),
                &crate::generated::RenamePasskeyRequest {
                    label: label.to_string(),
                },
            )
            .await?;
        raw.try_into()
    }

    /// `POST /me/passkeys/{id}/revoke` — always signs (`passkey.revoke_last`,
    /// `[passkey_id, identity_id]`), whether or not this is actually the
    /// identity's last remaining passkey: #697/#698 only require a fresh
    /// signature in the last-passkey case, but an unused-but-valid
    /// signature on a non-last revoke is harmless, same simplification the
    /// Hub frontend already makes (see [`super::AccountSession::sign`]'s
    /// own doc comment).
    pub async fn revoke_passkey(&self, passkey_id: Uuid) -> Result<(), SdkError> {
        let signature = self.sign(
            "passkey.revoke_last",
            &[&passkey_id.to_string(), &self.identity().id.0.to_string()],
        );
        self.post_no_response(
            &format!("/me/passkeys/{passkey_id}/revoke"),
            &RevokePasskeyRequest { signature },
        )
        .await
    }
}
