//! Exercises `AvalonClient::authenticate()` against a real, running
//! `avalon-server` (and therefore a real Postgres). Gated on `--ignored`
//! since it needs live infra — see `make test-live` / `make start`.
//!
//! Setup mirrors real usage: this SDK never creates identities or logs a
//! player in itself (that's the Hub's job), so the test hits the raw HTTP
//! endpoints for that part, exactly as the Hub would, then uses the SDK only
//! for the game-side `authenticate()` call.

use avalon_sdk::{AvalonClient, AvalonConfig};

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

#[tokio::test]
#[ignore]
async fn authenticate_against_a_real_server() {
    let base = server_url();
    let http = reqwest::Client::new();
    let username = format!("sdk-test-{}", uuid::Uuid::new_v4());

    let register = http
        .post(format!("{base}/identities"))
        .json(&serde_json::json!({ "username": username, "password": "sdk-test-password" }))
        .send()
        .await
        .expect("register request failed — is `make start` running?");
    assert!(
        register.status().is_success(),
        "register failed: {:?}",
        register.status()
    );

    let login = http
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({ "username": username, "password": "sdk-test-password" }))
        .send()
        .await
        .expect("login request failed");
    assert!(
        login.status().is_success(),
        "login failed: {:?}",
        login.status()
    );
    let login_body: serde_json::Value = login.json().await.expect("login response was not JSON");
    let token = login_body["token"]
        .as_str()
        .expect("login response missing token");

    let client = AvalonClient::new(AvalonConfig {
        server_url: base,
        game_credential_key_id: "sdk-test".to_string(),
    });
    let session = client
        .authenticate(token)
        .await
        .expect("authenticate() should succeed with a valid session token");

    assert_eq!(session.profile().display_name, username);

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
        game_credential_key_id: "sdk-test".to_string(),
    });

    let result = client.authenticate("not-a-real-token").await;
    assert!(matches!(
        result,
        Err(avalon_sdk::SdkError::AuthenticationFailed)
    ));
}
