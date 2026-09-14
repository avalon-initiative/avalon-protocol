//! Exercises integrator registration and the challenge-response integrator-auth flow
//! (issue #26) against a real, running `avalon-server` and Postgres. Gated
//! `--ignored` since it needs live infra — see `make test-live` / `make
//! start`. Skipped in this sandbox per `.claude/CLAUDE.md` (no reachable
//! Postgres here); written but not run against a live database.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

/// A fresh Ed25519 keypair plus the registration body it belongs in — a
/// short random slug/name pair so concurrent test runs never collide on the
/// unique `slug` constraint, mirroring `unique_guild_body()` in
/// `crates/server/tests/guilds.rs`.
struct UnregisteredIntegrator {
    signing_key: SigningKey,
    body: serde_json::Value,
}

fn unique_integrator() -> UnregisteredIntegrator {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let body = serde_json::json!({
        "slug": format!("test-integrator-{}", &suffix[..12]),
        "name": format!("Test Integrator {}", &suffix[..8]),
        "owner_name": "Test Studio",
        "requested_capabilities": ["presence.read", "friends.read"],
        "initial_key": {
            "algorithm": "ed25519",
            "public_key": BASE64.encode(signing_key.verifying_key().as_bytes()),
        },
    });
    UnregisteredIntegrator { signing_key, body }
}

#[tokio::test]
#[ignore]
async fn registering_a_integrator_returns_the_integrator_and_its_credential() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = unique_integrator();

    let response = http
        .post(format!("{base}/integrators"))
        .json(&integrator.body)
        .send()
        .await
        .expect("register integrator failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());

    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["slug"].as_str().unwrap(), integrator.body["slug"]);
    assert_eq!(body["name"].as_str().unwrap(), integrator.body["name"]);
    assert_eq!(
        body["owner_name"].as_str().unwrap(),
        integrator.body["owner_name"]
    );
    assert_eq!(body["status"].as_str().unwrap(), "active");
    // #282: omitted category defaults to "game".
    assert_eq!(body["category"].as_str().unwrap(), "game");
    assert_eq!(body["requested_capabilities"].as_array().unwrap().len(), 2);
    let credential = &body["credential"];
    assert_eq!(
        credential["integrator_id"].as_str().unwrap(),
        body["id"].as_str().unwrap()
    );
    assert!(!credential["key_id"].as_str().unwrap().is_empty());
}

#[tokio::test]
#[ignore]
async fn registering_with_an_explicit_category_is_honored_and_returned_by_get_integrator() {
    let http = reqwest::Client::new();
    let base = server_url();
    let mut integrator = unique_integrator();
    integrator.body["category"] = serde_json::json!("app");

    let register = http
        .post(format!("{base}/integrators"))
        .json(&integrator.body)
        .send()
        .await
        .expect("register integrator failed — is `make start` running?");
    assert!(register.status().is_success(), "{:?}", register.status());
    let registered: serde_json::Value = register.json().await.unwrap();
    assert_eq!(registered["category"].as_str().unwrap(), "app");

    let slug = integrator.body["slug"].as_str().unwrap();
    let get = http
        .get(format!("{base}/integrators/{slug}"))
        .send()
        .await
        .unwrap();
    assert!(get.status().is_success());
    let found: serde_json::Value = get.json().await.unwrap();
    assert_eq!(found["category"].as_str().unwrap(), "app");
}

#[tokio::test]
#[ignore]
async fn registering_with_an_unrecognized_category_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let mut integrator = unique_integrator();
    integrator.body["category"] = serde_json::json!("bogus");

    let response = http
        .post(format!("{base}/integrators"))
        .json(&integrator.body)
        .send()
        .await
        .expect("register integrator failed — is `make start` running?");
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore]
async fn a_second_registration_with_the_same_slug_conflicts() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = unique_integrator();

    let first = http
        .post(format!("{base}/integrators"))
        .json(&integrator.body)
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success());

    // Different developer/key, same slug — should still collide on slug
    // alone, matching the invariant that an integrator cannot register with
    // another integrator's slug.
    let mut second_body = integrator.body.clone();
    second_body["owner_name"] = serde_json::json!("A Different Studio");
    let second = http
        .post(format!("{base}/integrators"))
        .json(&second_body)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test]
#[ignore]
async fn an_uppercase_slug_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let mut integrator = unique_integrator();
    integrator.body["slug"] = serde_json::json!("Not-Lowercase");

    let response = http
        .post(format!("{base}/integrators"))
        .json(&integrator.body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore]
async fn the_challenge_response_auth_flow_round_trips() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = unique_integrator();

    let register = http
        .post(format!("{base}/integrators"))
        .json(&integrator.body)
        .send()
        .await
        .unwrap();
    assert!(register.status().is_success());
    let registered: serde_json::Value = register.json().await.unwrap();
    let slug = registered["slug"].as_str().unwrap();
    let key_id = registered["credential"]["key_id"].as_str().unwrap();

    let challenge = http
        .post(format!("{base}/integrators/{slug}/challenge"))
        .send()
        .await
        .unwrap();
    assert!(challenge.status().is_success(), "{:?}", challenge.status());
    let challenge: serde_json::Value = challenge.json().await.unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();

    let signature = integrator.signing_key.sign(&nonce);

    let whoami = http
        .get(format!("{base}/integrators/whoami"))
        .header("x-avalon-integrator-key-id", key_id)
        .header("x-avalon-integrator-challenge-id", challenge_id)
        .header(
            "x-avalon-integrator-signature",
            BASE64.encode(signature.to_bytes()),
        )
        .send()
        .await
        .unwrap();
    assert!(whoami.status().is_success(), "{:?}", whoami.status());
    let whoami: serde_json::Value = whoami.json().await.unwrap();
    assert_eq!(
        whoami["integrator_id"].as_str().unwrap(),
        registered["id"].as_str().unwrap()
    );
}

#[tokio::test]
#[ignore]
async fn a_challenge_cannot_be_replayed() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = unique_integrator();

    let register = http
        .post(format!("{base}/integrators"))
        .json(&integrator.body)
        .send()
        .await
        .unwrap();
    let registered: serde_json::Value = register.json().await.unwrap();
    let slug = registered["slug"].as_str().unwrap();
    let key_id = registered["credential"]["key_id"].as_str().unwrap();

    let challenge: serde_json::Value = http
        .post(format!("{base}/integrators/{slug}/challenge"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let signature_b64 = BASE64.encode(integrator.signing_key.sign(&nonce).to_bytes());

    let first = http
        .get(format!("{base}/integrators/whoami"))
        .header("x-avalon-integrator-key-id", key_id)
        .header("x-avalon-integrator-challenge-id", challenge_id)
        .header("x-avalon-integrator-signature", &signature_b64)
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success());

    // The same (challenge_id, signature) pair a second time — the
    // challenge row was already consumed by the first request.
    let second = http
        .get(format!("{base}/integrators/whoami"))
        .header("x-avalon-integrator-key-id", key_id)
        .header("x-avalon-integrator-challenge-id", challenge_id)
        .header("x-avalon-integrator-signature", &signature_b64)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore]
async fn a_signature_from_the_wrong_key_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = unique_integrator();

    let register = http
        .post(format!("{base}/integrators"))
        .json(&integrator.body)
        .send()
        .await
        .unwrap();
    let registered: serde_json::Value = register.json().await.unwrap();
    let slug = registered["slug"].as_str().unwrap();
    let key_id = registered["credential"]["key_id"].as_str().unwrap();

    let challenge: serde_json::Value = http
        .post(format!("{base}/integrators/{slug}/challenge"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();

    // Signed with a key the integrator never registered.
    let mut csprng = rand::rng();
    let wrong_key = SigningKey::generate(&mut csprng);
    let signature_b64 = BASE64.encode(wrong_key.sign(&nonce).to_bytes());

    let response = http
        .get(format!("{base}/integrators/whoami"))
        .header("x-avalon-integrator-key-id", key_id)
        .header("x-avalon-integrator-challenge-id", challenge_id)
        .header("x-avalon-integrator-signature", &signature_b64)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}

// --- Issue #270: GET /integrators list + directory read path -------------------

#[tokio::test]
#[ignore]
async fn listing_integrators_finds_a_freshly_registered_integrator_by_name_search() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = unique_integrator();

    let register = http
        .post(format!("{base}/integrators"))
        .json(&integrator.body)
        .send()
        .await
        .expect("register integrator failed — is `make start` running?");
    assert!(register.status().is_success());
    let registered: serde_json::Value = register.json().await.unwrap();
    let slug = registered["slug"].as_str().unwrap();
    let name = registered["name"].as_str().unwrap();

    // No auth header at all — GET /integrators is public and unauthenticated.
    let response = http
        .get(format!("{base}/integrators"))
        .query(&[("q", name), ("sort", "name")])
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());

    let body: serde_json::Value = response.json().await.unwrap();
    let integrators = body["integrators"].as_array().unwrap();
    assert!(integrators.iter().any(|g| g["slug"].as_str() == Some(slug)));
    let found = integrators
        .iter()
        .find(|g| g["slug"].as_str() == Some(slug))
        .unwrap();
    assert_eq!(found["name"].as_str().unwrap(), name);
    assert_eq!(found["status"].as_str().unwrap(), "active");
    // Directory listing returns only the public summary fields — no
    // credential/requested_capabilities noise a card doesn't show.
    assert!(found.get("credential").is_none());
    assert!(found.get("requested_capabilities").is_none());
}

#[tokio::test]
#[ignore]
async fn listing_integrators_paginates_with_cursor_and_next_cursor() {
    let http = reqwest::Client::new();
    let base = server_url();

    for _ in 0..3 {
        let integrator = unique_integrator();
        http.post(format!("{base}/integrators"))
            .json(&integrator.body)
            .send()
            .await
            .unwrap();
    }

    let first_page: serde_json::Value = http
        .get(format!("{base}/integrators"))
        .query(&[("sort", "newest"), ("limit", "1")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let first_integrators = first_page["integrators"].as_array().unwrap();
    assert_eq!(first_integrators.len(), 1);
    let next_cursor = first_page["next_cursor"].as_str();
    assert!(
        next_cursor.is_some(),
        "expected a next_cursor with 3+ integrators registered"
    );

    let second_page: serde_json::Value = http
        .get(format!("{base}/integrators"))
        .query(&[
            ("sort", "newest"),
            ("limit", "1"),
            ("cursor", next_cursor.unwrap()),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let second_integrators = second_page["integrators"].as_array().unwrap();
    assert_eq!(second_integrators.len(), 1);
    // The second page's integrator must differ from the first's — the cursor
    // actually advanced rather than re-returning the same row.
    assert_ne!(first_integrators[0]["id"], second_integrators[0]["id"]);
}

// --- Issue #293: `/integrations` as the canonical path, `/integrators` as a ----
// --- compatibility redirect/dual-route, either auth header name works. --
//
// The tests above all still hit `/integrators` unmodified, proving old-path
// backward compat. These hit `/integrations` directly.

#[tokio::test]
#[ignore]
async fn registering_via_integrations_returns_the_integrator_and_its_credential() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = unique_integrator();

    let response = http
        .post(format!("{base}/integrations"))
        .json(&integrator.body)
        .send()
        .await
        .expect("register integration failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());

    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["slug"].as_str().unwrap(), integrator.body["slug"]);
    assert_eq!(body["name"].as_str().unwrap(), integrator.body["name"]);
    assert_eq!(
        body["owner_name"].as_str().unwrap(),
        integrator.body["owner_name"]
    );
}

#[tokio::test]
#[ignore]
async fn get_integrators_slug_redirects_to_integrations_slug() {
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let base = server_url();
    let integrator = unique_integrator();

    let register = http
        .post(format!("{base}/integrators"))
        .json(&integrator.body)
        .send()
        .await
        .unwrap();
    let registered: serde_json::Value = register.json().await.unwrap();
    let slug = registered["slug"].as_str().unwrap();

    let response = http
        .get(format!("{base}/integrators/{slug}"))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_redirection(),
        "{:?}",
        response.status()
    );
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(location, format!("/integrations/{slug}"));
}

#[tokio::test]
#[ignore]
async fn get_integrators_redirects_to_integrations_preserving_query_string() {
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let base = server_url();

    let response = http
        .get(format!("{base}/integrators"))
        .query(&[("q", "ashen"), ("sort", "name")])
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_redirection(),
        "{:?}",
        response.status()
    );
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(location.starts_with("/integrations?"));
    assert!(location.contains("q=ashen"));
    assert!(location.contains("sort=name"));
}

#[tokio::test]
#[ignore]
async fn get_integrations_slug_serves_directly_without_redirecting() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = unique_integrator();

    let register = http
        .post(format!("{base}/integrations"))
        .json(&integrator.body)
        .send()
        .await
        .unwrap();
    let registered: serde_json::Value = register.json().await.unwrap();
    let slug = registered["slug"].as_str().unwrap();

    let response = http
        .get(format!("{base}/integrations/{slug}"))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["slug"].as_str().unwrap(), slug);
}

#[tokio::test]
#[ignore]
async fn either_auth_header_name_works_for_the_challenge_response_flow() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = unique_integrator();

    let register = http
        .post(format!("{base}/integrations"))
        .json(&integrator.body)
        .send()
        .await
        .unwrap();
    let registered: serde_json::Value = register.json().await.unwrap();
    let slug = registered["slug"].as_str().unwrap();
    let key_id = registered["credential"]["key_id"].as_str().unwrap();

    // New `x-avalon-integrator-*` header names, same challenge-response flow.
    let challenge: serde_json::Value = http
        .post(format!("{base}/integrators/{slug}/challenge"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let signature_b64 = BASE64.encode(integrator.signing_key.sign(&nonce).to_bytes());

    let whoami = http
        .get(format!("{base}/integrators/whoami"))
        .header("x-avalon-integrator-key-id", key_id)
        .header("x-avalon-integrator-challenge-id", challenge_id)
        .header("x-avalon-integrator-signature", &signature_b64)
        .send()
        .await
        .unwrap();
    assert!(whoami.status().is_success(), "{:?}", whoami.status());
    let whoami: serde_json::Value = whoami.json().await.unwrap();
    assert_eq!(
        whoami["integrator_id"].as_str().unwrap(),
        registered["id"].as_str().unwrap()
    );

    // Mixed old/new header names on a second round trip — either name is
    // accepted independently of the others.
    let challenge: serde_json::Value = http
        .post(format!("{base}/integrators/{slug}/challenge"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let signature_b64 = BASE64.encode(integrator.signing_key.sign(&nonce).to_bytes());

    let mixed = http
        .get(format!("{base}/integrators/whoami"))
        .header("x-avalon-integrator-key-id", key_id)
        .header("x-avalon-integrator-challenge-id", challenge_id)
        .header("x-avalon-integrator-signature", &signature_b64)
        .send()
        .await
        .unwrap();
    assert!(mixed.status().is_success(), "{:?}", mixed.status());
}
