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

struct RegisteredIssuer {
    signing_key: SigningKey,
    slug: String,
    game_id: Uuid,
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
        "developer": "Test Studio",
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
        game_id: registered["id"].as_str().unwrap().parse().unwrap(),
        key_id: registered["credential"]["key_id"]
            .as_str()
            .unwrap()
            .to_string(),
    }
}

async fn auth_headers(http: &reqwest::Client, base: &str, issuer: &RegisteredIssuer) -> HeaderMap {
    let challenge: serde_json::Value = http
        .post(format!("{base}/games/{}/challenge", issuer.slug))
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
async fn a_game_issues_a_signed_achievement_to_a_bound_consenting_user() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (identity_id, token) = seed_identity_session(&pool).await;
    let game = register_issuer(&http, &base, "game", &["achievements.issue"]).await;

    // Define the achievement.
    let headers = auth_headers(&http, &base, &game).await;
    let define = http
        .post(format!("{base}/games/{}/achievements", game.slug))
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
    let connect = http
        .post(format!("{base}/games/{}/connect", game.slug))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "capabilities": ["achievements.issue"] }))
        .send()
        .await
        .unwrap();
    assert!(connect.status().is_success(), "{:?}", connect.status());

    // Sign and issue the attestation.
    let issuer_ref = format!("game:{}", game.slug);
    let signing_bytes =
        attestation_signing_bytes("achievement", &issuer_ref, identity_id, &achievement_id);
    let signature = game.signing_key.sign(&signing_bytes);

    let mut headers = auth_headers(&http, &base, &game).await;
    headers.insert(
        "x-avalon-identity-id",
        identity_id.to_string().parse().unwrap(),
    );
    let issue = http
        .post(format!(
            "{base}/games/{}/achievements/dragon_slayer/issue",
            game.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "key_id": game.key_id,
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
    let row = sqlx::query("SELECT game_id FROM achievement_attestations WHERE subject = $1")
        .bind(identity_id)
        .fetch_one(&pool)
        .await
        .expect("attestation row should exist");
    let stored_game_id: Uuid = sqlx::Row::try_get(&row, "game_id").unwrap();
    assert_eq!(stored_game_id, game.game_id);
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

    let connect = http
        .post(format!("{base}/games/{}/connect", app.slug))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "capabilities": ["milestones.issue"] }))
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
    let game = register_issuer(&http, &base, "game", &["achievements.issue"]).await;

    let headers = auth_headers(&http, &base, &game).await;
    http.post(format!("{base}/games/{}/achievements", game.slug))
        .headers(headers)
        .json(&serde_json::json!({
            "key": "dragon_slayer",
            "name": "Dragon Slayer",
            "description": "Slew the dragon",
        }))
        .send()
        .await
        .unwrap();

    http.post(format!("{base}/games/{}/connect", game.slug))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "capabilities": ["achievements.issue"] }))
        .send()
        .await
        .unwrap();

    let mut headers = auth_headers(&http, &base, &game).await;
    headers.insert(
        "x-avalon-identity-id",
        identity_id.to_string().parse().unwrap(),
    );
    let issue = http
        .post(format!(
            "{base}/games/{}/achievements/dragon_slayer/issue",
            game.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "key_id": game.key_id,
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
    let game = register_issuer(&http, &base, "game", &["achievements.issue"]).await;

    let headers = auth_headers(&http, &base, &game).await;
    http.post(format!("{base}/games/{}/achievements", game.slug))
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

    let issuer_ref = format!("game:{}", game.slug);
    let signing_bytes = attestation_signing_bytes(
        "achievement",
        &issuer_ref,
        identity_id,
        &format!("game:{}:achievement:dragon_slayer", game.slug),
    );
    let signature = game.signing_key.sign(&signing_bytes);

    let mut headers = auth_headers(&http, &base, &game).await;
    headers.insert(
        "x-avalon-identity-id",
        identity_id.to_string().parse().unwrap(),
    );
    let issue = http
        .post(format!(
            "{base}/games/{}/achievements/dragon_slayer/issue",
            game.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "key_id": game.key_id,
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
    let game = register_issuer(&http, &base, "game", &["achievements.issue"]).await;

    let headers = auth_headers(&http, &base, &game).await;
    http.post(format!("{base}/games/{}/achievements", game.slug))
        .headers(headers)
        .json(&serde_json::json!({
            "key": "dragon_slayer",
            "name": "Dragon Slayer",
            "description": "Slew the dragon",
        }))
        .send()
        .await
        .unwrap();

    http.post(format!("{base}/games/{}/connect", game.slug))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "capabilities": ["achievements.issue"] }))
        .send()
        .await
        .unwrap();

    let headers = auth_headers(&http, &base, &game).await;
    http.patch(format!(
        "{base}/games/{}/achievements/dragon_slayer",
        game.slug
    ))
    .headers(headers)
    .json(&serde_json::json!({ "retired": true }))
    .send()
    .await
    .unwrap();

    let issuer_ref = format!("game:{}", game.slug);
    let signing_bytes = attestation_signing_bytes(
        "achievement",
        &issuer_ref,
        identity_id,
        &format!("game:{}:achievement:dragon_slayer", game.slug),
    );
    let signature = game.signing_key.sign(&signing_bytes);

    let mut headers = auth_headers(&http, &base, &game).await;
    headers.insert(
        "x-avalon-identity-id",
        identity_id.to_string().parse().unwrap(),
    );
    let issue = http
        .post(format!(
            "{base}/games/{}/achievements/dragon_slayer/issue",
            game.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "key_id": game.key_id,
            "signature": BASE64.encode(signature.to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(issue.status(), reqwest::StatusCode::CONFLICT);
}
