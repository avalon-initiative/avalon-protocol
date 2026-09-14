//! Exercises the App/Service "Milestone" claim-definition routes (issues
//! #324/#325, generalizing #31's Integrator-only "Achievement" CRUD) against a
//! real, running `avalon-server` and Postgres. Gated `--ignored` since it
//! needs live infra — see `make test-live` / `make start`.
//!
//! Deliberately does not re-cover ordinary CRUD mechanics
//! (`crates/server/tests/achievements.rs` already does — the two routes
//! share the same implementation, see `achievements.rs`'s module doc
//! comment). What's actually new here is the category enforcement: a
//! route's claim vocabulary is derived from the issuer's own real
//! registered category, never caller-asserted.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use reqwest::header::HeaderMap;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

struct RegisteredIssuer {
    signing_key: SigningKey,
    slug: String,
    key_id: String,
}

async fn register_issuer(http: &reqwest::Client, base: &str, category: &str) -> RegisteredIssuer {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let slug = format!("test-{}-{}", category, &suffix[..10]);
    let body = serde_json::json!({
        "slug": slug,
        "name": format!("Test {} {}", category, &suffix[..8]),
        "owner_name": "Test Studio",
        "category": category,
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
    let key_id = registered["credential"]["key_id"]
        .as_str()
        .unwrap()
        .to_string();

    RegisteredIssuer {
        signing_key,
        slug,
        key_id,
    }
}

/// Same shape `achievements.rs`'s own `integrator_auth_headers` uses — a new
/// challenge every call, since one is single-use.
async fn auth_headers(http: &reqwest::Client, base: &str, issuer: &RegisteredIssuer) -> HeaderMap {
    let challenge: serde_json::Value = http
        .post(format!("{base}/integrators/{}/challenge", issuer.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let signature = issuer.signing_key.sign(&nonce);

    let mut headers = HeaderMap::new();
    headers.insert("x-avalon-integrator-key-id", issuer.key_id.parse().unwrap());
    headers.insert(
        "x-avalon-integrator-challenge-id",
        challenge_id.parse().unwrap(),
    );
    headers.insert(
        "x-avalon-integrator-signature",
        BASE64.encode(signature.to_bytes()).parse().unwrap(),
    );
    headers
}

#[tokio::test]
#[ignore]
async fn an_app_creates_a_milestone_namespaced_under_its_own_slug() {
    let http = reqwest::Client::new();
    let base = server_url();
    let app = register_issuer(&http, &base, "app").await;

    let headers = auth_headers(&http, &base, &app).await;
    let response = http
        .post(format!("{base}/integrations/{}/milestones", app.slug))
        .headers(headers)
        .json(&serde_json::json!({
            "key": "onboarded",
            "name": "Onboarded",
            "description": "Completed onboarding",
        }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
    let created: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        created["id"].as_str().unwrap(),
        format!("app:{}:milestone:onboarded", app.slug)
    );
}

#[tokio::test]
#[ignore]
async fn a_service_creates_a_milestone_namespaced_under_its_own_slug() {
    let http = reqwest::Client::new();
    let base = server_url();
    let service = register_issuer(&http, &base, "service").await;

    let headers = auth_headers(&http, &base, &service).await;
    let response = http
        .post(format!("{base}/integrations/{}/milestones", service.slug))
        .headers(headers)
        .json(&serde_json::json!({
            "key": "verified",
            "name": "Verified",
            "description": "Identity verified",
        }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
    let created: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        created["id"].as_str().unwrap(),
        format!("service:{}:milestone:verified", service.slug)
    );
}

#[tokio::test]
#[ignore]
async fn an_app_cannot_create_an_achievement_via_the_integrators_route() {
    let http = reqwest::Client::new();
    let base = server_url();
    let app = register_issuer(&http, &base, "app").await;

    let headers = auth_headers(&http, &base, &app).await;
    let response = http
        .post(format!("{base}/integrators/{}/achievements", app.slug))
        .headers(headers)
        .json(&serde_json::json!({
            "key": "sneaky",
            "name": "Sneaky",
            "description": "Should be rejected",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn a_integrator_cannot_create_a_milestone_via_the_integrations_route() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_issuer(&http, &base, "game").await;

    let headers = auth_headers(&http, &base, &integrator).await;
    let response = http
        .post(format!(
            "{base}/integrations/{}/milestones",
            integrator.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "key": "sneaky",
            "name": "Sneaky",
            "description": "Should be rejected",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn listing_achievements_for_an_app_is_rejected_not_silently_empty() {
    // The read side needs the same category check as the write side, or
    // GET /integrators/{app-slug}/achievements would silently serve that app's
    // real milestones back mislabeled as achievements (both live in the
    // same underlying table, keyed by integrator_id — see achievements.rs's
    // module doc comment).
    let http = reqwest::Client::new();
    let base = server_url();
    let app = register_issuer(&http, &base, "app").await;

    let headers = auth_headers(&http, &base, &app).await;
    http.post(format!("{base}/integrations/{}/milestones", app.slug))
        .headers(headers)
        .json(&serde_json::json!({
            "key": "onboarded",
            "name": "Onboarded",
            "description": "Completed onboarding",
        }))
        .send()
        .await
        .unwrap();

    let response = http
        .get(format!("{base}/integrators/{}/achievements", app.slug))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn listing_milestones_for_an_app_is_public_and_unauthenticated() {
    let http = reqwest::Client::new();
    let base = server_url();
    let app = register_issuer(&http, &base, "app").await;

    let headers = auth_headers(&http, &base, &app).await;
    http.post(format!("{base}/integrations/{}/milestones", app.slug))
        .headers(headers)
        .json(&serde_json::json!({
            "key": "onboarded",
            "name": "Onboarded",
            "description": "Completed onboarding",
        }))
        .send()
        .await
        .unwrap();

    let response = http
        .get(format!("{base}/integrations/{}/milestones", app.slug))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
    let list: Vec<serde_json::Value> = response.json().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["key"].as_str().unwrap(), "onboarded");
}
