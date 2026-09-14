//! Exercises `Session::achievements()`/`issue_achievement()` (issue #34)
//! against a real, running `avalon-server` and Postgres. Gated `--ignored`
//! since it needs live infra — see `make test-live` / `make start`.
//!
//! Setup mirrors real usage: a real WebAuthn ceremony creates the user
//! identity (same helper `authenticate.rs` uses, duplicated here rather
//! than shared — see that file's own comment on why), a real integrator
//! registers and the user consents to it, then the SDK — never the raw
//! HTTP API — issues an achievement to itself and reads its own history
//! back.

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
/// in, returning the session token — same shape as `authenticate.rs`'s own
/// helper.
async fn register_and_login(http: &reqwest::Client, base: &str, display_name: &str) -> String {
    let identity_id = Uuid::new_v4();
    let origin_str = webauthn_origin();
    let origin_url =
        url::Url::parse(&origin_str).expect("AVALON_WEBAUTHN_ORIGIN must be a valid URL");

    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let event_signing_public_key = BASE64.encode(signing_key.verifying_key().to_bytes());

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

struct RegisteredIntegrator {
    signing_key: SigningKey,
    slug: String,
    key_id: String,
}

async fn register_integrator(http: &reqwest::Client, base: &str) -> RegisteredIntegrator {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let slug = format!("sdk-achv-{}", &suffix[..10]);
    let body = json!({
        "slug": slug,
        "name": format!("SDK Achievements Test {}", &suffix[..8]),
        "owner_name": "Test Studio",
        "requested_capabilities": ["achievements.issue", "achievements.read"],
        "initial_key": {
            "algorithm": "ed25519",
            "public_key": BASE64.encode(signing_key.verifying_key().as_bytes()),
        },
    });
    let response = http
        .post(format!("{base}/integrations"))
        .json(&body)
        .send()
        .await
        .expect("register integrator failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());
    let registered: serde_json::Value = response.json().await.unwrap();
    RegisteredIntegrator {
        signing_key,
        slug,
        key_id: registered["credential"]["key_id"]
            .as_str()
            .unwrap()
            .to_string(),
    }
}

async fn define_achievement(
    http: &reqwest::Client,
    base: &str,
    integrator: &RegisteredIntegrator,
    key: &str,
) {
    let challenge: serde_json::Value = http
        .post(format!("{base}/integrations/{}/challenge", integrator.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let signature = integrator.signing_key.sign(&nonce);

    let response = http
        .post(format!(
            "{base}/integrations/{}/achievements",
            integrator.slug
        ))
        .header("x-avalon-integrator-key-id", &integrator.key_id)
        .header("x-avalon-integrator-challenge-id", challenge_id)
        .header(
            "x-avalon-integrator-signature",
            BASE64.encode(signature.to_bytes()),
        )
        .json(&json!({
            "key": key,
            "name": "Dragon Slayer",
            "description": "Slew the dragon",
        }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
}

#[tokio::test]
#[ignore]
async fn issue_achievement_then_read_it_back_via_the_sdk() {
    let http = reqwest::Client::new();
    let base = server_url();
    let display_name = format!("sdk-achv-{}", Uuid::new_v4());

    let token = register_and_login(&http, &base, &display_name).await;
    let integrator = register_integrator(&http, &base).await;
    define_achievement(&http, &base, &integrator, "dragon_slayer").await;

    // The user's own consent: an active binding plus grants for both
    // capabilities the SDK's two calls below each require.
    let connect = http
        .post(format!("{base}/integrations/{}/connect", integrator.slug))
        .bearer_auth(&token)
        .json(&json!({ "capabilities": ["achievements.issue", "achievements.read"] }))
        .send()
        .await
        .unwrap();
    assert!(connect.status().is_success(), "{:?}", connect.status());

    let client = AvalonClient::new(AvalonConfig {
        server_url: base,
        integrator_credential_key_id: integrator.key_id.clone(),
        integrator_slug: Some(integrator.slug.clone()),
        signing_key: Some(integrator.signing_key.to_bytes()),
    });
    let session = client
        .authenticate(&token)
        .await
        .expect("authenticate() should succeed with a valid session token");

    let attestation_id = session
        .issue_achievement("dragon_slayer")
        .await
        .expect("issue_achievement should succeed once granted");

    let history = session
        .achievements()
        .await
        .expect("achievements() should succeed once granted");
    assert_eq!(history.len(), 1);
    let attestation = &history[0];
    assert_eq!(attestation.id, attestation_id);
    assert!(matches!(
        attestation.authenticity,
        avalon_sdk::achievements::Authenticity::Authentic { .. }
    ));
    assert!(matches!(
        attestation.validity,
        avalon_sdk::achievements::Validity::Valid
    ));
    assert_eq!(attestation.history.len(), 1);
    assert_eq!(attestation.history[0].event, "issued");
}

#[tokio::test]
#[ignore]
async fn issue_achievement_without_a_configured_signing_key_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let display_name = format!("sdk-achv-nokey-{}", Uuid::new_v4());
    let token = register_and_login(&http, &base, &display_name).await;
    let integrator = register_integrator(&http, &base).await;
    define_achievement(&http, &base, &integrator, "dragon_slayer").await;

    let connect = http
        .post(format!("{base}/integrations/{}/connect", integrator.slug))
        .bearer_auth(&token)
        .json(&json!({ "capabilities": ["achievements.issue"] }))
        .send()
        .await
        .unwrap();
    assert!(connect.status().is_success(), "{:?}", connect.status());

    // No `integrator_slug`/`signing_key` configured — the SDK never even
    // attempts an HTTP call in this case (see `SdkError::MissingIssuerCredentials`'s
    // own doc comment).
    let client = AvalonClient::new(AvalonConfig {
        server_url: base,
        integrator_credential_key_id: integrator.key_id.clone(),
        integrator_slug: None,
        signing_key: None,
    });
    let session = client.authenticate(&token).await.unwrap();

    let result = session.issue_achievement("dragon_slayer").await;
    assert!(matches!(
        result,
        Err(avalon_sdk::SdkError::MissingIssuerCredentials)
    ));
}
