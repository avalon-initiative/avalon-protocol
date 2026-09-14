//! Exercises `AvalonClient::authenticate()` against a real, running
//! `avalon-server` (and therefore a real Postgres). Gated on `--ignored`
//! since it needs live infra — see `make test-live` / `make start`.
//!
//! Setup mirrors real usage: this SDK never creates identities or logs a
//! user in itself (that's the Hub's/CLI's job), so the test drives the raw
//! WebAuthn registration and login ceremonies directly over HTTP, using a
//! virtual/software authenticator (`passkey-authenticator`'s `testable`
//! feature) to simulate what a browser+passkey will do once the Hub (#55)
//! exists — then uses the SDK only for the integrator-side `authenticate()` call.
//! See `docs/architecture/identity.md` for the two-key model this drives:
//! a WebAuthn passkey for login, a separate raw Ed25519 key for signing the
//! `identity.created` event.

use avalon_sdk::{AvalonClient, AvalonConfig};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::{CredentialCreationOptions, CredentialRequestOptions};
use serde_json::json;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn webauthn_origin() -> String {
    std::env::var("AVALON_WEBAUTHN_ORIGIN").unwrap_or_else(|_| "http://localhost:8080".to_string())
}

/// Must match `avalon-server`'s `handlers::identity_created_signing_bytes`
/// exactly — duplicated here since this test doesn't depend on
/// `avalon-server`.
fn identity_created_signing_bytes(identity_id: Uuid, display_name: &str) -> Vec<u8> {
    format!("avalon:identity.created:v1:{identity_id}:{display_name}").into_bytes()
}

/// Registers a brand-new identity via the real HTTP ceremony, then logs it
/// in, returning the session token — everything a real client (the Hub,
/// eventually) would do before an integrator ever calls `AvalonClient::authenticate()`.
async fn register_and_login(http: &reqwest::Client, base: &str, display_name: &str) -> String {
    let identity_id = Uuid::new_v4();
    let origin_str = webauthn_origin();
    let origin_url =
        url::Url::parse(&origin_str).expect("AVALON_WEBAUTHN_ORIGIN must be a valid URL");

    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let event_signing_public_key = BASE64.encode(signing_key.verifying_key().to_bytes());

    // One authenticator drives both ceremonies below — registration's
    // make_credential and login's get_assertion each check user presence
    // once, hence times(2).
    let store = MemoryStore::new();
    let user_mock = MockUserValidationMethod::verified_user(2);
    let authenticator = Authenticator::new(Aaguid::new_empty(), store, user_mock);
    let mut client = Client::new(authenticator).allows_insecure_localhost(true);

    let start: serde_json::Value = http
        .post(format!("{base}/identities/register/start"))
        .json(&json!({ "identity_id": identity_id, "display_name": display_name }))
        .send()
        .await
        .expect("register/start request failed — is `make start` running?")
        .json()
        .await
        .expect("register/start response was not JSON");
    let ticket_id = start["ticket_id"].as_str().unwrap().to_string();
    let creation_options: CredentialCreationOptions =
        serde_json::from_value(start["challenge"].clone()).expect("bad creation challenge");

    let webauthn_credential = client
        .register(
            Origin::from(&origin_url),
            creation_options,
            DefaultClientData,
        )
        .await
        .expect("WebAuthn registration ceremony failed");

    let signing_bytes = identity_created_signing_bytes(identity_id, display_name);
    let signature = signing_key.sign(&signing_bytes);

    let register_finish = http
        .post(format!("{base}/identities/register/finish"))
        .json(&json!({
            "ticket_id": ticket_id,
            "webauthn_credential": webauthn_credential,
            "event_signing_public_key": event_signing_public_key,
            "event_signature": BASE64.encode(signature.to_bytes()),
        }))
        .send()
        .await
        .expect("register/finish request failed");
    assert!(
        register_finish.status().is_success(),
        "register/finish failed: {:?}",
        register_finish.status()
    );

    let session_start: serde_json::Value = http
        .post(format!("{base}/sessions/start"))
        .json(&json!({ "identity_id": identity_id }))
        .send()
        .await
        .expect("sessions/start request failed")
        .json()
        .await
        .expect("sessions/start response was not JSON");
    let session_ticket_id = session_start["ticket_id"].as_str().unwrap().to_string();
    let request_options: CredentialRequestOptions =
        serde_json::from_value(session_start["challenge"].clone()).expect("bad request challenge");

    let assertion = client
        .authenticate(
            Origin::from(&origin_url),
            request_options,
            DefaultClientData,
        )
        .await
        .expect("WebAuthn authentication ceremony failed");

    let session_finish = http
        .post(format!("{base}/sessions/finish"))
        .json(&json!({ "ticket_id": session_ticket_id, "credential": assertion }))
        .send()
        .await
        .expect("sessions/finish request failed");
    assert!(
        session_finish.status().is_success(),
        "sessions/finish failed: {:?}",
        session_finish.status()
    );
    let login_body: serde_json::Value = session_finish
        .json()
        .await
        .expect("sessions/finish response was not JSON");
    login_body["token"]
        .as_str()
        .expect("sessions/finish response missing token")
        .to_string()
}

#[tokio::test]
#[ignore]
async fn authenticate_against_a_real_server() {
    let base = server_url();
    let http = reqwest::Client::new();
    let display_name = format!("sdk-test-{}", Uuid::new_v4());

    let token = register_and_login(&http, &base, &display_name).await;

    let client = AvalonClient::new(AvalonConfig {
        server_url: base,
        integrator_credential_key_id: "sdk-test".to_string(),
        integrator_slug: None,
        signing_key: None,
        retry: Default::default(),
    });
    let session = client
        .authenticate(&token)
        .await
        .expect("authenticate() should succeed with a valid session token");

    assert_eq!(session.profile().display_name, display_name);

    // A capability that hasn't been granted (permissions aren't implemented
    // yet) must be rejected, not silently allowed.
    let result = session.achievements().await;
    assert!(matches!(
        result,
        Err(avalon_sdk::SdkError::CapabilityNotGranted(_))
    ));
}

#[tokio::test]
#[ignore]
async fn authenticate_rejects_an_invalid_token() {
    let client = AvalonClient::new(AvalonConfig {
        server_url: server_url(),
        integrator_credential_key_id: "sdk-test".to_string(),
        integrator_slug: None,
        signing_key: None,
        retry: Default::default(),
    });

    let result = client.authenticate("not-a-real-token").await;
    assert!(matches!(result, Err(avalon_sdk::SdkError::Unauthorized)));
}
