//! Exercises `AvalonClient::cross_node_login()`/`CrossNodeLogin::wait()`
//! and `AvalonClient::submit_cross_node_login_grant()` (epic #623, issue
//! #637) against a real, running `avalon-server` (and therefore a real
//! Postgres). Gated `--ignored` — see `make test-live` / `make start`.
//!
//! Unlike `crates/sdk/tests/device_login.rs`'s "approver" (an
//! already-logged-in session, structurally a different identity than the
//! one being paired in), cross-node login's approval is a signed grant
//! from the *same* identity's own real Ed25519 event-signing key — there
//! is no Hub UI to drive yet (#639, not built), so the "approving device"
//! side here is simulated by minting and submitting a grant directly over
//! HTTP, the same way `crates/server/tests/cross_node_login.rs` already
//! does server-side.

use avalon_sdk::{AvalonClient, AvalonConfig, SdkError};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::{CredentialCreationOptions, CredentialRequestOptions};
use serde_json::json;
use uuid::Uuid;

use avalon_sdk::cross_node_login::{signing_bytes, CrossNodeLoginGrant, DEFAULT_TTL_SECONDS};

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn webauthn_origin() -> String {
    std::env::var("AVALON_WEBAUTHN_ORIGIN").unwrap_or_else(|_| "http://localhost:8080".to_string())
}

/// Registers a brand-new identity via the real WebAuthn ceremony, then logs
/// it in once (only to fetch its real `signing_key_id` via `GET
/// /me/devices` — `register/finish`'s own response carries only
/// `identity_id`), returning enough to mint a real grant: the identity id,
/// its display name, its real `SigningKey`, and the `signing_key_id`
/// `avalon-server` assigned it.
async fn register_identity(
    http: &reqwest::Client,
    base: &str,
    display_name: &str,
) -> (Uuid, SigningKey, Uuid) {
    let identity_id = Uuid::new_v4();
    let origin_url =
        url::Url::parse(&webauthn_origin()).expect("AVALON_WEBAUTHN_ORIGIN must be a valid URL");

    let signing_key = SigningKey::generate(&mut rand::rng());
    let event_signing_public_key = BASE64.encode(signing_key.verifying_key().to_bytes());

    // One authenticator drives both ceremonies below — registration's
    // make_credential and login's get_assertion each check user presence
    // once, hence times(2), same as `crates/sdk/tests/authenticate.rs`.
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

    let signing_bytes_for_creation =
        format!("avalon:identity.created:v1:{identity_id}:{display_name}").into_bytes();
    let signature = signing_key.sign(&signing_bytes_for_creation);

    http.post(format!("{base}/identities/register/finish"))
        .json(&json!({
            "ticket_id": ticket_id,
            "webauthn_credential": webauthn_credential,
            "event_signing_public_key": event_signing_public_key,
            "event_signature": BASE64.encode(signature.to_bytes()),
            "device_label": null,
        }))
        .send()
        .await
        .expect("register/finish request failed")
        .error_for_status()
        .expect("register/finish should succeed");

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
    let session_finish: serde_json::Value = http
        .post(format!("{base}/sessions/finish"))
        .json(&json!({ "ticket_id": session_ticket_id, "credential": assertion }))
        .send()
        .await
        .expect("sessions/finish request failed")
        .error_for_status()
        .expect("sessions/finish should succeed")
        .json()
        .await
        .expect("sessions/finish response was not JSON");
    let session_token = session_finish["token"].as_str().unwrap().to_string();

    let devices: serde_json::Value = http
        .get(format!("{base}/me/devices"))
        .bearer_auth(&session_token)
        .send()
        .await
        .expect("me/devices request failed")
        .json()
        .await
        .expect("me/devices response was not JSON");
    let signing_key_id: Uuid = devices
        .as_array()
        .expect("me/devices should return an array")
        .first()
        .expect("a freshly registered identity has exactly one signing key")["id"]
        .as_str()
        .unwrap()
        .parse()
        .expect("signing_key_id should be a valid UUID");

    (identity_id, signing_key, signing_key_id)
}

fn client(base: &str) -> AvalonClient {
    AvalonClient::new(AvalonConfig {
        server_url: base.to_string(),
        integrator_credential_key_id: "sdk-test".to_string(),
        integrator_slug: None,
        signing_key: None,
        retry: Default::default(),
    })
}

fn mint_grant(
    identity_id: Uuid,
    signing_key_id: Uuid,
    signing_key: &SigningKey,
    destination_base_url: &str,
) -> CrossNodeLoginGrant {
    let issued_at = time::OffsetDateTime::now_utc();
    let expires_at = issued_at + time::Duration::seconds(DEFAULT_TTL_SECONDS);
    let nonce = Uuid::new_v4();
    let requesting_context = destination_base_url.to_string();
    let bytes = signing_bytes(
        identity_id,
        signing_key_id,
        destination_base_url,
        &requesting_context,
        nonce,
        issued_at,
        expires_at,
    );
    let signature = signing_key.sign(&bytes);
    CrossNodeLoginGrant {
        identity_id,
        signing_key_id,
        destination_base_url: destination_base_url.to_string(),
        requesting_context,
        nonce,
        issued_at,
        expires_at,
        signature: hex::encode(signature.to_bytes()),
    }
}

/// The cross-device flow: `cross_node_login()` starts a request,
/// `wait()` polls it, and a "simulated Hub" submits a real signed grant
/// against its `user_code` shortly after — same shape
/// `device_login.rs::wait_resolves_to_a_real_session_once_approved`
/// exercises for device pairing.
#[tokio::test]
#[ignore]
async fn wait_resolves_to_a_real_session_once_a_grant_is_submitted() {
    let base = server_url();
    let http = reqwest::Client::new();
    let display_name = format!("sdk-cross-node-login-test-{}", Uuid::new_v4());
    let (identity_id, signing_key, signing_key_id) =
        register_identity(&http, &base, &display_name).await;

    let client = client(&base);
    let pending = client
        .cross_node_login()
        .await
        .expect("cross_node_login() should start a request");
    assert!(!pending.user_code.is_empty());
    assert!(!pending.requesting_context.is_empty());
    assert!(pending.expires_in > 0);

    let user_code = pending.user_code.clone();
    let base_for_submit = base.clone();
    let submit = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let grant = mint_grant(identity_id, signing_key_id, &signing_key, &base_for_submit);
        let response = http
            .post(format!("{base_for_submit}/auth/cross-node/submit"))
            .json(&json!({ "user_code": user_code, "grant": grant }))
            .send()
            .await
            .expect("submit request failed");
        assert!(response.status().is_success(), "{:?}", response.status());
    });

    let session = pending.wait().await.expect("wait() should resolve");
    submit.await.expect("submit task panicked");

    assert_eq!(session.profile().display_name, display_name);
}

#[tokio::test]
#[ignore]
async fn wait_returns_a_typed_error_when_denied() {
    let base = server_url();
    let http = reqwest::Client::new();
    let display_name = format!("sdk-cross-node-login-deny-test-{}", Uuid::new_v4());
    let (_identity_id, _signing_key, _signing_key_id) =
        register_identity(&http, &base, &display_name).await;

    let client = client(&base);
    let pending = client
        .cross_node_login()
        .await
        .expect("cross_node_login() should start a request");

    let user_code = pending.user_code.clone();
    let base_for_deny = base.clone();
    let deny = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let response = http
            .post(format!("{base_for_deny}/auth/cross-node/deny"))
            .json(&json!({ "user_code": user_code }))
            .send()
            .await
            .expect("deny request failed");
        assert!(response.status().is_success(), "{:?}", response.status());
    });

    let result = pending.wait().await;
    deny.await.expect("deny task panicked");

    assert!(matches!(result, Err(SdkError::CrossNodeLoginDenied)));
}

/// The same-device fast path: `submit_cross_node_login_grant` mints,
/// signs, and submits a grant in one call, with no `start`/poll at all,
/// resolving directly to a real `Session`.
#[tokio::test]
#[ignore]
async fn submit_cross_node_login_grant_resolves_directly_to_a_session() {
    let base = server_url();
    let http = reqwest::Client::new();
    let display_name = format!("sdk-cross-node-login-same-device-test-{}", Uuid::new_v4());
    let (identity_id, signing_key, signing_key_id) =
        register_identity(&http, &base, &display_name).await;

    let client = client(&base);
    let session = client
        .submit_cross_node_login_grant(identity_id, signing_key_id, &signing_key)
        .await
        .expect("submit_cross_node_login_grant should resolve to a real session");

    assert_eq!(session.profile().display_name, display_name);
}

/// A grant signed with the wrong key is rejected — proves this isn't just
/// trusting whatever `identity_id`/`signing_key_id` the caller claims.
#[tokio::test]
#[ignore]
async fn submit_cross_node_login_grant_rejects_a_grant_signed_by_the_wrong_key() {
    let base = server_url();
    let http = reqwest::Client::new();
    let display_name = format!("sdk-cross-node-login-wrong-key-test-{}", Uuid::new_v4());
    let (identity_id, _real_signing_key, signing_key_id) =
        register_identity(&http, &base, &display_name).await;

    let impostor_key = SigningKey::generate(&mut rand::rng());
    let client = client(&base);
    let result = client
        .submit_cross_node_login_grant(identity_id, signing_key_id, &impostor_key)
        .await;

    assert!(matches!(result, Err(SdkError::Unauthorized)));
}
