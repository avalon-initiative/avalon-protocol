//! Exercises Integrator Space schema publication (issue #255) against a real,
//! running `avalon-server` and Postgres. Gated `--ignored` since it needs
//! live infra — see `make test-live` / `make start`. Skipped in this
//! sandbox per `.claude/CLAUDE.md` (no reachable Postgres here); written but
//! not run against a live database.
//!
//! Mirrors `crates/server/tests/achievements.rs`'s own pattern for the
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
    let slug = format!("test-schema-{}", &suffix[..12]);
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

/// Mints a fresh challenge for `integrator` and signs it, returning the three
/// `x-avalon-integrator-*` headers `integrators::authenticate_integrator` verifies — a new
/// set every call, since a challenge is single-use.
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

// Valid proto3 syntax as of #384: `proto_source` is now actually parsed
// (`crate::proto_schema`), not stored opaquely, so these fixtures must be
// real, parseable `.proto` text — unlike before #384, where any non-empty
// string was accepted.
const PROTO_V1: &str =
    "syntax = \"proto3\"; message Character { uint32 level = 1; uint64 xp = 2; }";
const PROTO_V2: &str =
    "syntax = \"proto3\"; message Character { uint32 level = 1; uint64 xp = 2; string title = 3; }";

#[tokio::test]
#[ignore]
async fn publishing_a_schema_version_round_trips_the_proto_source_verbatim() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base).await;

    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let published: serde_json::Value = http
        .post(format!("{base}/integrations/{}/schemas", integrator.slug))
        .headers(headers)
        .json(&serde_json::json!({ "proto_source": PROTO_V1 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(published["version"], 1);
    assert_eq!(
        published["id"].as_str().unwrap(),
        format!("game:{}:schema:1", integrator.slug)
    );

    let fetched: serde_json::Value = http
        .get(format!("{base}/integrations/{}/schemas/1", integrator.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fetched["proto_source"].as_str().unwrap(), PROTO_V1);
    assert_eq!(fetched["id"], published["id"]);
    assert!(fetched["superseded_by"].is_null());
}

#[tokio::test]
#[ignore]
async fn publishing_a_second_version_leaves_the_first_untouched_and_links_lineage() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base).await;

    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    http.post(format!("{base}/integrations/{}/schemas", integrator.slug))
        .headers(headers)
        .json(&serde_json::json!({ "proto_source": PROTO_V1 }))
        .send()
        .await
        .unwrap();

    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let v2: serde_json::Value = http
        .post(format!("{base}/integrations/{}/schemas", integrator.slug))
        .headers(headers)
        .json(&serde_json::json!({ "proto_source": PROTO_V2 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v2["version"], 2);

    // The first version's proto text is unchanged — immutability.
    let v1_after: serde_json::Value = http
        .get(format!("{base}/integrations/{}/schemas/1", integrator.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v1_after["proto_source"].as_str().unwrap(), PROTO_V1);
    // ...but now points at the version that superseded it.
    assert_eq!(v1_after["superseded_by"], v2["id"]);
}

#[tokio::test]
#[ignore]
async fn a_integrator_publishing_under_another_slug_is_forbidden() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator_a = register_integrator(&http, &base).await;
    let integrator_b = register_integrator(&http, &base).await;

    // integrator_a's real credential, but the path names integrator_b's slug.
    let headers = integrator_auth_headers(&http, &base, &integrator_a).await;
    let response = http
        .post(format!("{base}/integrations/{}/schemas", integrator_b.slug))
        .headers(headers)
        .json(&serde_json::json!({ "proto_source": PROTO_V1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn listing_a_integrators_schema_versions_is_public_and_unauthenticated() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base).await;

    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    http.post(format!("{base}/integrations/{}/schemas", integrator.slug))
        .headers(headers)
        .json(&serde_json::json!({ "proto_source": PROTO_V1 }))
        .send()
        .await
        .unwrap();

    let listed: Vec<serde_json::Value> = http
        .get(format!("{base}/integrations/{}/schemas", integrator.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["version"], 1);
}

#[tokio::test]
#[ignore]
async fn a_integrator_with_no_publications_lists_empty() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base).await;

    let listed: Vec<serde_json::Value> = http
        .get(format!("{base}/integrations/{}/schemas", integrator.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(listed.is_empty());
}

#[tokio::test]
#[ignore]
async fn two_concurrent_publishes_for_the_same_integrator_produce_distinct_sequential_versions() {
    // Regression test for the race `publish_schema_version` closes with
    // `SELECT ... FOR UPDATE` on the integrator row: two publish requests that
    // land in the same instant used to both read the same `MAX(version)`
    // and race the `UNIQUE (integrator_id, version)` constraint, surfacing an
    // unhandled 500 for whichever lost. With the lock in place, the second
    // transaction blocks until the first commits and correctly computes
    // the next version instead — so both requests must succeed, and must
    // land on versions 1 and 2, in either order.
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base).await;

    // Mint both single-use challenges up front so neither request waits on
    // the other's challenge round-trip once fired.
    let headers_a = integrator_auth_headers(&http, &base, &integrator).await;
    let headers_b = integrator_auth_headers(&http, &base, &integrator).await;

    let publish = |headers: HeaderMap, proto: &'static str| {
        let http = http.clone();
        let url = format!("{base}/integrations/{}/schemas", integrator.slug);
        async move {
            http.post(url)
                .headers(headers)
                .json(&serde_json::json!({ "proto_source": proto }))
                .send()
                .await
                .unwrap()
        }
    };

    let (response_a, response_b) =
        tokio::join!(publish(headers_a, PROTO_V1), publish(headers_b, PROTO_V2));

    assert_eq!(response_a.status(), reqwest::StatusCode::OK);
    assert_eq!(response_b.status(), reqwest::StatusCode::OK);

    let body_a: serde_json::Value = response_a.json().await.unwrap();
    let body_b: serde_json::Value = response_b.json().await.unwrap();
    let mut versions = [
        body_a["version"].as_u64().unwrap(),
        body_b["version"].as_u64().unwrap(),
    ];
    versions.sort_unstable();
    assert_eq!(versions, [1, 2]);

    let listed: Vec<serde_json::Value> = http
        .get(format!("{base}/integrations/{}/schemas", integrator.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.len(), 2);
}
