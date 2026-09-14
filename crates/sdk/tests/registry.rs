//! Exercises `AvalonClient::registry()` (issue #95) against a real, running
//! `avalon-server` and Postgres. Gated `--ignored` since it needs live
//! infra — see `make test-live` / `make start`.

use avalon_sdk::{AvalonClient, AvalonConfig};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::SigningKey;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn client() -> AvalonClient {
    AvalonClient::new(AvalonConfig {
        server_url: server_url(),
        integrator_credential_key_id: String::new(),
        integrator_slug: None,
        signing_key: None,
        retry: Default::default(),
    })
}

/// No `Session`/`authenticate()` needed at all — `registry()` is public and
/// unauthenticated on the server side, matching `AvalonClient::authenticate`
/// itself being the only thing callable before a `Session` exists.
async fn register_unique_integrator(http: &reqwest::Client, base: &str) -> String {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let slug = format!("sdk-registry-test-{}", &suffix[..12]);
    let body = serde_json::json!({
        "slug": slug,
        "name": format!("SDK Registry Test {}", &suffix[..8]),
        "owner_name": "Test Studio",
        "category": "game",
        "requested_capabilities": [],
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
    slug
}

#[tokio::test]
#[ignore]
async fn registry_returns_zeroed_labeled_metrics_for_a_fresh_integrator() {
    let http = reqwest::Client::new();
    let base = server_url();
    let slug = register_unique_integrator(&http, &base).await;

    let registry = client()
        .registry(&slug)
        .await
        .expect("registry() should succeed for a real, freshly registered integrator");

    assert_eq!(registry.players.value, 0);
    assert_eq!(registry.players.class, "durable-derived");
    assert!(!registry.players.definition.is_empty());
    assert!(registry.players.exact, "zero must always be exact");
    assert_eq!(registry.total_players_ever.value, 0);
    assert_eq!(registry.achievements_issued.value, 0);
    assert_eq!(registry.achievements_revoked.value, 0);
    assert_eq!(registry.unique_achievement_holders.value, 0);
}

#[tokio::test]
#[ignore]
async fn registry_errors_for_an_unregistered_slug() {
    let err = client()
        .registry(&format!("does-not-exist-{}", Uuid::new_v4()))
        .await
        .expect_err("an unregistered slug must not succeed");
    assert!(matches!(err, avalon_sdk::SdkError::NotFound(_)));
}
