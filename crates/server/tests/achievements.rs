//! Exercises `AchievementDefinition` CRUD per integrator (issue #31) against a
//! real, running `avalon-server` and Postgres. Gated `--ignored` since it
//! needs live infra — see `make test-live` / `make start`. Skipped in this
//! sandbox per `.claude/CLAUDE.md` (no reachable Postgres here); written but
//! not run against a live database.
//!
//! Mirrors `crates/server/tests/integrators.rs`'s own pattern for the
//! challenge-response integrator-auth flow: each integrator-authenticated request needs
//! its own fresh, single-use challenge, so [`integrator_auth_headers`] mints one
//! per call rather than caching headers across requests.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use reqwest::header::HeaderMap;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
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
    let slug = format!("test-ach-{}", &suffix[..12]);
    let body = serde_json::json!({
        "slug": slug,
        "name": format!("Test Integrator {}", &suffix[..8]),
        "owner_name": "Test Studio",
        "requested_capabilities": [],
        "initial_key": {
            "algorithm": "ed25519",
            "public_key": BASE64.encode(signing_key.verifying_key().as_bytes()),
        },
    });

    let response = http
        .post(format!("{base}/integrators"))
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

    RegisteredIntegrator {
        signing_key,
        slug,
        key_id,
    }
}

/// Mints a fresh challenge for `integrator` and signs it, returning the three
/// `x-avalon-integrator-*` headers `integrators::authenticate_integrator` verifies — a new
/// set every call, since a challenge is single-use.
async fn integrator_auth_headers(
    http: &reqwest::Client,
    base: &str,
    integrator: &RegisteredIntegrator,
) -> HeaderMap {
    let challenge: serde_json::Value = http
        .post(format!("{base}/integrators/{}/challenge", integrator.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let signature = integrator.signing_key.sign(&nonce);

    let mut headers = HeaderMap::new();
    headers.insert(
        "x-avalon-integrator-key-id",
        integrator.key_id.parse().unwrap(),
    );
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
async fn two_integrators_defining_the_same_key_both_succeed_with_distinct_ids() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator_a = register_integrator(&http, &base).await;
    let integrator_b = register_integrator(&http, &base).await;

    let body = serde_json::json!({
        "key": "dragon_slayer",
        "name": "Dragon Slayer",
        "description": "Slew the dragon",
    });

    let headers_a = integrator_auth_headers(&http, &base, &integrator_a).await;
    let response_a = http
        .post(format!(
            "{base}/integrators/{}/achievements",
            integrator_a.slug
        ))
        .headers(headers_a)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(
        response_a.status().is_success(),
        "{:?}",
        response_a.status()
    );
    let def_a: serde_json::Value = response_a.json().await.unwrap();

    let headers_b = integrator_auth_headers(&http, &base, &integrator_b).await;
    let response_b = http
        .post(format!(
            "{base}/integrators/{}/achievements",
            integrator_b.slug
        ))
        .headers(headers_b)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(
        response_b.status().is_success(),
        "{:?}",
        response_b.status()
    );
    let def_b: serde_json::Value = response_b.json().await.unwrap();

    assert_ne!(def_a["id"], def_b["id"]);
    assert_eq!(
        def_a["id"].as_str().unwrap(),
        format!("game:{}:achievement:dragon_slayer", integrator_a.slug)
    );
    assert_eq!(
        def_b["id"].as_str().unwrap(),
        format!("game:{}:achievement:dragon_slayer", integrator_b.slug)
    );
    assert_eq!(def_a["version"], 1);
}

#[tokio::test]
#[ignore]
async fn defining_a_duplicate_key_for_the_same_integrator_conflicts() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base).await;

    let body = serde_json::json!({
        "key": "dragon_slayer",
        "name": "Dragon Slayer",
        "description": "Slew the dragon",
    });

    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let first = http
        .post(format!(
            "{base}/integrators/{}/achievements",
            integrator.slug
        ))
        .headers(headers)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success());

    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let second = http
        .post(format!(
            "{base}/integrators/{}/achievements",
            integrator.slug
        ))
        .headers(headers)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test]
#[ignore]
async fn a_integrator_defining_under_another_slug_is_forbidden() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator_a = register_integrator(&http, &base).await;
    let integrator_b = register_integrator(&http, &base).await;

    let body = serde_json::json!({
        "key": "dragon_slayer",
        "name": "Dragon Slayer",
        "description": "Slew the dragon",
    });

    // integrator_a's real credential, but the path names integrator_b's slug.
    let headers = integrator_auth_headers(&http, &base, &integrator_a).await;
    let response = http
        .post(format!(
            "{base}/integrators/{}/achievements",
            integrator_b.slug
        ))
        .headers(headers)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn updating_a_definition_bumps_version_and_leaves_the_id_unchanged() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base).await;

    let create_body = serde_json::json!({
        "key": "dragon_slayer",
        "name": "Dragon Slayer",
        "description": "Slew the dragon",
    });
    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let created: serde_json::Value = http
        .post(format!(
            "{base}/integrators/{}/achievements",
            integrator.slug
        ))
        .headers(headers)
        .json(&create_body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(created["version"], 1);

    let update_body = serde_json::json!({
        "description": "Slew the Dragon Lord in single combat",
    });
    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let updated_response = http
        .patch(format!(
            "{base}/integrators/{}/achievements/dragon_slayer",
            integrator.slug
        ))
        .headers(headers)
        .json(&update_body)
        .send()
        .await
        .unwrap();
    assert!(updated_response.status().is_success());
    let updated: serde_json::Value = updated_response.json().await.unwrap();

    assert_eq!(updated["id"], created["id"]);
    assert_eq!(updated["version"], 2);
    assert_eq!(
        updated["description"].as_str().unwrap(),
        "Slew the Dragon Lord in single combat"
    );
}

#[tokio::test]
#[ignore]
async fn listing_a_integrators_achievements_is_public_and_unauthenticated() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base).await;

    let create_body = serde_json::json!({
        "key": "dragon_slayer",
        "name": "Dragon Slayer",
        "description": "Slew the dragon",
    });
    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    http.post(format!(
        "{base}/integrators/{}/achievements",
        integrator.slug
    ))
    .headers(headers)
    .json(&create_body)
    .send()
    .await
    .unwrap();

    // No auth headers at all — this is a public read.
    let list_response = http
        .get(format!(
            "{base}/integrators/{}/achievements",
            integrator.slug
        ))
        .send()
        .await
        .unwrap();
    assert!(list_response.status().is_success());
    let list: Vec<serde_json::Value> = list_response.json().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["key"], "dragon_slayer");
    assert_eq!(list[0]["retired"], false);
}

#[tokio::test]
#[ignore]
async fn retiring_a_definition_marks_it_retired_without_deleting_it() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base).await;

    let create_body = serde_json::json!({
        "key": "dragon_slayer",
        "name": "Dragon Slayer",
        "description": "Slew the dragon",
    });
    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    http.post(format!(
        "{base}/integrators/{}/achievements",
        integrator.slug
    ))
    .headers(headers)
    .json(&create_body)
    .send()
    .await
    .unwrap();

    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let retire_response = http
        .patch(format!(
            "{base}/integrators/{}/achievements/dragon_slayer",
            integrator.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({ "retired": true }))
        .send()
        .await
        .unwrap();
    assert!(retire_response.status().is_success());
    let retired: serde_json::Value = retire_response.json().await.unwrap();
    assert_eq!(retired["retired"], true);
    // Retiring never changes the definition or version.
    assert_eq!(retired["version"], 1);

    let list_response = http
        .get(format!(
            "{base}/integrators/{}/achievements",
            integrator.slug
        ))
        .send()
        .await
        .unwrap();
    let list: Vec<serde_json::Value> = list_response.json().await.unwrap();
    assert_eq!(list.len(), 1, "retiring never deletes the definition");
    assert_eq!(list[0]["retired"], true);
}

#[tokio::test]
#[ignore]
async fn a_definition_with_neither_icon_field_set_still_gets_a_default_icon() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base).await;

    let create_body = serde_json::json!({
        "key": "dragon_slayer",
        "name": "Dragon Slayer",
        "description": "Slew the dragon",
    });
    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let response = http
        .post(format!(
            "{base}/integrators/{}/achievements",
            integrator.slug
        ))
        .headers(headers)
        .json(&create_body)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let def: serde_json::Value = response.json().await.unwrap();
    assert_eq!(def["icon"], "trophy");
    assert_eq!(def["icon_url"], serde_json::Value::Null);
}

#[tokio::test]
#[ignore]
async fn icon_url_takes_precedence_and_round_trips_through_create_and_update() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base).await;

    let create_body = serde_json::json!({
        "key": "dragon_slayer",
        "name": "Dragon Slayer",
        "description": "Slew the dragon",
        "icon": "sword",
        "icon_url": "https://cdn.example.com/dragon-slayer.png",
    });
    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let response = http
        .post(format!(
            "{base}/integrators/{}/achievements",
            integrator.slug
        ))
        .headers(headers)
        .json(&create_body)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let def: serde_json::Value = response.json().await.unwrap();
    assert_eq!(def["icon"], "sword");
    assert_eq!(def["icon_url"], "https://cdn.example.com/dragon-slayer.png");

    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let update_response = http
        .patch(format!(
            "{base}/integrators/{}/achievements/dragon_slayer",
            integrator.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({ "icon": "shield" }))
        .send()
        .await
        .unwrap();
    assert!(update_response.status().is_success());
    let updated: serde_json::Value = update_response.json().await.unwrap();
    assert_eq!(updated["icon"], "shield");
    // icon_url was left untouched by the update (field omitted).
    assert_eq!(
        updated["icon_url"],
        "https://cdn.example.com/dragon-slayer.png"
    );
    assert_eq!(updated["version"], 2, "changing icon bumps version");
}

#[tokio::test]
#[ignore]
async fn a_non_http_icon_url_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base).await;

    let create_body = serde_json::json!({
        "key": "dragon_slayer",
        "name": "Dragon Slayer",
        "description": "Slew the dragon",
        "icon_url": "javascript:alert(1)",
    });
    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let response = http
        .post(format!(
            "{base}/integrators/{}/achievements",
            integrator.slug
        ))
        .headers(headers)
        .json(&create_body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore]
async fn an_unknown_icon_key_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base).await;

    let create_body = serde_json::json!({
        "key": "dragon_slayer",
        "name": "Dragon Slayer",
        "description": "Slew the dragon",
        "icon": "not_a_real_icon",
    });
    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let response = http
        .post(format!(
            "{base}/integrators/{}/achievements",
            integrator.slug
        ))
        .headers(headers)
        .json(&create_body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}
