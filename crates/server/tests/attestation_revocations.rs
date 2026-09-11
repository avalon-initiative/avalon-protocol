//! Exercises attestation revocation (issue #85, implementing #81's decided
//! mechanics) against a real, running `avalon-server` and Postgres. Gated
//! `--ignored` since it needs live infra — see `make test-live` / `make
//! start`.
//!
//! "Scenario C" (`docs/architecture/revocation.md`): issue then revoke;
//! history shows both; `validity(at)` flips at the revocation timestamp.

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
        .bind(format!("revoke-test-{identity_id}"))
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

struct RegisteredGame {
    signing_key: SigningKey,
    slug: String,
    key_id: String,
}

async fn register_game(http: &reqwest::Client, base: &str) -> RegisteredGame {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let slug = format!("test-revoke-{}", &suffix[..10]);
    let body = serde_json::json!({
        "slug": slug,
        "name": format!("Revoke Test {}", &suffix[..8]),
        "developer": "Test Studio",
        "requested_capabilities": ["achievements.issue"],
        "initial_key": {
            "algorithm": "ed25519",
            "public_key": BASE64.encode(signing_key.verifying_key().as_bytes()),
        },
    });
    let response = http
        .post(format!("{base}/games"))
        .json(&body)
        .send()
        .await
        .expect("register game failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());
    let registered: serde_json::Value = response.json().await.unwrap();
    RegisteredGame {
        signing_key,
        slug,
        key_id: registered["credential"]["key_id"]
            .as_str()
            .unwrap()
            .to_string(),
    }
}

async fn auth_headers(http: &reqwest::Client, base: &str, game: &RegisteredGame) -> HeaderMap {
    let challenge: serde_json::Value = http
        .post(format!("{base}/games/{}/challenge", game.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let signature = game.signing_key.sign(&nonce);

    let mut headers = HeaderMap::new();
    headers.insert("x-avalon-integrator-key-id", game.key_id.parse().unwrap());
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

fn attestation_signing_bytes(
    claim_kind: &str,
    issuer_ref: &str,
    subject: Uuid,
    achievement: &str,
) -> Vec<u8> {
    format!("avalon:{claim_kind}.issued:v1:{issuer_ref}:{subject}:{achievement}").into_bytes()
}

fn revocation_signing_bytes(
    claim_kind: &str,
    issuer_ref: &str,
    attestation_id: Uuid,
    reason_code: &str,
) -> Vec<u8> {
    format!("avalon:{claim_kind}.revoked:v1:{issuer_ref}:{attestation_id}:{reason_code}")
        .into_bytes()
}

/// Full setup shared by every test below: register a game, define an
/// achievement, connect + grant, issue it. Returns the game and the new
/// attestation's id.
async fn issue_one(
    http: &reqwest::Client,
    base: &str,
    pool: &PgPool,
) -> (RegisteredGame, String, Uuid) {
    let (identity_id, token) = seed_identity_session(pool).await;
    let game = register_game(http, base).await;

    let headers = auth_headers(http, base, &game).await;
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
    let achievement_id = define.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    http.post(format!("{base}/games/{}/connect", game.slug))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "capabilities": ["achievements.issue"] }))
        .send()
        .await
        .unwrap();

    let issuer_ref = format!("game:{}", game.slug);
    let signing_bytes =
        attestation_signing_bytes("achievement", &issuer_ref, identity_id, &achievement_id);
    let signature = game.signing_key.sign(&signing_bytes);

    let mut headers = auth_headers(http, base, &game).await;
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
    let attestation_id: Uuid = issue.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    (game, issuer_ref, attestation_id)
}

#[tokio::test]
#[ignore]
async fn scenario_c_issue_then_revoke_flips_validity_and_history_shows_both() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (game, issuer_ref, attestation_id) = issue_one(&http, &base, &pool).await;

    let before = http
        .get(format!("{base}/attestations/{attestation_id}"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(before["validity"]["status"], "valid");
    assert_eq!(before["history"].as_array().unwrap().len(), 1);

    let reason_code = "cheating_detected";
    let signing_bytes =
        revocation_signing_bytes("achievement", &issuer_ref, attestation_id, reason_code);
    let signature = game.signing_key.sign(&signing_bytes);

    let headers = auth_headers(&http, &base, &game).await;
    let revoke = http
        .post(format!("{base}/attestations/{attestation_id}/revoke"))
        .headers(headers)
        .json(&serde_json::json!({
            "key_id": game.key_id,
            "signature": BASE64.encode(signature.to_bytes()),
            "reason_code": reason_code,
            "reason": "Player used unauthorized tooling",
        }))
        .send()
        .await
        .unwrap();
    assert!(revoke.status().is_success(), "{:?}", revoke.status());

    let after = http
        .get(format!("{base}/attestations/{attestation_id}"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(after["validity"]["status"], "invalid");
    // Authenticity is untouched by revocation — a revoked claim was still
    // genuinely signed by the issuer.
    assert_eq!(after["authenticity"]["status"], "authentic");
    let history = after["history"].as_array().unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0]["event"], "issued");
    assert_eq!(history[1]["event"], "revoked");
    assert_eq!(history[1]["reason_code"], reason_code);
}

#[tokio::test]
#[ignore]
async fn revoking_twice_is_rejected_not_silently_accepted() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (game, issuer_ref, attestation_id) = issue_one(&http, &base, &pool).await;

    let revoke_once = |reason_code: &'static str| {
        let http = http.clone();
        let base = base.clone();
        let issuer_ref = issuer_ref.clone();
        let key_id = game.key_id.clone();
        let signing_key_bytes = game.signing_key.to_bytes();
        async move {
            let signing_key = SigningKey::from_bytes(&signing_key_bytes);
            let signing_bytes =
                revocation_signing_bytes("achievement", &issuer_ref, attestation_id, reason_code);
            let signature = signing_key.sign(&signing_bytes);
            let challenge: serde_json::Value = http
                .post(format!(
                    "{base}/games/{}/challenge",
                    issuer_ref.strip_prefix("game:").unwrap()
                ))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let challenge_id = challenge["challenge_id"].as_str().unwrap();
            let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
            let challenge_signature = signing_key.sign(&nonce);
            http.post(format!("{base}/attestations/{attestation_id}/revoke"))
                .header("x-avalon-integrator-key-id", &key_id)
                .header("x-avalon-integrator-challenge-id", challenge_id)
                .header(
                    "x-avalon-integrator-signature",
                    BASE64.encode(challenge_signature.to_bytes()),
                )
                .json(&serde_json::json!({
                    "key_id": key_id,
                    "signature": BASE64.encode(signature.to_bytes()),
                    "reason_code": reason_code,
                    "reason": "test",
                }))
                .send()
                .await
                .unwrap()
        }
    };

    let first = revoke_once("first_reason").await;
    assert!(first.status().is_success(), "{:?}", first.status());
    let second = revoke_once("second_reason").await;
    assert_eq!(second.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test]
#[ignore]
async fn only_the_original_issuer_may_revoke() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_original_game, _issuer_ref, attestation_id) = issue_one(&http, &base, &pool).await;

    // A completely unrelated game tries to revoke it.
    let intruder = register_game(&http, &base).await;
    let headers = auth_headers(&http, &base, &intruder).await;
    let attempt = http
        .post(format!("{base}/attestations/{attestation_id}/revoke"))
        .headers(headers)
        .json(&serde_json::json!({
            "key_id": intruder.key_id,
            "signature": BASE64.encode([0u8; 64]),
            "reason_code": "malicious",
            "reason": "not my attestation to revoke",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(attempt.status(), reqwest::StatusCode::FORBIDDEN);
}
