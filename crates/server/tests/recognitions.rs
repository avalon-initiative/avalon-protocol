//! Exercises public recognition relationships (issue #89) against a real,
//! running `avalon-server` and Postgres. Gated `--ignored` since it needs
//! live infra — see `make test-live` / `make start`.
//!
//! Reuses `crates/server/tests/integrator_schemas.rs`'s own
//! `register_integrator`/`integrator_auth_headers` challenge-response
//! helpers, duplicated here rather than shared — this test suite's own
//! established convention (see that file's module doc comment).

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
    let slug = format!("test-recog-{}", &suffix[..12]);
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

    RegisteredIntegrator {
        signing_key,
        slug,
        key_id,
    }
}

async fn integrator_auth_headers(
    http: &reqwest::Client,
    base: &str,
    integrator: &RegisteredIntegrator,
) -> HeaderMap {
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
async fn publish_read_and_revoke_a_recognition_round_trips() {
    let http = reqwest::Client::new();
    let base = server_url();

    let a = register_integrator(&http, &base).await;
    let b = register_integrator(&http, &base).await;

    let headers = integrator_auth_headers(&http, &base, &a).await;
    let publish = http
        .post(format!("{base}/integrations/{}/recognitions", a.slug))
        .headers(headers)
        .json(&serde_json::json!({
            "recognized_slug": b.slug,
            "scope": ["achievements", "tournament_results"],
        }))
        .send()
        .await
        .unwrap();
    assert!(publish.status().is_success(), "{:?}", publish.status());

    // A recognizes B: shows up in A's own "who do I recognize" list...
    let recognitions: serde_json::Value = http
        .get(format!("{base}/integrations/{}/recognitions", a.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let recognitions = recognitions.as_array().unwrap();
    assert_eq!(recognitions.len(), 1);
    assert_eq!(recognitions[0]["recognized_slug"], b.slug);
    assert_eq!(
        recognitions[0]["scope"],
        serde_json::json!(["achievements", "tournament_results"])
    );

    // ...and in B's own "who recognizes me" list.
    let recognized_by: serde_json::Value = http
        .get(format!("{base}/integrations/{}/recognized-by", b.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let recognized_by = recognized_by.as_array().unwrap();
    assert_eq!(recognized_by.len(), 1);
    assert_eq!(recognized_by[0]["recognizer_slug"], a.slug);

    // Not symmetric: B does not automatically recognize A.
    let b_recognitions: serde_json::Value = http
        .get(format!("{base}/integrations/{}/recognitions", b.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(b_recognitions.as_array().unwrap().is_empty());

    // Revoke: disappears from both directional reads, but the row itself
    // (not asserted here directly) is kept, not deleted.
    let headers = integrator_auth_headers(&http, &base, &a).await;
    let revoke = http
        .post(format!(
            "{base}/integrations/{}/recognitions/revoke",
            a.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({ "recognized_slug": b.slug }))
        .send()
        .await
        .unwrap();
    assert!(revoke.status().is_success(), "{:?}", revoke.status());

    let recognitions_after: serde_json::Value = http
        .get(format!("{base}/integrations/{}/recognitions", a.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(recognitions_after.as_array().unwrap().is_empty());
}

#[tokio::test]
#[ignore]
async fn recognizing_a_different_integrators_slug_as_the_caller_is_forbidden() {
    let http = reqwest::Client::new();
    let base = server_url();

    let a = register_integrator(&http, &base).await;
    let b = register_integrator(&http, &base).await;
    let c = register_integrator(&http, &base).await;

    // Authenticated as `a`, but the path names `b` — must be rejected, not
    // silently attributed to `a` or `b`.
    let headers = integrator_auth_headers(&http, &base, &a).await;
    let response = http
        .post(format!("{base}/integrations/{}/recognitions", b.slug))
        .headers(headers)
        .json(&serde_json::json!({ "recognized_slug": c.slug, "scope": ["achievements"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn recognizing_yourself_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();

    let a = register_integrator(&http, &base).await;
    let headers = integrator_auth_headers(&http, &base, &a).await;
    let response = http
        .post(format!("{base}/integrations/{}/recognitions", a.slug))
        .headers(headers)
        .json(&serde_json::json!({ "recognized_slug": a.slug, "scope": ["achievements"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}
