//! Exercises the schema-to-schema mapping model (issue #491) against a
//! real, running `avalon-server` and Postgres. Gated `--ignored` since it
//! needs live infra — see `make test-live` / `make start`.
//!
//! Mirrors `crates/server/tests/integrator_schemas.rs`'s own
//! challenge-response helper pattern.

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
    let slug = format!("test-mapping-{}", &suffix[..12]);
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

const PROTO_V1: &str =
    "syntax = \"proto3\"; message Character { uint32 level = 1; uint64 xp = 2; }";
const PROTO_V2: &str = "syntax = \"proto3\"; message Character { uint32 progression_rank = 1; uint64 progression_experience = 2; }";

async fn publish_schema(
    http: &reqwest::Client,
    base: &str,
    integrator: &RegisteredIntegrator,
    proto_source: &str,
) -> String {
    let headers = integrator_auth_headers(http, base, integrator).await;
    let published: serde_json::Value = http
        .post(format!("{base}/integrations/{}/schemas", integrator.slug))
        .headers(headers)
        .json(&serde_json::json!({ "proto_source": proto_source }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    published["id"].as_str().unwrap().to_string()
}

#[tokio::test]
#[ignore]
async fn publishing_a_mapping_round_trips_and_is_fetchable() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base).await;
    let v1 = publish_schema(&http, &base, &integrator, PROTO_V1).await;
    let v2 = publish_schema(&http, &base, &integrator, PROTO_V2).await;

    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let published: serde_json::Value = http
        .post(format!("{base}/integrations/{}/mappings", integrator.slug))
        .headers(headers)
        .json(&serde_json::json!({
            "from_schema_id": v1,
            "to_schema_id": v2,
            "description": "progression fields were renamed",
            "field_correspondence": { "level": "progression_rank", "xp": "progression_experience" },
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(published["from_schema_id"], v1);
    assert_eq!(published["to_schema_id"], v2);
    assert_eq!(
        published["id"].as_str().unwrap(),
        format!("game:{}:schema_mapping:1", integrator.slug)
    );

    let fetched: serde_json::Value = http
        .get(format!(
            "{base}/integrations/{}/mappings/1",
            integrator.slug
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fetched["id"], published["id"]);
    assert_eq!(
        fetched["field_correspondence"]["level"].as_str().unwrap(),
        "progression_rank"
    );
}

#[tokio::test]
#[ignore]
async fn publishing_a_mapping_against_a_schema_not_owned_by_the_caller_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let owner = register_integrator(&http, &base).await;
    let other = register_integrator(&http, &base).await;
    let owner_v1 = publish_schema(&http, &base, &owner, PROTO_V1).await;
    let other_v1 = publish_schema(&http, &base, &other, PROTO_V1).await;

    // `other` tries to publish a mapping from its own schema to one it
    // doesn't own (`owner`'s) — must be rejected, not silently accepted.
    let headers = integrator_auth_headers(&http, &base, &other).await;
    let response = http
        .post(format!("{base}/integrations/{}/mappings", other.slug))
        .headers(headers)
        .json(&serde_json::json!({
            "from_schema_id": other_v1,
            "to_schema_id": owner_v1,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn publishing_a_mapping_against_a_nonexistent_schema_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base).await;
    let v1 = publish_schema(&http, &base, &integrator, PROTO_V1).await;

    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let response = http
        .post(format!("{base}/integrations/{}/mappings", integrator.slug))
        .headers(headers)
        .json(&serde_json::json!({
            "from_schema_id": v1,
            "to_schema_id": format!("game:{}:schema:999", integrator.slug),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore]
async fn publishing_a_mapping_with_identical_from_and_to_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base).await;
    let v1 = publish_schema(&http, &base, &integrator, PROTO_V1).await;

    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let response = http
        .post(format!("{base}/integrations/{}/mappings", integrator.slug))
        .headers(headers)
        .json(&serde_json::json!({ "from_schema_id": v1, "to_schema_id": v1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore]
async fn a_integrator_publishing_a_mapping_under_another_slug_is_forbidden() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator_a = register_integrator(&http, &base).await;
    let integrator_b = register_integrator(&http, &base).await;
    let a_v1 = publish_schema(&http, &base, &integrator_a, PROTO_V1).await;
    let a_v2 = publish_schema(&http, &base, &integrator_a, PROTO_V2).await;

    let headers = integrator_auth_headers(&http, &base, &integrator_a).await;
    let response = http
        .post(format!(
            "{base}/integrations/{}/mappings",
            integrator_b.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({ "from_schema_id": a_v1, "to_schema_id": a_v2 }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn listing_a_integrators_mappings_is_public_and_unauthenticated() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base).await;
    let v1 = publish_schema(&http, &base, &integrator, PROTO_V1).await;
    let v2 = publish_schema(&http, &base, &integrator, PROTO_V2).await;

    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    http.post(format!("{base}/integrations/{}/mappings", integrator.slug))
        .headers(headers)
        .json(&serde_json::json!({ "from_schema_id": v1, "to_schema_id": v2 }))
        .send()
        .await
        .unwrap();

    let listed: Vec<serde_json::Value> = http
        .get(format!("{base}/integrations/{}/mappings", integrator.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["from_schema_id"], v1);
}

#[tokio::test]
#[ignore]
async fn a_integrator_with_no_published_mappings_lists_empty() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base).await;

    let listed: Vec<serde_json::Value> = http
        .get(format!("{base}/integrations/{}/mappings", integrator.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(listed.is_empty());
}
