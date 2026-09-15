//! Exercises the per-network issuer registration gate (issue #481,
//! implementing the ADR decided in #479) against a real, running
//! `avalon-server` and Postgres. Gated `--ignored` since it needs live
//! infra — see `make test-live` / `make start`.
//!
//! Same helper shapes `crates/server/tests/attestations.rs` already
//! establishes (register an issuer, sign a challenge, issue an
//! attestation) — duplicated here rather than shared, matching this
//! repo's own per-test-file convention.

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
        .bind(format!("issuer-registration-test-{identity_id}"))
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
    key_id: String,
}

async fn register_issuer(http: &reqwest::Client, base: &str) -> RegisteredIssuer {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let slug = format!("test-issuer-reg-{}", &suffix[..12]);
    let body = serde_json::json!({
        "slug": slug,
        "name": format!("Test Issuer {}", &suffix[..8]),
        "owner_name": "Test Studio",
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
        .expect("register integrator failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());
    let registered: serde_json::Value = response.json().await.unwrap();

    RegisteredIssuer {
        signing_key,
        slug,
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

fn attestation_signing_bytes(
    claim_kind: &str,
    issuer_ref: &str,
    subject: Uuid,
    achievement: &str,
) -> Vec<u8> {
    format!("avalon:{claim_kind}.issued:v1:{issuer_ref}:{subject}:{achievement}").into_bytes()
}

/// Full attestation-issuance flow, mirroring
/// `attestations.rs::a_integrator_issues_a_signed_achievement_to_a_bound_consenting_user`
/// — used here as the vehicle to exercise the write-path registration gate,
/// not to re-test issuance itself.
async fn issue_achievement_attestation(
    http: &reqwest::Client,
    base: &str,
    issuer: &RegisteredIssuer,
    identity_id: Uuid,
    token: &str,
) -> reqwest::Response {
    let headers = auth_headers(http, base, issuer).await;
    let define = http
        .post(format!("{base}/integrations/{}/achievements", issuer.slug))
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

    let connect = http
        .post(format!("{base}/integrations/{}/connect", issuer.slug))
        .bearer_auth(token)
        .json(&serde_json::json!({ "capabilities": ["achievements.issue"] }))
        .send()
        .await
        .unwrap();
    assert!(connect.status().is_success(), "{:?}", connect.status());

    let issuer_ref = format!("game:{}", issuer.slug);
    let signing_bytes =
        attestation_signing_bytes("achievement", &issuer_ref, identity_id, &achievement_id);
    let signature = issuer.signing_key.sign(&signing_bytes);

    let mut headers = auth_headers(http, base, issuer).await;
    headers.insert(
        "x-avalon-identity-id",
        identity_id.to_string().parse().unwrap(),
    );
    http.post(format!(
        "{base}/integrations/{}/achievements/dragon_slayer/issue",
        issuer.slug
    ))
    .headers(headers)
    .json(&serde_json::json!({
        "key_id": issuer.key_id,
        "signature": BASE64.encode(signature.to_bytes()),
    }))
    .send()
    .await
    .unwrap()
}

#[tokio::test]
#[ignore]
async fn a_never_before_seen_key_auto_registers_on_the_dev_network() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (identity_id, token) = seed_identity_session(&pool).await;
    let issuer = register_issuer(&http, &base).await;

    // Never called POST /issuers/register for this key — the local dev
    // server's network_id (avalon-dev-local) auto-registers on the first
    // valid signed write instead of rejecting it (#481's dev/int policy).
    let response = issue_achievement_attestation(&http, &base, &issuer, identity_id, &token).await;
    assert!(response.status().is_success(), "{:?}", response.status());

    let pubkey_bytes = issuer.signing_key.verifying_key().to_bytes();
    let row = sqlx::query(
        "SELECT issuer_ref, auto_registered FROM issuer_network_registrations WHERE issuer_pubkey = $1",
    )
    .bind(pubkey_bytes.as_slice())
    .fetch_one(&pool)
    .await
    .expect("auto-registration row should exist after the write");
    let issuer_ref: String = sqlx::Row::try_get(&row, "issuer_ref").unwrap();
    let auto_registered: bool = sqlx::Row::try_get(&row, "auto_registered").unwrap();
    assert_eq!(issuer_ref, format!("game:{}", issuer.slug));
    assert!(auto_registered);
}

#[tokio::test]
#[ignore]
async fn explicit_registration_round_trips_and_write_path_no_longer_needs_to_auto_register() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (identity_id, token) = seed_identity_session(&pool).await;
    let issuer = register_issuer(&http, &base).await;
    let issuer_ref = format!("game:{}", issuer.slug);
    let pubkey_b64 = BASE64.encode(issuer.signing_key.verifying_key().as_bytes());

    let network_id: serde_json::Value = http
        .get(format!("{base}/ledger/sth/latest"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let network_id = network_id["network_id"].as_str().unwrap().to_string();

    let challenge: serde_json::Value = http
        .post(format!("{base}/issuers/registration-challenge"))
        .json(&serde_json::json!({ "issuer_pubkey": pubkey_b64 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = challenge["nonce"].as_str().unwrap();

    let message = format!("avalon:issuer.registered:v1:{issuer_ref}:{network_id}:{nonce}");
    let signature = issuer.signing_key.sign(message.as_bytes());

    let register = http
        .post(format!("{base}/issuers/register"))
        .json(&serde_json::json!({
            "issuer_pubkey": pubkey_b64,
            "issuer_ref": issuer_ref,
            "declared_network_id": network_id,
            "challenge_id": challenge_id,
            "proof_of_possession_signature": BASE64.encode(signature.to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert!(register.status().is_success(), "{:?}", register.status());
    let registered: serde_json::Value = register.json().await.unwrap();
    assert_eq!(registered["issuer_ref"].as_str().unwrap(), issuer_ref);

    let pubkey_bytes = issuer.signing_key.verifying_key().to_bytes();
    let row = sqlx::query(
        "SELECT auto_registered FROM issuer_network_registrations WHERE issuer_pubkey = $1",
    )
    .bind(pubkey_bytes.as_slice())
    .fetch_one(&pool)
    .await
    .expect("explicit registration row should exist");
    let auto_registered: bool = sqlx::Row::try_get(&row, "auto_registered").unwrap();
    assert!(!auto_registered);

    // The write path now finds this key already registered — no
    // auto-registration needed, the same success path either way.
    let response = issue_achievement_attestation(&http, &base, &issuer, identity_id, &token).await;
    assert!(response.status().is_success(), "{:?}", response.status());
}

#[tokio::test]
#[ignore]
async fn a_registration_challenge_is_single_use() {
    let http = reqwest::Client::new();
    let base = server_url();
    let issuer = register_issuer(&http, &base).await;
    let issuer_ref = format!("game:{}", issuer.slug);
    let pubkey_b64 = BASE64.encode(issuer.signing_key.verifying_key().as_bytes());

    let network_id: serde_json::Value = http
        .get(format!("{base}/ledger/sth/latest"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let network_id = network_id["network_id"].as_str().unwrap().to_string();

    let challenge: serde_json::Value = http
        .post(format!("{base}/issuers/registration-challenge"))
        .json(&serde_json::json!({ "issuer_pubkey": pubkey_b64 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = challenge["nonce"].as_str().unwrap();
    let message = format!("avalon:issuer.registered:v1:{issuer_ref}:{network_id}:{nonce}");
    let signature = issuer.signing_key.sign(message.as_bytes());
    let body = serde_json::json!({
        "issuer_pubkey": pubkey_b64,
        "issuer_ref": issuer_ref,
        "declared_network_id": network_id,
        "challenge_id": challenge_id,
        "proof_of_possession_signature": BASE64.encode(signature.to_bytes()),
    });

    let first = http
        .post(format!("{base}/issuers/register"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success(), "{:?}", first.status());

    let second = http
        .post(format!("{base}/issuers/register"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore]
async fn a_declared_network_id_that_does_not_match_this_server_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let issuer = register_issuer(&http, &base).await;
    let issuer_ref = format!("game:{}", issuer.slug);
    let pubkey_b64 = BASE64.encode(issuer.signing_key.verifying_key().as_bytes());

    let challenge: serde_json::Value = http
        .post(format!("{base}/issuers/registration-challenge"))
        .json(&serde_json::json!({ "issuer_pubkey": pubkey_b64 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = challenge["nonce"].as_str().unwrap();
    let wrong_network_id = "avalon-mainnet-1";
    let message = format!("avalon:issuer.registered:v1:{issuer_ref}:{wrong_network_id}:{nonce}");
    let signature = issuer.signing_key.sign(message.as_bytes());

    let response = http
        .post(format!("{base}/issuers/register"))
        .json(&serde_json::json!({
            "issuer_pubkey": pubkey_b64,
            "issuer_ref": issuer_ref,
            "declared_network_id": wrong_network_id,
            "challenge_id": challenge_id,
            "proof_of_possession_signature": BASE64.encode(signature.to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore]
async fn a_proof_of_possession_signed_by_the_wrong_key_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let issuer = register_issuer(&http, &base).await;
    let impostor = SigningKey::generate(&mut rand::rng());
    let issuer_ref = format!("game:{}", issuer.slug);
    let pubkey_b64 = BASE64.encode(issuer.signing_key.verifying_key().as_bytes());

    let network_id: serde_json::Value = http
        .get(format!("{base}/ledger/sth/latest"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let network_id = network_id["network_id"].as_str().unwrap().to_string();

    let challenge: serde_json::Value = http
        .post(format!("{base}/issuers/registration-challenge"))
        .json(&serde_json::json!({ "issuer_pubkey": pubkey_b64 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = challenge["nonce"].as_str().unwrap();
    let message = format!("avalon:issuer.registered:v1:{issuer_ref}:{network_id}:{nonce}");
    // Signed by a key that has nothing to do with `issuer_pubkey` — proves
    // possession of the *wrong* key.
    let signature = impostor.sign(message.as_bytes());

    let response = http
        .post(format!("{base}/issuers/register"))
        .json(&serde_json::json!({
            "issuer_pubkey": pubkey_b64,
            "issuer_ref": issuer_ref,
            "declared_network_id": network_id,
            "challenge_id": challenge_id,
            "proof_of_possession_signature": BASE64.encode(signature.to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}
