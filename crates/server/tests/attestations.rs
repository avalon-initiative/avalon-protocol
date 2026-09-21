//! Exercises attestation issuance (issue #32) against a real, running
//! `avalon-server` and Postgres. Gated `--ignored` since it needs live
//! infra — see `make test-live` / `make start`.
//!
//! Same "seed identity/session directly via SQL" pattern
//! `crates/server/tests/connections.rs` established — these endpoints only
//! care that the bearer token is valid, not that it came from a real
//! WebAuthn ceremony.

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
        .bind(format!("attestations-test-{identity_id}"))
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

struct RegisteredIssuer {
    signing_key: SigningKey,
    slug: String,
    integrator_id: Uuid,
    key_id: String,
}

async fn register_issuer(
    http: &reqwest::Client,
    base: &str,
    category: &str,
    requested_capabilities: &[&str],
) -> RegisteredIssuer {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let slug = format!("test-issue-{}-{}", category, &suffix[..8]);
    let body = serde_json::json!({
        "slug": slug,
        "name": format!("Test Issuer {}", &suffix[..8]),
        "owner_name": "Test Studio",
        "category": category,
        "requested_capabilities": requested_capabilities,
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

    RegisteredIssuer {
        signing_key,
        slug,
        integrator_id: registered["id"].as_str().unwrap().parse().unwrap(),
        key_id: registered["credential"]["key_id"]
            .as_str()
            .unwrap()
            .to_string(),
    }
}

async fn auth_headers(http: &reqwest::Client, base: &str, issuer: &RegisteredIssuer) -> HeaderMap {
    let challenge: serde_json::Value = http
        .post(format!("{base}/integrations/{}/challenge", issuer.slug))
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

/// The exact canonical bytes `attestation_signing_bytes` produces —
/// duplicated here (not imported, this test crate can't see protocol
/// internals it doesn't depend on) so the test itself proves the wire
/// format, not just that some string round-trips.
fn attestation_signing_bytes(
    claim_kind: &str,
    issuer_ref: &str,
    subject: Uuid,
    achievement: &str,
) -> Vec<u8> {
    format!("avalon:{claim_kind}.issued:v1:{issuer_ref}:{subject}:{achievement}").into_bytes()
}

#[tokio::test]
#[ignore]
async fn a_integrator_issues_a_signed_achievement_to_a_bound_consenting_user() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (identity_id, token) = seed_identity_session(&pool).await;
    let integrator = register_issuer(&http, &base, "game", &["achievements.issue"]).await;

    // Define the achievement.
    let headers = auth_headers(&http, &base, &integrator).await;
    let define = http
        .post(format!(
            "{base}/integrations/{}/achievements",
            integrator.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "key": "dragon_slayer",
            "name": "Dragon Slayer",
            "description": "Slew the dragon",
        }))
        .send()
        .await
        .unwrap();
    assert!(define.status().is_success(), "{:?}", define.status());
    let achievement_id = define.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // The user connects and grants achievements.issue.
    let (signing_key_id, signing_key) = seed_signing_key(&pool, identity_id).await;
    let connect_capabilities = ["achievements.issue"];
    let connect = http
        .post(format!("{base}/integrations/{}/connect", integrator.slug))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "capabilities": connect_capabilities,
            "signing_key_id": signing_key_id,
            "signature": sign_connect(&signing_key, &integrator.slug, &connect_capabilities),
        }))
        .send()
        .await
        .unwrap();
    assert!(connect.status().is_success(), "{:?}", connect.status());

    // Sign and issue the attestation.
    let issuer_ref = format!("game:{}", integrator.slug);
    let signing_bytes =
        attestation_signing_bytes("achievement", &issuer_ref, identity_id, &achievement_id);
    let signature = integrator.signing_key.sign(&signing_bytes);

    let mut headers = auth_headers(&http, &base, &integrator).await;
    headers.insert(
        "x-avalon-identity-id",
        identity_id.to_string().parse().unwrap(),
    );
    let issue = http
        .post(format!(
            "{base}/integrations/{}/achievements/dragon_slayer/issue",
            integrator.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "key_id": integrator.key_id,
            "signature": BASE64.encode(signature.to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert!(issue.status().is_success(), "{:?}", issue.status());
    let attestation: serde_json::Value = issue.json().await.unwrap();
    assert_eq!(attestation["issuer"].as_str().unwrap(), issuer_ref);
    assert_eq!(
        attestation["subject"].as_str().unwrap(),
        identity_id.to_string()
    );
    assert_eq!(attestation["achievement"].as_str().unwrap(), achievement_id);

    // The row landed in achievement_attestations.
    let row = sqlx::query("SELECT integrator_id FROM achievement_attestations WHERE subject = $1")
        .bind(identity_id)
        .fetch_one(&pool)
        .await
        .expect("attestation row should exist");
    let stored_integrator_id: Uuid = sqlx::Row::try_get(&row, "integrator_id").unwrap();
    assert_eq!(stored_integrator_id, integrator.integrator_id);
}

#[tokio::test]
#[ignore]
async fn an_app_issues_a_signed_milestone_to_a_bound_consenting_user() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (identity_id, token) = seed_identity_session(&pool).await;
    let app = register_issuer(&http, &base, "app", &["milestones.issue"]).await;

    let headers = auth_headers(&http, &base, &app).await;
    let define = http
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
    assert!(define.status().is_success(), "{:?}", define.status());
    let milestone_id = define.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (signing_key_id, signing_key) = seed_signing_key(&pool, identity_id).await;
    let connect_capabilities = ["milestones.issue"];
    let connect = http
        .post(format!("{base}/integrations/{}/connect", app.slug))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "capabilities": connect_capabilities,
            "signing_key_id": signing_key_id,
            "signature": sign_connect(&signing_key, &app.slug, &connect_capabilities),
        }))
        .send()
        .await
        .unwrap();
    assert!(connect.status().is_success(), "{:?}", connect.status());

    let issuer_ref = format!("app:{}", app.slug);
    let signing_bytes =
        attestation_signing_bytes("milestone", &issuer_ref, identity_id, &milestone_id);
    let signature = app.signing_key.sign(&signing_bytes);

    let mut headers = auth_headers(&http, &base, &app).await;
    headers.insert(
        "x-avalon-identity-id",
        identity_id.to_string().parse().unwrap(),
    );
    let issue = http
        .post(format!(
            "{base}/integrations/{}/milestones/onboarded/issue",
            app.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "key_id": app.key_id,
            "signature": BASE64.encode(signature.to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert!(issue.status().is_success(), "{:?}", issue.status());
    let attestation: serde_json::Value = issue.json().await.unwrap();
    assert_eq!(attestation["issuer"].as_str().unwrap(), issuer_ref);
    assert_eq!(attestation["achievement"].as_str().unwrap(), milestone_id);
}

#[tokio::test]
#[ignore]
async fn a_tampered_signature_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (identity_id, token) = seed_identity_session(&pool).await;
    let integrator = register_issuer(&http, &base, "game", &["achievements.issue"]).await;

    let headers = auth_headers(&http, &base, &integrator).await;
    http.post(format!(
        "{base}/integrations/{}/achievements",
        integrator.slug
    ))
    .headers(headers)
    .json(&serde_json::json!({
        "key": "dragon_slayer",
        "name": "Dragon Slayer",
        "description": "Slew the dragon",
    }))
    .send()
    .await
    .unwrap();

    let (signing_key_id, signing_key) = seed_signing_key(&pool, identity_id).await;
    let connect_capabilities = ["achievements.issue"];
    http.post(format!("{base}/integrations/{}/connect", integrator.slug))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "capabilities": connect_capabilities,
            "signing_key_id": signing_key_id,
            "signature": sign_connect(&signing_key, &integrator.slug, &connect_capabilities),
        }))
        .send()
        .await
        .unwrap();

    let mut headers = auth_headers(&http, &base, &integrator).await;
    headers.insert(
        "x-avalon-identity-id",
        identity_id.to_string().parse().unwrap(),
    );
    let issue = http
        .post(format!(
            "{base}/integrations/{}/achievements/dragon_slayer/issue",
            integrator.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "key_id": integrator.key_id,
            "signature": BASE64.encode([0u8; 64]),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(issue.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore]
async fn issuance_to_a_non_bound_identity_is_forbidden() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (identity_id, _token) = seed_identity_session(&pool).await;
    let integrator = register_issuer(&http, &base, "game", &["achievements.issue"]).await;

    let headers = auth_headers(&http, &base, &integrator).await;
    http.post(format!(
        "{base}/integrations/{}/achievements",
        integrator.slug
    ))
    .headers(headers)
    .json(&serde_json::json!({
        "key": "dragon_slayer",
        "name": "Dragon Slayer",
        "description": "Slew the dragon",
    }))
    .send()
    .await
    .unwrap();
    // Deliberately never connects/grants.

    let issuer_ref = format!("game:{}", integrator.slug);
    let signing_bytes = attestation_signing_bytes(
        "achievement",
        &issuer_ref,
        identity_id,
        &format!("game:{}:achievement:dragon_slayer", integrator.slug),
    );
    let signature = integrator.signing_key.sign(&signing_bytes);

    let mut headers = auth_headers(&http, &base, &integrator).await;
    headers.insert(
        "x-avalon-identity-id",
        identity_id.to_string().parse().unwrap(),
    );
    let issue = http
        .post(format!(
            "{base}/integrations/{}/achievements/dragon_slayer/issue",
            integrator.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "key_id": integrator.key_id,
            "signature": BASE64.encode(signature.to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(issue.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn issuance_against_a_retired_definition_conflicts() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (identity_id, token) = seed_identity_session(&pool).await;
    let integrator = register_issuer(&http, &base, "game", &["achievements.issue"]).await;

    let headers = auth_headers(&http, &base, &integrator).await;
    http.post(format!(
        "{base}/integrations/{}/achievements",
        integrator.slug
    ))
    .headers(headers)
    .json(&serde_json::json!({
        "key": "dragon_slayer",
        "name": "Dragon Slayer",
        "description": "Slew the dragon",
    }))
    .send()
    .await
    .unwrap();

    let (signing_key_id, signing_key) = seed_signing_key(&pool, identity_id).await;
    let connect_capabilities = ["achievements.issue"];
    http.post(format!("{base}/integrations/{}/connect", integrator.slug))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "capabilities": connect_capabilities,
            "signing_key_id": signing_key_id,
            "signature": sign_connect(&signing_key, &integrator.slug, &connect_capabilities),
        }))
        .send()
        .await
        .unwrap();

    let headers = auth_headers(&http, &base, &integrator).await;
    http.patch(format!(
        "{base}/integrations/{}/achievements/dragon_slayer",
        integrator.slug
    ))
    .headers(headers)
    .json(&serde_json::json!({ "retired": true }))
    .send()
    .await
    .unwrap();

    let issuer_ref = format!("game:{}", integrator.slug);
    let signing_bytes = attestation_signing_bytes(
        "achievement",
        &issuer_ref,
        identity_id,
        &format!("game:{}:achievement:dragon_slayer", integrator.slug),
    );
    let signature = integrator.signing_key.sign(&signing_bytes);

    let mut headers = auth_headers(&http, &base, &integrator).await;
    headers.insert(
        "x-avalon-identity-id",
        identity_id.to_string().parse().unwrap(),
    );
    let issue = http
        .post(format!(
            "{base}/integrations/{}/achievements/dragon_slayer/issue",
            integrator.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "key_id": integrator.key_id,
            "signature": BASE64.encode(signature.to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(issue.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test]
#[ignore]
async fn list_my_achievements_paginates_and_filters_by_integrator_and_claim_kind() {
    // Issue #377: the issuance ceremony itself is covered by the other
    // tests in this file; this one only exercises GET /me/achievements'
    // read-side filtering/pagination, so the attestation rows are seeded
    // directly rather than issued through a real signed ceremony each.
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (identity_id, token) = seed_identity_session(&pool).await;

    let game = register_issuer(&http, &base, "game", &[]).await;
    let app = register_issuer(&http, &base, "app", &[]).await;

    for i in 0..3 {
        sqlx::query(
            "INSERT INTO achievement_attestations \
             (id, integrator_id, issuer, subject, achievement, issued_at, \
              proof_key_id, proof_algorithm, proof_bytes) \
             VALUES ($1, $2, $3, $4, $5, now() - ($6 || ' seconds')::interval, $7, 'ed25519', $8)",
        )
        .bind(Uuid::new_v4())
        .bind(game.integrator_id)
        .bind(format!("game:{}", game.slug))
        .bind(identity_id)
        .bind(format!("game:{}:achievement:test-{i}", game.slug))
        .bind(i.to_string())
        .bind(Uuid::new_v4())
        .bind(vec![0u8; 4])
        .execute(&pool)
        .await
        .expect("failed to seed achievement attestation");
    }
    sqlx::query(
        "INSERT INTO achievement_attestations \
         (id, integrator_id, issuer, subject, achievement, issued_at, \
          proof_key_id, proof_algorithm, proof_bytes) \
         VALUES ($1, $2, $3, $4, $5, now(), $6, 'ed25519', $7)",
    )
    .bind(Uuid::new_v4())
    .bind(app.integrator_id)
    .bind(format!("app:{}", app.slug))
    .bind(identity_id)
    .bind(format!("app:{}:milestone:test-0", app.slug))
    .bind(Uuid::new_v4())
    .bind(vec![0u8; 4])
    .execute(&pool)
    .await
    .expect("failed to seed milestone attestation");

    let achievements_only: serde_json::Value = http
        .get(format!("{base}/me/achievements?claim_kind=achievement"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        achievements_only["achievements"].as_array().unwrap().len(),
        3
    );

    let milestones_only: serde_json::Value = http
        .get(format!("{base}/me/achievements?claim_kind=milestone"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(milestones_only["achievements"].as_array().unwrap().len(), 1);

    let game_only: serde_json::Value = http
        .get(format!(
            "{base}/me/achievements?integrator_id={}",
            game.integrator_id
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(game_only["achievements"].as_array().unwrap().len(), 3);

    // Pagination: 4 total attestations seeded, limit=2 should split into
    // two pages with a real next_cursor between them.
    let page1: serde_json::Value = http
        .get(format!("{base}/me/achievements?limit=2"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page1["achievements"].as_array().unwrap().len(), 2);
    let cursor = page1["next_cursor"]
        .as_str()
        .expect("expected a next_cursor with more rows remaining");

    let page2: serde_json::Value = http
        .get(format!("{base}/me/achievements?limit=2&before={cursor}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page2["achievements"].as_array().unwrap().len(), 2);
    assert!(page2["next_cursor"].is_null());

    let bad = http
        .get(format!("{base}/me/achievements?claim_kind=quest"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), reqwest::StatusCode::BAD_REQUEST);
}
