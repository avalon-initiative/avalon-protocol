//! Exercises bulk attestation issuance (issue #495, implementing #492's
//! decided shape) against a real, running `avalon-server` and Postgres.
//! Gated `--ignored` since it needs live infra — see `make test-live` /
//! `make start`.
//!
//! Identity/session setup mirrors `crates/server/tests/connections.rs`'s
//! own pattern: seed a bare identity + session directly via SQL rather
//! than a real WebAuthn ceremony (these endpoints only care that the
//! bearer token is valid and that a grant exists). Integrator/challenge
//! setup mirrors `crates/server/tests/achievements.rs`'s own pattern.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
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
        .bind(format!("bulk-issue-test-{identity_id}"))
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
    key_id: Uuid,
}

async fn register_integrator(http: &reqwest::Client, base: &str) -> RegisteredIntegrator {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let slug = format!("test-bulk-{}", &suffix[..12]);
    let body = serde_json::json!({
        "slug": slug,
        "name": format!("Bulk Issuance Test {}", &suffix[..8]),
        "owner_name": "Test Studio",
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
        .expect("register integrator failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());
    let registered: serde_json::Value = response.json().await.unwrap();
    let key_id: Uuid = registered["credential"]["key_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    RegisteredIntegrator {
        signing_key,
        slug,
        key_id,
    }
}

async fn integrator_challenge(
    http: &reqwest::Client,
    base: &str,
    integrator: &RegisteredIntegrator,
) -> (String, Vec<u8>) {
    let challenge: serde_json::Value = http
        .post(format!("{base}/integrations/{}/challenge", integrator.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap().to_string();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    (challenge_id, nonce)
}

async fn define_achievement(
    http: &reqwest::Client,
    base: &str,
    integrator: &RegisteredIntegrator,
    key: &str,
) {
    let (challenge_id, nonce) = integrator_challenge(http, base, integrator).await;
    let signature = integrator.signing_key.sign(&nonce);

    let response = http
        .post(format!(
            "{base}/integrations/{}/achievements",
            integrator.slug
        ))
        .header("x-avalon-integrator-key-id", integrator.key_id.to_string())
        .header("x-avalon-integrator-challenge-id", challenge_id)
        .header(
            "x-avalon-integrator-signature",
            BASE64.encode(signature.to_bytes()),
        )
        .json(&serde_json::json!({
            "key": key,
            "name": key,
            "description": key,
        }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
}

async fn retire_achievement(
    http: &reqwest::Client,
    base: &str,
    integrator: &RegisteredIntegrator,
    key: &str,
) {
    let (challenge_id, nonce) = integrator_challenge(http, base, integrator).await;
    let signature = integrator.signing_key.sign(&nonce);

    let response = http
        .patch(format!(
            "{base}/integrations/{}/achievements/{key}",
            integrator.slug
        ))
        .header("x-avalon-integrator-key-id", integrator.key_id.to_string())
        .header("x-avalon-integrator-challenge-id", challenge_id)
        .header(
            "x-avalon-integrator-signature",
            BASE64.encode(signature.to_bytes()),
        )
        .json(&serde_json::json!({ "retired": true }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
}

/// Grants `identity_token`'s identity an active binding + `achievements.issue`
/// to `integrator`, via the real connect endpoint — the user's own consent,
/// same gate every issuance (bulk or single) requires.
async fn connect(
    http: &reqwest::Client,
    base: &str,
    integrator: &RegisteredIntegrator,
    identity_token: &str,
) {
    let response = http
        .post(format!("{base}/integrations/{}/connect", integrator.slug))
        .bearer_auth(identity_token)
        .json(&serde_json::json!({ "capabilities": ["achievements.issue"] }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
}

fn bulk_signing_bytes(issuer_ref: &str, subject: Uuid, achievements: &[String]) -> Vec<u8> {
    let mut message =
        format!("avalon:achievement.issued.bulk:v1:{issuer_ref}:{subject}:").into_bytes();
    message.extend_from_slice(&(achievements.len() as u32).to_be_bytes());
    for achievement in achievements {
        message.extend_from_slice(&(achievement.len() as u32).to_be_bytes());
        message.extend_from_slice(achievement.as_bytes());
    }
    message
}

struct BulkCall {
    integrator: RegisteredIntegrator,
    subject: Uuid,
}

async fn setup(http: &reqwest::Client, base: &str, pool: &PgPool) -> BulkCall {
    let (subject, identity_token) = seed_identity_session(pool).await;
    let integrator = register_integrator(http, base).await;
    connect(http, base, &integrator, &identity_token).await;
    BulkCall {
        integrator,
        subject,
    }
}

/// Signs `keys` (achievement refs derived the same way the server does)
/// with `call.integrator`'s real key and posts the bulk-issue request,
/// returning the raw response for the caller to assert on.
async fn bulk_issue(
    http: &reqwest::Client,
    base: &str,
    call: &BulkCall,
    keys: &[&str],
) -> reqwest::Response {
    let achievements: Vec<String> = keys
        .iter()
        .map(|key| format!("game:{}:achievement:{key}", call.integrator.slug))
        .collect();
    let issuer_ref = format!("game:{}", call.integrator.slug);
    let signing_bytes = bulk_signing_bytes(&issuer_ref, call.subject, &achievements);
    let signature = call.integrator.signing_key.sign(&signing_bytes);

    let (challenge_id, nonce) = integrator_challenge(http, base, &call.integrator).await;
    let challenge_signature = call.integrator.signing_key.sign(&nonce);

    http.post(format!(
        "{base}/integrations/{}/achievements/bulk-issue",
        call.integrator.slug
    ))
    .header(
        "x-avalon-integrator-key-id",
        call.integrator.key_id.to_string(),
    )
    .header("x-avalon-integrator-challenge-id", challenge_id)
    .header(
        "x-avalon-integrator-signature",
        BASE64.encode(challenge_signature.to_bytes()),
    )
    .header("x-avalon-identity-id", call.subject.to_string())
    .json(&serde_json::json!({
        "key_id": call.integrator.key_id,
        "signature": BASE64.encode(signature.to_bytes()),
        "claims": keys.iter().map(|key| serde_json::json!({ "key": key })).collect::<Vec<_>>(),
    }))
    .send()
    .await
    .unwrap()
}

#[tokio::test]
#[ignore]
async fn bulk_issue_with_all_valid_claims_issues_every_one() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let call = setup(&http, &base, &pool).await;
    define_achievement(&http, &base, &call.integrator, "dragon_slayer").await;
    define_achievement(&http, &base, &call.integrator, "lost_city").await;

    let response = bulk_issue(&http, &base, &call, &["dragon_slayer", "lost_city"]).await;
    assert!(response.status().is_success(), "{:?}", response.status());
    let body: serde_json::Value = response.json().await.unwrap();
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["status"], "issued");
    assert_eq!(results[0]["key"], "dragon_slayer");
    assert!(results[0]["attestation"]["id"].is_string());
    assert_eq!(results[1]["status"], "issued");
    assert_eq!(results[1]["key"], "lost_city");
}

#[tokio::test]
#[ignore]
async fn bulk_issue_reports_per_claim_failures_without_failing_the_whole_batch() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let call = setup(&http, &base, &pool).await;
    define_achievement(&http, &base, &call.integrator, "dragon_slayer").await;
    define_achievement(&http, &base, &call.integrator, "retired_one").await;
    retire_achievement(&http, &base, &call.integrator, "retired_one").await;

    let response = bulk_issue(
        &http,
        &base,
        &call,
        &["dragon_slayer", "does_not_exist", "retired_one"],
    )
    .await;
    assert!(
        response.status().is_success(),
        "a partial failure must still be a 2xx — per-item, not whole-call: {:?}",
        response.status()
    );
    let body: serde_json::Value = response.json().await.unwrap();
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 3);

    assert_eq!(results[0]["status"], "issued");
    assert_eq!(results[0]["key"], "dragon_slayer");

    assert_eq!(results[1]["status"], "failed");
    assert_eq!(results[1]["key"], "does_not_exist");
    assert_eq!(results[1]["code"], "ACHIEVEMENT_DEFINITION_NOT_FOUND");

    assert_eq!(results[2]["status"], "failed");
    assert_eq!(results[2]["key"], "retired_one");
    assert_eq!(results[2]["code"], "ATTESTATION_DEFINITION_RETIRED");
}

#[tokio::test]
#[ignore]
async fn bulk_issue_rejects_a_signature_from_a_different_key() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let call = setup(&http, &base, &pool).await;
    define_achievement(&http, &base, &call.integrator, "dragon_slayer").await;

    let impostor_key = SigningKey::generate(&mut rand::rng());
    let achievements = vec![format!(
        "game:{}:achievement:dragon_slayer",
        call.integrator.slug
    )];
    let issuer_ref = format!("game:{}", call.integrator.slug);
    let signing_bytes = bulk_signing_bytes(&issuer_ref, call.subject, &achievements);
    let forged_signature = impostor_key.sign(&signing_bytes);

    let (challenge_id, nonce) = integrator_challenge(&http, &base, &call.integrator).await;
    let challenge_signature = call.integrator.signing_key.sign(&nonce);

    let response = http
        .post(format!(
            "{base}/integrations/{}/achievements/bulk-issue",
            call.integrator.slug
        ))
        .header(
            "x-avalon-integrator-key-id",
            call.integrator.key_id.to_string(),
        )
        .header("x-avalon-integrator-challenge-id", challenge_id)
        .header(
            "x-avalon-integrator-signature",
            BASE64.encode(challenge_signature.to_bytes()),
        )
        .header("x-avalon-identity-id", call.subject.to_string())
        .json(&serde_json::json!({
            "key_id": call.integrator.key_id,
            "signature": BASE64.encode(forged_signature.to_bytes()),
            "claims": [{ "key": "dragon_slayer" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore]
async fn bulk_issue_rejects_a_claim_list_tampered_after_signing() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let call = setup(&http, &base, &pool).await;
    define_achievement(&http, &base, &call.integrator, "dragon_slayer").await;
    define_achievement(&http, &base, &call.integrator, "lost_city").await;

    // Sign over just `dragon_slayer`, but submit both claims — the
    // signature must not cover a list it was never actually produced for.
    let achievements = vec![format!(
        "game:{}:achievement:dragon_slayer",
        call.integrator.slug
    )];
    let issuer_ref = format!("game:{}", call.integrator.slug);
    let signing_bytes = bulk_signing_bytes(&issuer_ref, call.subject, &achievements);
    let signature = call.integrator.signing_key.sign(&signing_bytes);

    let (challenge_id, nonce) = integrator_challenge(&http, &base, &call.integrator).await;
    let challenge_signature = call.integrator.signing_key.sign(&nonce);

    let response = http
        .post(format!(
            "{base}/integrations/{}/achievements/bulk-issue",
            call.integrator.slug
        ))
        .header(
            "x-avalon-integrator-key-id",
            call.integrator.key_id.to_string(),
        )
        .header("x-avalon-integrator-challenge-id", challenge_id)
        .header(
            "x-avalon-integrator-signature",
            BASE64.encode(challenge_signature.to_bytes()),
        )
        .header("x-avalon-identity-id", call.subject.to_string())
        .json(&serde_json::json!({
            "key_id": call.integrator.key_id,
            "signature": BASE64.encode(signature.to_bytes()),
            "claims": [{ "key": "dragon_slayer" }, { "key": "lost_city" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore]
async fn bulk_issue_rejects_an_empty_claims_list() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let call = setup(&http, &base, &pool).await;

    let response = bulk_issue(&http, &base, &call, &[]).await;
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore]
async fn single_claim_issuance_is_unaffected_by_the_bulk_endpoint() {
    // Regression coverage for #495's own invariant: bulk issuance must not
    // have altered the pre-existing single-issuance path at all.
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let call = setup(&http, &base, &pool).await;
    define_achievement(&http, &base, &call.integrator, "dragon_slayer").await;

    let (challenge_id, nonce) = integrator_challenge(&http, &base, &call.integrator).await;
    let challenge_signature = call.integrator.signing_key.sign(&nonce);

    let achievement = format!("game:{}:achievement:dragon_slayer", call.integrator.slug);
    let issuer_ref = format!("game:{}", call.integrator.slug);
    let signing_bytes = format!(
        "avalon:achievement.issued:v1:{issuer_ref}:{}:{achievement}",
        call.subject
    )
    .into_bytes();
    let signature = call.integrator.signing_key.sign(&signing_bytes);

    let response = http
        .post(format!(
            "{base}/integrations/{}/achievements/dragon_slayer/issue",
            call.integrator.slug
        ))
        .header(
            "x-avalon-integrator-key-id",
            call.integrator.key_id.to_string(),
        )
        .header("x-avalon-integrator-challenge-id", challenge_id)
        .header(
            "x-avalon-integrator-signature",
            BASE64.encode(challenge_signature.to_bytes()),
        )
        .header("x-avalon-identity-id", call.subject.to_string())
        .json(&serde_json::json!({
            "key_id": call.integrator.key_id,
            "signature": BASE64.encode(signature.to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
}
