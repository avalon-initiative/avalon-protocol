//! Exercises the per-(issuer, subject) write-rate quota (issue #365,
//! implementing a piece of #306's decided abuse floor) against a real,
//! running `avalon-server` and Postgres. Gated `--ignored` since it needs
//! live infra — see `make test-live` / `make start`.
//!
//! The production default (`crates/server/src/achievements.rs`'s
//! `DEFAULT_WRITE_QUOTA`) is generous enough that hitting it for real would
//! mean issuing hundreds of attestations in this test — instead, start the
//! server with a small `AVALON_ACHIEVEMENT_WRITE_QUOTA` to exercise the
//! rejection path quickly: `AVALON_ACHIEVEMENT_WRITE_QUOTA=3 make start`.
//! Without it, both tests below skip rather than grinding out hundreds of
//! real HTTP calls against the production default.

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

fn configured_quota() -> Option<usize> {
    std::env::var("AVALON_ACHIEVEMENT_WRITE_QUOTA")
        .ok()
        .and_then(|s| s.parse().ok())
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
        .bind(format!("write-quota-test-{identity_id}"))
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

/// #697/#698: `POST /integrations/{slug}/connect` is signature-required
/// -- seeds a real signing key for `identity_id` so a connect call can
/// produce a genuine fresh signature over HTTP.
async fn seed_signing_key(pool: &PgPool, identity_id: Uuid) -> (Uuid, SigningKey) {
    let signing_key = SigningKey::generate(&mut rand::rng());
    let public_key = signing_key.verifying_key().to_bytes();
    let row = sqlx::query(
        "INSERT INTO identity_signing_keys (identity_id, public_key) VALUES ($1, $2) RETURNING id",
    )
    .bind(identity_id)
    .bind(public_key.as_slice())
    .fetch_one(pool)
    .await
    .expect("failed to seed signing key");
    (sqlx::Row::try_get(&row, "id").unwrap(), signing_key)
}

/// Mirrors `crate::signature_gate::canonical_message` byte-for-byte.
fn sign_connect(signing_key: &SigningKey, slug: &str, capabilities: &[&str]) -> String {
    let message = format!(
        "avalon:integration.connect:v1:{slug}:{}",
        capabilities.join(",")
    );
    BASE64.encode(signing_key.sign(message.as_bytes()).to_bytes())
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
    let slug = format!("test-quota-{}", &suffix[..10]);
    let body = serde_json::json!({
        "slug": slug,
        "name": format!("Write Quota Test {}", &suffix[..8]),
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

async fn connect(
    http: &reqwest::Client,
    base: &str,
    pool: &PgPool,
    integrator: &RegisteredIntegrator,
    identity_id: Uuid,
    identity_token: &str,
) {
    let (signing_key_id, signing_key) = seed_signing_key(pool, identity_id).await;
    let capabilities = ["achievements.issue"];
    let response = http
        .post(format!("{base}/integrations/{}/connect", integrator.slug))
        .bearer_auth(identity_token)
        .json(&serde_json::json!({
            "capabilities": capabilities,
            "signing_key_id": signing_key_id,
            "signature": sign_connect(&signing_key, &integrator.slug, &capabilities),
        }))
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

/// `achievement` here must be the definition's full ref
/// (`game:<slug>:achievement:<key>`, matching the server's own
/// `achievements::definition_ref` format), not the bare key.
fn single_signing_bytes(issuer_ref: &str, subject: Uuid, achievement: &str) -> Vec<u8> {
    format!("avalon:achievement.issued:v1:{issuer_ref}:{subject}:{achievement}").into_bytes()
}

/// Bulk-issues `key` `count` times (the same definition, repeated — the
/// endpoint doesn't dedupe claim keys within a call) in one signed
/// envelope, returning the raw response.
async fn bulk_issue_repeated(
    http: &reqwest::Client,
    base: &str,
    integrator: &RegisteredIntegrator,
    subject: Uuid,
    key: &str,
    count: usize,
) -> reqwest::Response {
    let achievements: Vec<String> = (0..count)
        .map(|_| format!("game:{}:achievement:{key}", integrator.slug))
        .collect();
    let issuer_ref = format!("game:{}", integrator.slug);
    let signing_bytes = bulk_signing_bytes(&issuer_ref, subject, &achievements);
    let signature = integrator.signing_key.sign(&signing_bytes);

    let (challenge_id, nonce) = integrator_challenge(http, base, integrator).await;
    let challenge_signature = integrator.signing_key.sign(&nonce);

    http.post(format!(
        "{base}/integrations/{}/achievements/bulk-issue",
        integrator.slug
    ))
    .header("x-avalon-integrator-key-id", integrator.key_id.to_string())
    .header("x-avalon-integrator-challenge-id", challenge_id)
    .header(
        "x-avalon-integrator-signature",
        BASE64.encode(challenge_signature.to_bytes()),
    )
    .header("x-avalon-identity-id", subject.to_string())
    .json(&serde_json::json!({
        "key_id": integrator.key_id,
        "signature": BASE64.encode(signature.to_bytes()),
        "claims": (0..count).map(|_| serde_json::json!({ "key": key })).collect::<Vec<_>>(),
    }))
    .send()
    .await
    .unwrap()
}

/// Single-issues `key` once, returning the raw response.
async fn issue_once(
    http: &reqwest::Client,
    base: &str,
    integrator: &RegisteredIntegrator,
    subject: Uuid,
    key: &str,
) -> reqwest::Response {
    let issuer_ref = format!("game:{}", integrator.slug);
    let achievement_ref = format!("game:{}:achievement:{key}", integrator.slug);
    let signing_bytes = single_signing_bytes(&issuer_ref, subject, &achievement_ref);
    let signature = integrator.signing_key.sign(&signing_bytes);

    let (challenge_id, nonce) = integrator_challenge(http, base, integrator).await;
    let challenge_signature = integrator.signing_key.sign(&nonce);

    http.post(format!(
        "{base}/integrations/{}/achievements/{key}/issue",
        integrator.slug
    ))
    .header("x-avalon-integrator-key-id", integrator.key_id.to_string())
    .header("x-avalon-integrator-challenge-id", challenge_id)
    .header(
        "x-avalon-integrator-signature",
        BASE64.encode(challenge_signature.to_bytes()),
    )
    .header("x-avalon-identity-id", subject.to_string())
    .json(&serde_json::json!({
        "key_id": integrator.key_id,
        "signature": BASE64.encode(signature.to_bytes()),
    }))
    .send()
    .await
    .unwrap()
}

#[tokio::test]
#[ignore]
async fn writes_past_the_quota_are_rejected_but_a_different_subject_is_unaffected() {
    let Some(quota) = configured_quota() else {
        eprintln!(
            "skipping: set AVALON_ACHIEVEMENT_WRITE_QUOTA (e.g. 3) before `make start` to run this test"
        );
        return;
    };

    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;

    let integrator = register_integrator(&http, &base).await;
    define_achievement(&http, &base, &integrator, "grind").await;

    let (subject_a, token_a) = seed_identity_session(&pool).await;
    connect(&http, &base, &pool, &integrator, subject_a, &token_a).await;

    // Exactly at the quota: allowed, in one shared envelope.
    let filled = bulk_issue_repeated(&http, &base, &integrator, subject_a, "grind", quota).await;
    assert!(
        filled.status().is_success(),
        "issuing exactly the quota ({quota}) should succeed: {:?}",
        filled.status()
    );
    let body: serde_json::Value = filled.json().await.unwrap();
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), quota);
    assert!(results.iter().all(|r| r["status"] == "issued"));

    // One more write about the same subject crosses the quota.
    let over = issue_once(&http, &base, &integrator, subject_a, "grind").await;
    assert_eq!(
        over.status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "the write past the quota must be rejected, not silently dropped or allowed: {:?}",
        over.status()
    );
    let over_body: serde_json::Value = over.json().await.unwrap();
    assert_eq!(over_body["code"], "ATTESTATION_WRITE_QUOTA_EXCEEDED");

    // A different subject from the same issuer is completely unaffected —
    // the quota is scoped to (issuer, subject), not the issuer alone.
    let (subject_b, token_b) = seed_identity_session(&pool).await;
    connect(&http, &base, &pool, &integrator, subject_b, &token_b).await;
    let other_subject = issue_once(&http, &base, &integrator, subject_b, "grind").await;
    assert!(
        other_subject.status().is_success(),
        "a different subject must not be affected by subject_a's quota: {:?}",
        other_subject.status()
    );
}

#[tokio::test]
#[ignore]
async fn a_bulk_call_that_would_cross_the_quota_is_rejected_as_a_whole() {
    let Some(quota) = configured_quota() else {
        eprintln!(
            "skipping: set AVALON_ACHIEVEMENT_WRITE_QUOTA (e.g. 3) before `make start` to run this test"
        );
        return;
    };

    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;

    let integrator = register_integrator(&http, &base).await;
    define_achievement(&http, &base, &integrator, "grind").await;

    let (subject, token) = seed_identity_session(&pool).await;
    connect(&http, &base, &pool, &integrator, subject, &token).await;

    // A single call asking for one more claim than the quota allows must
    // be rejected outright — never partially applied claim-by-claim.
    let over = bulk_issue_repeated(&http, &base, &integrator, subject, "grind", quota + 1).await;
    assert_eq!(
        over.status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "{:?}",
        over.status()
    );

    // Nothing from the rejected call should have landed.
    let count_row = sqlx::query_as::<_, (i64,)>(
        "SELECT COUNT(*) FROM achievement_attestations WHERE subject = $1",
    )
    .bind(subject)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        count_row.0, 0,
        "a whole-call rejection must not leave any partial writes behind"
    );
}
