//! Drives a real WebAuthn registration/login ceremony against a
//! software-only virtual authenticator (`passkey-authenticator`'s
//! `testable` feature) — the same approach
//! `crates/cli/src/dev_tools.rs::create_identity`/`login` already proved
//! for `avalon create-identity`/`avalon login`. Duplicated here rather
//! than shared via a new dependency edge: `avalon-cli` already depends on
//! `avalon-sdk`, not the other way around, and this module's own ceremony
//! logic is small enough that factoring it out into a third shared crate
//! wasn't worth the churn for #699 — see that ticket's own scoping notes.
//!
//! This SDK target is native/CLI-context, not a browser: there is no real
//! WebAuthn surface to drive from a Rust process, so [`AccountSession`]
//! registration/login always goes through this virtual authenticator,
//! exactly the way `avalon-cli`'s dev tooling does.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use coset::{CborSerializable, CoseKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::{CredentialCreationOptions, CredentialRequestOptions};
use passkey_types::Passkey;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::SdkError;

/// A JSON-serializable mirror of `passkey_types::Passkey`, matching
/// `crates/cli/src/dev_tools.rs::StoredPasskey` field-for-field — see that
/// type's own doc comment for why this exists instead of deriving
/// `Serialize`/`Deserialize` on `Passkey` directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredPasskey {
    key_cbor_base64: String,
    credential_id_base64: String,
    rp_id: String,
    user_handle_base64: Option<String>,
    username: Option<String>,
    user_display_name: Option<String>,
    counter: Option<u32>,
}

impl From<&Passkey> for StoredPasskey {
    fn from(passkey: &Passkey) -> Self {
        Self {
            key_cbor_base64: BASE64.encode(
                passkey
                    .key
                    .clone()
                    .to_vec()
                    .expect("CoseKey should always serialize to CBOR"),
            ),
            credential_id_base64: BASE64.encode(Vec::from(passkey.credential_id.clone())),
            rp_id: passkey.rp_id.clone(),
            user_handle_base64: passkey
                .user_handle
                .clone()
                .map(|handle| BASE64.encode(Vec::from(handle))),
            username: passkey.username.clone(),
            user_display_name: passkey.user_display_name.clone(),
            counter: passkey.counter,
        }
    }
}

impl StoredPasskey {
    fn into_passkey(self) -> Passkey {
        let key_bytes = BASE64
            .decode(&self.key_cbor_base64)
            .expect("stored passkey's key_cbor_base64 should be valid base64");
        Passkey {
            key: CoseKey::from_slice(&key_bytes)
                .expect("stored passkey's CoseKey CBOR should decode"),
            credential_id: BASE64
                .decode(&self.credential_id_base64)
                .expect("stored passkey's credential_id_base64 should be valid base64")
                .into(),
            rp_id: self.rp_id,
            user_handle: self.user_handle_base64.map(|encoded| {
                BASE64
                    .decode(encoded)
                    .expect("stored passkey's user_handle_base64 should be valid base64")
                    .into()
            }),
            username: self.username,
            user_display_name: self.user_display_name,
            counter: self.counter,
            extensions: Default::default(),
        }
    }
}

/// `AVALON_WEBAUTHN_ORIGIN`, falling back to the same default
/// `crates/cli/src/dev_tools.rs` uses — the ceremony origin has to match
/// whatever `avalon-server`'s own `Webauthn` instance was configured with.
fn webauthn_origin() -> Result<url::Url, SdkError> {
    let origin_str = std::env::var("AVALON_WEBAUTHN_ORIGIN")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    url::Url::parse(&origin_str)
        .map_err(|e| SdkError::Protocol(format!("invalid AVALON_WEBAUTHN_ORIGIN: {e}")))
}

fn virtual_authenticator() -> Authenticator<MemoryStore, MockUserValidationMethod> {
    let store = MemoryStore::new();
    let user_mock = MockUserValidationMethod::verified_user(1);
    Authenticator::new(Aaguid::new_empty(), store, user_mock)
}

/// Drives a full registration ceremony against `creation_options` and
/// returns the resulting WebAuthn credential plus the one passkey the
/// virtual authenticator created — the caller (`AccountSession`
/// registration) is responsible for persisting the latter if it wants to
/// log back in later via [`login_ceremony`].
pub(crate) async fn registration_ceremony(
    creation_options: CredentialCreationOptions,
) -> Result<
    (
        passkey_types::webauthn::CreatedPublicKeyCredential,
        StoredPasskey,
    ),
    SdkError,
> {
    let origin_url = webauthn_origin()?;
    let mut client = Client::new(virtual_authenticator()).allows_insecure_localhost(true);

    let webauthn_credential = client
        .register(
            Origin::from(&origin_url),
            creation_options,
            DefaultClientData,
        )
        .await
        .map_err(|e| SdkError::Protocol(format!("WebAuthn registration ceremony failed: {e:?}")))?;

    let store = client.authenticator().store();
    let passkey = store
        .values()
        .next()
        .expect("registration should have saved exactly one passkey to the virtual store");
    Ok((webauthn_credential, StoredPasskey::from(passkey)))
}

/// Drives a full authentication ceremony against `request_options`, using
/// a virtual authenticator preloaded with `stored_passkey` — "the same
/// device asking again," matching `crates/cli/src/dev_tools.rs::login`.
pub(crate) async fn login_ceremony(
    request_options: CredentialRequestOptions,
    stored_passkey: StoredPasskey,
) -> Result<passkey_types::webauthn::AuthenticatedPublicKeyCredential, SdkError> {
    let origin_url = webauthn_origin()?;
    let passkey = stored_passkey.into_passkey();
    let mut store = MemoryStore::new();
    store.insert(Vec::from(passkey.credential_id.clone()), passkey);
    let user_mock = MockUserValidationMethod::verified_user(1);
    let authenticator = Authenticator::new(Aaguid::new_empty(), store, user_mock);
    let mut client = Client::new(authenticator).allows_insecure_localhost(true);

    client
        .authenticate(
            Origin::from(&origin_url),
            request_options,
            DefaultClientData,
        )
        .await
        .map_err(|e| SdkError::Protocol(format!("WebAuthn authentication ceremony failed: {e:?}")))
}

/// Must produce exactly the bytes `avalon-server`'s
/// `handlers::identity_created_signing_bytes` reconstructs — duplicated
/// rather than shared, same reasoning as `crates/cli/src/dev_tools.rs`'s
/// own copy of this function.
pub(crate) fn identity_created_signing_bytes(identity_id: Uuid, display_name: &str) -> Vec<u8> {
    format!("avalon:identity.created:v1:{identity_id}:{display_name}").into_bytes()
}
