//! Live checks for the per-principal limit and the header-independent per-IP
//! ceiling. Each test needs its own small limit configured on the server:
//!
//! ```text
//! AVALON_PRINCIPAL_RATE_LIMIT_PER_MINUTE=5 make start
//! AVALON_PRINCIPAL_RATE_LIMIT_PER_MINUTE=5 cargo test -p avalon-server \
//!     --test rate_limit_layers principal -- --ignored
//!
//! AVALON_RATE_LIMIT_PER_MINUTE=5 make start
//! AVALON_RATE_LIMIT_PER_MINUTE=5 cargo test -p avalon-server \
//!     --test rate_limit_layers rotating -- --ignored
//! ```

use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::{CredentialCreationOptions, CredentialRequestOptions};
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn hub_origin() -> String {
    std::env::var("AVALON_HUB_ORIGIN")
        .ok()
        .and_then(|o| o.split(',').next().map(|s| s.trim().to_string()))
        .unwrap_or_else(|| "http://localhost:5173".to_string())
}

fn rp_origin() -> url::Url {
    let origin = std::env::var("AVALON_WEBAUTHN_ORIGIN")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    url::Url::parse(&origin).expect("AVALON_WEBAUTHN_ORIGIN must be a valid URL")
}

/// Registers a fresh identity and returns a logged-in session token.
async fn create_identity_and_log_in(http: &reqwest::Client, base: &str) -> String {
    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine;

    let identity_id = Uuid::new_v4();
    let display_name = format!("principal-limit-test-{identity_id}");
    let start: serde_json::Value = http
        .post(format!("{base}/identities/register/start"))
        .json(&serde_json::json!({ "identity_id": identity_id, "display_name": display_name }))
        .send()
        .await
        .expect("register/start failed — is `make start` running?")
        .json()
        .await
        .unwrap();
    let ticket_id = start["ticket_id"].as_str().unwrap().to_string();

    let mut client = Client::new(Authenticator::new(
        Aaguid::new_empty(),
        MemoryStore::new(),
        MockUserValidationMethod::verified_user(2),
    ))
    .allows_insecure_localhost(true);
    let origin = rp_origin();
    let creation_options: CredentialCreationOptions =
        serde_json::from_value(start["challenge"].clone()).unwrap();
    let credential = client
        .register(Origin::from(&origin), creation_options, DefaultClientData)
        .await
        .expect("virtual authenticator registration should succeed");

    let signing_key = SigningKey::generate(&mut rand::rng());
    let signature = signing_key
        .sign(format!("avalon:identity.created:v1:{identity_id}:{display_name}").as_bytes());
    http.post(format!("{base}/identities/register/finish"))
        .json(&serde_json::json!({
            "ticket_id": ticket_id,
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

    let session_start: serde_json::Value = http
        .post(format!("{base}/sessions/start"))
        .json(&serde_json::json!({ "identity_id": identity_id }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let request_options: CredentialRequestOptions =
        serde_json::from_value(session_start["challenge"].clone()).unwrap();
    let assertion = client
        .authenticate(Origin::from(&origin), request_options, DefaultClientData)
        .await
        .expect("virtual authenticator authentication should succeed");
    let finish: serde_json::Value = http
        .post(format!("{base}/sessions/finish"))
        .json(&serde_json::json!({
            "ticket_id": session_start["ticket_id"],
            "credential": assertion,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    finish["token"].as_str().unwrap().to_string()
}

#[tokio::test]
#[ignore]
async fn principal_limit_gives_each_identity_its_own_budget_from_one_ip() {
    let Some(per_minute) = std::env::var("AVALON_PRINCIPAL_RATE_LIMIT_PER_MINUTE")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    else {
        eprintln!(
            "skipping: set AVALON_PRINCIPAL_RATE_LIMIT_PER_MINUTE (e.g. 5) before `make start`"
        );
        return;
    };
    let http = reqwest::Client::new();
    let base = server_url();
    let token_a = create_identity_and_log_in(&http, &base).await;
    let token_b = create_identity_and_log_in(&http, &base).await;

    let get = |token: &str| {
        http.get(format!("{base}/friends"))
            .header(reqwest::header::ORIGIN, hub_origin())
            .bearer_auth(token)
            .send()
    };

    let mut limited = None;
    for _ in 0..(per_minute * 2) {
        let response = get(&token_a).await.unwrap();
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            limited = Some(response);
            break;
        }
        assert!(response.status().is_success());
    }
    let limited = limited.expect("identity A never hit its per-principal limit");
    assert!(limited.headers().contains_key(reqwest::header::RETRY_AFTER));
    assert!(limited
        .headers()
        .contains_key(reqwest::header::ACCESS_CONTROL_ALLOW_ORIGIN));

    // Identity B shares A's address but has an untouched budget.
    for _ in 0..per_minute {
        let response = get(&token_b).await.unwrap();
        assert!(
            response.status().is_success(),
            "identity B was throttled by identity A's traffic: {:?}",
            response.status()
        );
    }
}

#[tokio::test]
#[ignore]
async fn rotating_integrator_key_id_header_still_hits_the_per_ip_ceiling() {
    let Some(per_minute) = std::env::var("AVALON_RATE_LIMIT_PER_MINUTE")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    else {
        eprintln!("skipping: set AVALON_RATE_LIMIT_PER_MINUTE (e.g. 5) before `make start`");
        return;
    };
    let http = reqwest::Client::new();
    let base = server_url();

    let mut saw_429 = false;
    for _ in 0..(per_minute * 2) {
        let response = http
            .get(format!("{base}/ledger/sth/latest"))
            .header("x-avalon-integrator-key-id", Uuid::new_v4().to_string())
            .send()
            .await
            .expect("request failed — is `make start` running?");
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            saw_429 = true;
            break;
        }
    }
    assert!(
        saw_429,
        "a fresh header value per request must not yield a fresh bucket"
    );
}
