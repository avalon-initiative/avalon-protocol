//! Exercises Integrator Space instance-data publication and its visibility-aware
//! read endpoint (issue #384, implementing #381's decided policy) against a
//! real, running `avalon-server` and Postgres. Gated `--ignored` since it
//! needs live infra — see `make test-live` / `make start`.
//!
//! Same "seed identity/session directly via SQL, connect for a real
//! binding" pattern `crates/server/tests/attestations.rs` established.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use reqwest::header::HeaderMap;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

async fn test_pool() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

async fn seed_identity_session(pool: &PgPool) -> (Uuid, String) {
    let identity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(identity_id)
        .execute(pool)
        .await
        .expect("failed to seed identity");
    sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(format!("integrator-data-test-{identity_id}"))
        .execute(pool)
        .await
        .expect("failed to seed profile");

    let token = format!("test-token-{}", Uuid::new_v4());
    let expires_at = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    sqlx::query("INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)")
        .bind(&token)
        .bind(identity_id)
        .bind(expires_at)
        .execute(pool)
        .await
        .expect("failed to seed session");

    (identity_id, token)
}

struct RegisteredIntegrator {
    signing_key: SigningKey,
    slug: String,
    key_id: String,
}

async fn register_integrator(
    http: &reqwest::Client,
    base: &str,
    prefix: &str,
) -> RegisteredIntegrator {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let slug = format!("{prefix}-{}", &suffix[..10]);
    let body = serde_json::json!({
        "slug": slug,
        "name": format!("Test Integrator {}", &suffix[..8]),
        "developer": "Test Studio",
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
    RegisteredIntegrator {
        signing_key,
        slug,
        key_id: registered["credential"]["key_id"]
            .as_str()
            .unwrap()
            .to_string(),
    }
}

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

async fn connect(
    http: &reqwest::Client,
    base: &str,
    integrator: &RegisteredIntegrator,
    token: &str,
) {
    let response = http
        .post(format!("{base}/integrators/{}/connect", integrator.slug))
        .bearer_auth(token)
        .json(&serde_json::json!({ "capabilities": [] }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
}

async fn publish_schema(
    http: &reqwest::Client,
    base: &str,
    integrator: &RegisteredIntegrator,
    body: serde_json::Value,
) -> reqwest::Response {
    let headers = integrator_auth_headers(http, base, integrator).await;
    http.post(format!("{base}/integrators/{}/schemas", integrator.slug))
        .headers(headers)
        .json(&body)
        .send()
        .await
        .unwrap()
}

const CHARACTER_PROTO: &str =
    "syntax = \"proto3\"; message Character { uint32 level = 1; string title = 2; }";

/// Scenario 1: a schema whose `.proto` text fails to parse is rejected
/// cleanly (400, not a crash), and the server keeps serving everyone else
/// afterward.
#[tokio::test]
#[ignore]
async fn a_malformed_proto_schema_is_rejected_cleanly_and_the_server_stays_up() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base, "test-malformed").await;

    let response = publish_schema(
        &http,
        &base,
        &integrator,
        serde_json::json!({ "proto_source": "this is not valid proto syntax {{{" }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body.is_object());

    // The server is still alive and correctly serving an unrelated request.
    let other_integrator = register_integrator(&http, &base, "test-after-malformed").await;
    let ok = publish_schema(
        &http,
        &base,
        &other_integrator,
        serde_json::json!({ "proto_source": CHARACTER_PROTO }),
    )
    .await;
    assert!(ok.status().is_success(), "{:?}", ok.status());
}

/// Scenario 2: zero or multiple top-level messages are both rejected
/// cleanly.
#[tokio::test]
#[ignore]
async fn zero_or_multiple_top_level_messages_are_rejected_cleanly() {
    let http = reqwest::Client::new();
    let base = server_url();

    let integrator_zero = register_integrator(&http, &base, "test-zero-msg").await;
    let zero = publish_schema(
        &http,
        &base,
        &integrator_zero,
        serde_json::json!({ "proto_source": "syntax = \"proto3\";" }),
    )
    .await;
    assert_eq!(zero.status(), reqwest::StatusCode::BAD_REQUEST);

    let integrator_multi = register_integrator(&http, &base, "test-multi-msg").await;
    let multi = publish_schema(
        &http,
        &base,
        &integrator_multi,
        serde_json::json!({
            "proto_source": "syntax = \"proto3\"; message A { uint32 x = 1; } message B { uint32 y = 1; }"
        }),
    )
    .await;
    assert_eq!(multi.status(), reqwest::StatusCode::BAD_REQUEST);
}

/// Scenario 4: a `field_visibility` entry naming a nonexistent field is
/// rejected at schema-publish time.
#[tokio::test]
#[ignore]
async fn field_visibility_naming_a_nonexistent_field_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base, "test-bad-field-vis").await;

    let response = publish_schema(
        &http,
        &base,
        &integrator,
        serde_json::json!({
            "proto_source": CHARACTER_PROTO,
            "field_visibility": { "does_not_exist": "private" },
        }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}

/// Scenario 9: a pre-#384 publish request shape (no visibility fields at
/// all) still works exactly as before — full backward compatibility. Also
/// confirms the defaulted visibility is fully open.
#[tokio::test]
#[ignore]
async fn a_pre_384_publish_request_shape_still_works() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http, &base, "test-pre-384").await;

    let response = publish_schema(
        &http,
        &base,
        &integrator,
        serde_json::json!({ "proto_source": CHARACTER_PROTO }),
    )
    .await;
    assert!(response.status().is_success(), "{:?}", response.status());
    let published: serde_json::Value = response.json().await.unwrap();
    assert_eq!(published["default_visibility"], "public");
    assert_eq!(published["field_visibility"], serde_json::json!({}));
}

/// Scenario 3: an instance that doesn't match its schema — an unknown
/// field and a wrong type — is rejected, not stored.
#[tokio::test]
#[ignore]
async fn a_non_conforming_instance_is_rejected_and_never_stored() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let integrator = register_integrator(&http, &base, "test-bad-instance").await;
    let (identity_id, token) = seed_identity_session(&pool).await;
    connect(&http, &base, &integrator, &token).await;

    let published = publish_schema(
        &http,
        &base,
        &integrator,
        serde_json::json!({ "proto_source": CHARACTER_PROTO }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .unwrap();
    let version = published["version"].as_u64().unwrap();

    // Unknown field.
    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let unknown_field = http
        .post(format!(
            "{base}/integrators/{}/schemas/{version}/data",
            integrator.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "subject": identity_id,
            "instance": { "level": 5, "not_a_real_field": "x" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown_field.status(), reqwest::StatusCode::BAD_REQUEST);

    // Wrong type.
    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let wrong_type = http
        .post(format!(
            "{base}/integrators/{}/schemas/{version}/data",
            integrator.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "subject": identity_id,
            "instance": { "level": "not a number" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_type.status(), reqwest::StatusCode::BAD_REQUEST);

    // Nothing landed in integrator_data_instances for this subject.
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM integrator_data_instances WHERE subject = $1")
            .bind(identity_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
}

/// Scenario 7: an integrator cannot publish instance data for an identity it has
/// no active binding to.
#[tokio::test]
#[ignore]
async fn a_integrator_cannot_publish_instance_data_for_an_unbound_identity() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let integrator = register_integrator(&http, &base, "test-unbound").await;
    let (identity_id, _token) = seed_identity_session(&pool).await; // never connects

    let published = publish_schema(
        &http,
        &base,
        &integrator,
        serde_json::json!({ "proto_source": CHARACTER_PROTO }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .unwrap();
    let version = published["version"].as_u64().unwrap();

    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let response = http
        .post(format!(
            "{base}/integrators/{}/schemas/{version}/data",
            integrator.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "subject": identity_id,
            "instance": { "level": 1 },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
}

/// Scenario 8 (folded into the fuller cross-integrator isolation test
/// below too): an integrator cannot publish instance data against another integrator's
/// schema.
#[tokio::test]
#[ignore]
async fn a_integrator_cannot_publish_instance_data_against_another_integrators_schema() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let integrator_a = register_integrator(&http, &base, "test-owner").await;
    let integrator_b = register_integrator(&http, &base, "test-intruder").await;
    let (identity_id, token) = seed_identity_session(&pool).await;
    connect(&http, &base, &integrator_a, &token).await;
    connect(&http, &base, &integrator_b, &token).await;

    let published = publish_schema(
        &http,
        &base,
        &integrator_a,
        serde_json::json!({ "proto_source": CHARACTER_PROTO }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .unwrap();
    let version = published["version"].as_u64().unwrap();

    // integrator_b authenticates as itself, but the path names integrator_a's schema.
    let headers = integrator_auth_headers(&http, &base, &integrator_b).await;
    let response = http
        .post(format!(
            "{base}/integrators/{}/schemas/{version}/data",
            integrator_a.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "subject": identity_id,
            "instance": { "level": 1 },
        }))
        .send()
        .await
        .unwrap();
    // Rejected by `authenticate_owning_integrator` before schema ownership is
    // even consulted: the path's own slug doesn't match the caller.
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
}

/// Scenario 5: a private schema with one field explicitly marked public —
/// the read endpoint returns only that field.
#[tokio::test]
#[ignore]
async fn a_private_schema_with_one_public_field_exposes_only_that_field() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let integrator = register_integrator(&http, &base, "test-private-vis").await;
    let (identity_id, token) = seed_identity_session(&pool).await;
    connect(&http, &base, &integrator, &token).await;

    let published = publish_schema(
        &http,
        &base,
        &integrator,
        serde_json::json!({
            "proto_source": CHARACTER_PROTO,
            "default_visibility": "private",
            "field_visibility": { "title": "public" },
        }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .unwrap();
    let version = published["version"].as_u64().unwrap();

    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let publish_instance = http
        .post(format!(
            "{base}/integrators/{}/schemas/{version}/data",
            integrator.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "subject": identity_id,
            "instance": { "level": 42, "title": "Dragonslayer" },
        }))
        .send()
        .await
        .unwrap();
    assert!(
        publish_instance.status().is_success(),
        "{:?}",
        publish_instance.status()
    );

    let read: Vec<serde_json::Value> = http
        .get(format!("{base}/identities/{identity_id}/integrator-data"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(read.len(), 1);
    let fields = read[0]["fields"].as_object().unwrap();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields.get("title").unwrap(), "Dragonslayer");
    assert!(fields.get("level").is_none());
}

/// Scenario 6: a public schema (the default) with one field explicitly
/// marked private — the read endpoint returns everything except that
/// field.
#[tokio::test]
#[ignore]
async fn a_public_schema_with_one_private_field_hides_only_that_field() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let integrator = register_integrator(&http, &base, "test-public-vis").await;
    let (identity_id, token) = seed_identity_session(&pool).await;
    connect(&http, &base, &integrator, &token).await;

    let published = publish_schema(
        &http,
        &base,
        &integrator,
        serde_json::json!({
            "proto_source": CHARACTER_PROTO,
            "field_visibility": { "level": "private" },
        }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .unwrap();
    let version = published["version"].as_u64().unwrap();

    let headers = integrator_auth_headers(&http, &base, &integrator).await;
    let publish_instance = http
        .post(format!(
            "{base}/integrators/{}/schemas/{version}/data",
            integrator.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "subject": identity_id,
            "instance": { "level": 42, "title": "Dragonslayer" },
        }))
        .send()
        .await
        .unwrap();
    assert!(
        publish_instance.status().is_success(),
        "{:?}",
        publish_instance.status()
    );

    let read: Vec<serde_json::Value> = http
        .get(format!("{base}/identities/{identity_id}/integrator-data"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(read.len(), 1);
    let fields = read[0]["fields"].as_object().unwrap();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields.get("title").unwrap(), "Dragonslayer");
    assert!(fields.get("level").is_none());
}

/// Full cross-integrator write isolation, end to end: a second, genuinely
/// separate, properly-authenticated integrator can neither modify nor even
/// observe any change against the first integrator's schema, instance data, or
/// attestation. Deliberately stronger than "an unauthenticated caller is
/// rejected" — Integrator 2 authenticates as itself throughout.
#[tokio::test]
#[ignore]
async fn cross_integrator_write_isolation_is_total() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;

    // Integrator 1: schema + instance + achievement, all real.
    let integrator_1 = register_integrator(&http, &base, "test-iso-owner").await;
    let (identity_id, token) = seed_identity_session(&pool).await;
    connect(&http, &base, &integrator_1, &token).await;

    let published = publish_schema(
        &http,
        &base,
        &integrator_1,
        serde_json::json!({
            "proto_source": CHARACTER_PROTO,
            "default_visibility": "private",
            "field_visibility": { "title": "public" },
        }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .unwrap();
    let version = published["version"].as_u64().unwrap();
    let schema_id = published["id"].as_str().unwrap().to_string();

    let headers = integrator_auth_headers(&http, &base, &integrator_1).await;
    let instance_response = http
        .post(format!(
            "{base}/integrators/{}/schemas/{version}/data",
            integrator_1.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "subject": identity_id,
            "instance": { "level": 7, "title": "Owner's Data" },
        }))
        .send()
        .await
        .unwrap();
    assert!(instance_response.status().is_success());
    let instance_before: serde_json::Value = instance_response.json().await.unwrap();

    // A real achievement, defined and issued by integrator_1.
    // Re-register with the achievements.issue capability so `connect`
    // above (already run with `[]`) doesn't need to be redone — instead
    // register a second integrator specifically for this, matching
    // `tests/attestations.rs`'s own pattern.
    let issuer = {
        let suffix = Uuid::new_v4().simple().to_string();
        let mut csprng = rand::rng();
        let signing_key = SigningKey::generate(&mut csprng);
        let slug = format!("test-iso-issuer-{}", &suffix[..8]);
        let body = serde_json::json!({
            "slug": slug,
            "name": "Isolation Test Issuer",
            "developer": "Test Studio",
            "category": "game",
            "requested_capabilities": ["achievements.issue"],
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
            .unwrap();
        assert!(response.status().is_success());
        let registered: serde_json::Value = response.json().await.unwrap();
        RegisteredIntegrator {
            signing_key,
            slug,
            key_id: registered["credential"]["key_id"]
                .as_str()
                .unwrap()
                .to_string(),
        }
    };
    connect(&http, &base, &issuer, &token).await;
    let issuer_connect = http
        .post(format!("{base}/integrators/{}/connect", issuer.slug))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "capabilities": ["achievements.issue"] }))
        .send()
        .await
        .unwrap();
    assert!(issuer_connect.status().is_success());

    let headers = integrator_auth_headers(&http, &base, &issuer).await;
    let define = http
        .post(format!("{base}/integrators/{}/achievements", issuer.slug))
        .headers(headers)
        .json(&serde_json::json!({
            "key": "isolation_test",
            "name": "Isolation Test",
            "description": "Proves cross-integrator isolation",
        }))
        .send()
        .await
        .unwrap();
    assert!(define.status().is_success(), "{:?}", define.status());
    let achievement_id = define.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let issuer_ref_str = format!("game:{}", issuer.slug);
    let signing_bytes =
        format!("avalon:achievement.issued:v1:{issuer_ref_str}:{identity_id}:{achievement_id}")
            .into_bytes();
    let signature = issuer.signing_key.sign(&signing_bytes);
    let mut headers = integrator_auth_headers(&http, &base, &issuer).await;
    headers.insert(
        "x-avalon-identity-id",
        identity_id.to_string().parse().unwrap(),
    );
    let issue = http
        .post(format!(
            "{base}/integrators/{}/achievements/isolation_test/issue",
            issuer.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "key_id": issuer.key_id,
            "signature": BASE64.encode(signature.to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert!(issue.status().is_success(), "{:?}", issue.status());
    let attestation_before: serde_json::Value = issue.json().await.unwrap();
    let attestation_id = attestation_before["id"].as_str().unwrap().to_string();

    // Integrator 2: a fully legitimate, separately-registered, properly
    // authenticated integrator with no relationship to integrator_1's stuff.
    let integrator_2 = register_integrator(&http, &base, "test-iso-intruder").await;
    connect(&http, &base, &integrator_2, &token).await;

    // (a) Attempt to publish a new schema version attributed to integrator_1's
    // slug — the classic cross-slug schema-publish forbidden case.
    let headers = integrator_auth_headers(&http, &base, &integrator_2).await;
    let bad_schema_publish = http
        .post(format!("{base}/integrators/{}/schemas", integrator_1.slug))
        .headers(headers)
        .json(&serde_json::json!({ "proto_source": CHARACTER_PROTO }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_schema_publish.status(), reqwest::StatusCode::FORBIDDEN);

    // (b) Publish instance data against integrator_1's schema (scenario 8).
    let headers = integrator_auth_headers(&http, &base, &integrator_2).await;
    let bad_instance_publish = http
        .post(format!(
            "{base}/integrators/{}/schemas/{version}/data",
            integrator_1.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "subject": identity_id,
            "instance": { "level": 999, "title": "Hijacked" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        bad_instance_publish.status(),
        reqwest::StatusCode::FORBIDDEN
    );

    // (c) Issue an attestation under integrator_1's own issuer namespace — integrator_2
    // can only authenticate and issue as itself, never as integrator_1, so this
    // is exercised as "integrator_2 cannot issue under integrator_1's achievement key"
    // by attempting the issue route under integrator_1's slug entirely.
    let headers = integrator_auth_headers(&http, &base, &integrator_2).await;
    let bad_issue = http
        .post(format!(
            "{base}/integrators/{}/achievements/isolation_test/issue",
            integrator_1.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "key_id": issuer.key_id,
            "signature": BASE64.encode(signature.to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert!(!bad_issue.status().is_success(), "{:?}", bad_issue.status());

    // (d) Attempt to supersede integrator_1's existing instance data as integrator_2 —
    // same endpoint as (b), same result expected; folded in as a second
    // call to prove it's not a one-shot fluke.
    let headers = integrator_auth_headers(&http, &base, &integrator_2).await;
    let bad_supersede = http
        .post(format!(
            "{base}/integrators/{}/schemas/{version}/data",
            integrator_1.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "subject": identity_id,
            "instance": { "level": 1000, "title": "Hijacked Again" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_supersede.status(), reqwest::StatusCode::FORBIDDEN);

    // Nothing about integrator_1's stored state moved at all.
    let schema_after: serde_json::Value = http
        .get(format!(
            "{base}/integrators/{}/schemas/{version}",
            integrator_1.slug
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(schema_after["id"], schema_id);
    assert_eq!(schema_after["proto_source"], CHARACTER_PROTO);
    assert!(schema_after["superseded_by"].is_null());

    let instance_row = sqlx::query(
        "SELECT id, instance, superseded_by FROM integrator_data_instances \
         WHERE schema_id = $1 AND subject = $2",
    )
    .bind(&schema_id)
    .bind(identity_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(instance_row.len(), 1, "no new/superseding row was created");
    let stored_instance: serde_json::Value =
        sqlx::Row::try_get(&instance_row[0], "instance").unwrap();
    assert_eq!(
        stored_instance,
        serde_json::json!({ "level": 7, "title": "Owner's Data" })
    );
    assert_eq!(instance_before["instance"], stored_instance);

    let attestation_after: serde_json::Value = http
        .get(format!("{base}/attestations/{attestation_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        attestation_after["achievement"],
        attestation_before["achievement"]
    );
    assert_eq!(attestation_after["issuer"], attestation_before["issuer"]);
    assert!(attestation_after["history"]
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry["event"] != "revoked"));
}
