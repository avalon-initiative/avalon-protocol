//! Verifies (#397) that nothing in Avalon's own WebAuthn configuration
//! blocks hybrid transport ("use a phone or tablet") — the standard
//! WebAuthn ceremony where a nearby phone proves proximity over Bluetooth
//! and unlocks its own resident passkey to authenticate an otherwise
//! authenticator-less browser session. This is NOT #307's cross-device
//! pairing (a short-code/polling flow for a client with no WebAuthn surface
//! at all, like a game engine); hybrid transport is a built-in browser/OS
//! feature offered during an ordinary WebAuthn ceremony against
//! `/identities/register/*` and `/sessions/*`.
//!
//! What this test can and cannot prove: no headless environment can drive a
//! real phone over Bluetooth through an actual hybrid ceremony, so this does
//! NOT exercise hybrid transport itself — only a human with a real browser
//! and a real phone can do that (see `docs/architecture/identity.md`'s
//! "Today in the repo" note on this ticket for what was and wasn't manually
//! verified). What this test asserts instead, against a real running
//! server:
//!
//! 1. The raw JSON challenge returned by `/identities/register/start` sets
//!    no `authenticatorAttachment` restriction, and `/sessions/start` sets
//!    no per-credential `transports` restriction — the two fields that, if
//!    set, are exactly what would stop a browser from offering hybrid as an
//!    option at all.
//! 2. A credential produced by a generic virtual authenticator (standing in
//!    for "whatever authenticator type produced this credential, hybrid
//!    included") registers and authenticates exactly like any other
//!    credential in this repo's other passkey tests — proving no
//!    server-side code branches on authenticator type.

use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::{CredentialCreationOptions, CredentialRequestOptions};
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn rp_origin() -> url::Url {
    let origin = std::env::var("AVALON_WEBAUTHN_ORIGIN")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    url::Url::parse(&origin).expect("AVALON_WEBAUTHN_ORIGIN must be a valid URL")
}

/// Named so `Client`'s ceremony state can be carried across `await` points —
/// see `crates/server/tests/passkeys.rs`'s identical type alias for why the
/// TLD-verifier parameter has to be spelled out explicitly.
type VirtualClient =
    Client<MemoryStore, MockUserValidationMethod, public_suffix::PublicSuffixList, ()>;

fn new_virtual_client() -> VirtualClient {
    let authenticator = Authenticator::new(
        Aaguid::new_empty(),
        MemoryStore::new(),
        MockUserValidationMethod::verified_user(2),
    );
    Client::new(authenticator).allows_insecure_localhost(true)
}

#[tokio::test]
#[ignore]
async fn registration_and_login_challenges_do_not_restrict_authenticator_attachment() {
    let http = reqwest::Client::new();
    let base = server_url();
    let identity_id = Uuid::new_v4();
    let display_name = format!("hybrid-test-{identity_id}");

    let start: serde_json::Value = http
        .post(format!("{base}/identities/register/start"))
        .json(&serde_json::json!({
            "identity_id": identity_id,
            "display_name": display_name,
        }))
        .send()
        .await
        .expect("register/start failed — is `make start` running?")
        .json()
        .await
        .unwrap();

    let creation_public_key = &start["challenge"]["publicKey"];
    assert!(
        creation_public_key
            .get("authenticatorSelection")
            .and_then(|sel| sel.get("authenticatorAttachment"))
            .is_none(),
        "registration options must not set authenticatorAttachment, or \
         browsers would stop offering the hybrid (\"use a phone or \
         tablet\") option during passkey registration"
    );

    // Complete a real registration with a generic virtual authenticator —
    // not tagged platform-only — so there's a credential to exercise the
    // login challenge against below.
    let mut client = new_virtual_client();
    let origin = rp_origin();
    let creation_options: CredentialCreationOptions =
        serde_json::from_value(start["challenge"].clone()).unwrap();
    let credential = client
        .register(Origin::from(&origin), creation_options, DefaultClientData)
        .await
        .expect("virtual authenticator registration should succeed");

    let signing_key = SigningKey::generate(&mut rand::rng());
    let signing_bytes =
        format!("avalon:identity.created:v1:{identity_id}:{display_name}").into_bytes();
    let signature = signing_key.sign(&signing_bytes);

    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine;

    http.post(format!("{base}/identities/register/finish"))
        .json(&serde_json::json!({
            "ticket_id": start["ticket_id"],
            "webauthn_credential": credential,
            "event_signing_public_key": BASE64.encode(signing_key.verifying_key().to_bytes()),
            "event_signature": BASE64.encode(signature.to_bytes()),
            "device_label": null,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .expect("register/finish should succeed");

    let login_start: serde_json::Value = http
        .post(format!("{base}/sessions/start"))
        .json(&serde_json::json!({ "identity_id": identity_id }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let login_public_key = &login_start["challenge"]["publicKey"];
    let allow_credentials = login_public_key["allowCredentials"]
        .as_array()
        .expect("allowCredentials should be present");
    assert_eq!(allow_credentials.len(), 1);
    assert!(
        allow_credentials[0]
            .get("transports")
            .is_none_or(|t| t.is_null()),
        "allowCredentials must not set transports, or browsers would stop \
         offering the hybrid (\"use a phone or tablet\") option regardless \
         of how the credential was originally registered"
    );

    // Finishing the ceremony below is the "identical treatment" half of
    // this test: nothing server-side special-cases a credential based on
    // what produced it.
    let request_options: CredentialRequestOptions =
        serde_json::from_value(login_start["challenge"].clone()).unwrap();
    let assertion = client
        .authenticate(Origin::from(&origin), request_options, DefaultClientData)
        .await
        .expect("virtual authenticator authentication should succeed");

    let finish: serde_json::Value = http
        .post(format!("{base}/sessions/finish"))
        .json(&serde_json::json!({
            "ticket_id": login_start["ticket_id"],
            "credential": assertion,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .expect("sessions/finish should succeed")
        .json()
        .await
        .unwrap();

    let token = finish["token"].as_str().unwrap();
    let me: serde_json::Value = http
        .get(format!("{base}/me"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["identity_id"], identity_id.to_string());
}
