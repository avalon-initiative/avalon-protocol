//! Exercises cross-device pairing (issue #307) against a real, running
//! `avalon-server` and Postgres. Gated `--ignored` since it needs live
//! infra — see `make test-live` / `make start`.
//!
//! Test identities/sessions are seeded directly via SQL, same approach as
//! `crates/server/tests/device_grants.rs`.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

/// #697/#698: seeds a real signing key for `identity_id` so a test can
/// produce a genuine fresh-signature over HTTP, same pattern
/// `crates/server/tests/device_grants.rs` already established.
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
    let key_id: Uuid = sqlx::Row::try_get(&row, "id").unwrap();
    (key_id, signing_key)
}

/// Mirrors `crate::signature_gate::canonical_message` byte-for-byte.
fn sign_action(signing_key: &SigningKey, action_tag: &str, fields: &[&str]) -> String {
    let mut message = format!("avalon:{action_tag}:v1");
    for field in fields {
        message.push(':');
        message.push_str(field);
    }
    BASE64.encode(signing_key.sign(message.as_bytes()).to_bytes())
}

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
        .bind(format!("device-pairing-test-{identity_id}"))
        .execute(pool)
        .await
        .expect("failed to seed profile");

    let token = format!("test-token-{}", Uuid::new_v4());
    let expires_at = OffsetDateTime::now_utc() + time::Duration::hours(1);
    sqlx::query("INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)")
        .bind(&token)
        .bind(identity_id)
        .bind(expires_at)
        .execute(pool)
        .await
        .expect("failed to seed session");

    (identity_id, token)
}

fn auth(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request.bearer_auth(token)
}

async fn start_pairing(http: &reqwest::Client, base: &str) -> serde_json::Value {
    http.post(format!("{base}/auth/device/start"))
        .send()
        .await
        .expect("device/start request failed — is `make start` running?")
        .json()
        .await
        .expect("device/start response was not JSON")
}

#[tokio::test]
#[ignore]
async fn full_round_trip_start_approve_poll_delivers_a_real_session_exactly_once() {
    let http = reqwest::Client::new();
    let pool = test_pool().await;
    let base = server_url();
    let (identity_id, approver_token) = seed_identity_session(&pool).await;

    let start = start_pairing(&http, &base).await;
    let device_code = start["device_code"].as_str().unwrap().to_string();
    let user_code = start["user_code"].as_str().unwrap().to_string();
    assert!(start["expires_in"].as_i64().unwrap() > 0);
    assert!(start["verification_uri"]
        .as_str()
        .unwrap()
        .contains(&user_code));

    // Not yet approved: polling returns pending.
    let pending: serde_json::Value =
        auth(http.post(format!("{base}/auth/device/poll")), &device_code)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(pending["status"], "pending");

    let (signing_key_id, signing_key) = seed_signing_key(&pool, identity_id).await;
    let signature = sign_action(
        &signing_key,
        "device_pairing.approve",
        &[&identity_id.to_string(), &user_code],
    );
    let approve = auth(
        http.post(format!("{base}/auth/device/approve")),
        &approver_token,
    )
    .json(&serde_json::json!({
        "user_code": user_code,
        "signing_key_id": signing_key_id,
        "signature": signature,
    }))
    .send()
    .await
    .unwrap();
    assert!(approve.status().is_success(), "{:?}", approve.status());

    let approved: serde_json::Value =
        auth(http.post(format!("{base}/auth/device/poll")), &device_code)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(approved["status"], "approved");
    let token = approved["token"]
        .as_str()
        .expect("approved poll should carry a token");
    assert!(approved["expires_at"].is_string());

    // The minted token is a real session for the approver's identity.
    let me: serde_json::Value = auth(http.get(format!("{base}/me")), token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["identity_id"], identity_id.to_string());

    // Single-use: a second poll of the same device_code never returns the
    // token again.
    let second_poll: serde_json::Value =
        auth(http.post(format!("{base}/auth/device/poll")), &device_code)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(second_poll["status"], "expired");
    assert!(second_poll["token"].is_null());
}

/// Issue #704's gap #1: an ambient-session-only approve (no
/// `signing_key_id`/`signature` at all, and the approver has never
/// registered a signing key) must be rejected outright — a stolen bearer
/// token alone must not be able to mint a second session.
#[tokio::test]
#[ignore]
async fn approving_with_only_the_ambient_session_and_no_signing_key_is_rejected() {
    let http = reqwest::Client::new();
    let pool = test_pool().await;
    let base = server_url();
    let (_identity_id, approver_token) = seed_identity_session(&pool).await;

    let start = start_pairing(&http, &base).await;
    let user_code = start["user_code"].as_str().unwrap().to_string();

    let approve = auth(
        http.post(format!("{base}/auth/device/approve")),
        &approver_token,
    )
    .json(&serde_json::json!({ "user_code": user_code }))
    .send()
    .await
    .unwrap();
    assert_eq!(approve.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = approve.json().await.unwrap();
    assert_eq!(body["code"], "NO_REGISTERED_SIGNING_KEY");
}

/// Same gap, but the approver *does* have a registered signing key and
/// still omits the signature — a different, more specific rejection than
/// the no-key case above.
#[tokio::test]
#[ignore]
async fn approving_with_a_registered_key_but_no_signature_is_rejected() {
    let http = reqwest::Client::new();
    let pool = test_pool().await;
    let base = server_url();
    let (identity_id, approver_token) = seed_identity_session(&pool).await;
    seed_signing_key(&pool, identity_id).await;

    let start = start_pairing(&http, &base).await;
    let user_code = start["user_code"].as_str().unwrap().to_string();

    let approve = auth(
        http.post(format!("{base}/auth/device/approve")),
        &approver_token,
    )
    .json(&serde_json::json!({ "user_code": user_code }))
    .send()
    .await
    .unwrap();
    assert_eq!(approve.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: serde_json::Value = approve.json().await.unwrap();
    assert_eq!(body["code"], "FRESH_SIGNATURE_REQUIRED");
}

#[tokio::test]
#[ignore]
async fn approving_with_the_wrong_user_code_fails() {
    let http = reqwest::Client::new();
    let pool = test_pool().await;
    let base = server_url();
    let (_identity_id, approver_token) = seed_identity_session(&pool).await;

    let _start = start_pairing(&http, &base).await;

    let approve = auth(
        http.post(format!("{base}/auth/device/approve")),
        &approver_token,
    )
    .json(&serde_json::json!({ "user_code": "WRONGCODE" }))
    .send()
    .await
    .unwrap();
    assert_eq!(approve.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore]
async fn a_denied_pairing_polls_denied_and_can_never_be_approved_afterward() {
    let http = reqwest::Client::new();
    let pool = test_pool().await;
    let base = server_url();
    let (_identity_id, approver_token) = seed_identity_session(&pool).await;

    let start = start_pairing(&http, &base).await;
    let device_code = start["device_code"].as_str().unwrap().to_string();
    let user_code = start["user_code"].as_str().unwrap().to_string();

    let deny = auth(
        http.post(format!("{base}/auth/device/deny")),
        &approver_token,
    )
    .json(&serde_json::json!({ "user_code": user_code }))
    .send()
    .await
    .unwrap();
    assert!(deny.status().is_success(), "{:?}", deny.status());

    let polled: serde_json::Value =
        auth(http.post(format!("{base}/auth/device/poll")), &device_code)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(polled["status"], "denied");

    let approve_after_deny = auth(
        http.post(format!("{base}/auth/device/approve")),
        &approver_token,
    )
    .json(&serde_json::json!({ "user_code": user_code }))
    .send()
    .await
    .unwrap();
    assert_eq!(approve_after_deny.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore]
async fn polling_faster_than_the_advertised_interval_returns_slow_down() {
    let http = reqwest::Client::new();
    let base = server_url();

    let start = start_pairing(&http, &base).await;
    let device_code = start["device_code"].as_str().unwrap().to_string();

    let first: serde_json::Value =
        auth(http.post(format!("{base}/auth/device/poll")), &device_code)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(first["status"], "pending");

    let second: serde_json::Value =
        auth(http.post(format!("{base}/auth/device/poll")), &device_code)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(second["status"], "slow_down");
}

#[tokio::test]
#[ignore]
async fn polling_an_unknown_device_code_is_unauthorized() {
    let http = reqwest::Client::new();
    let base = server_url();

    let response = auth(
        http.post(format!("{base}/auth/device/poll")),
        "not-a-real-device-code",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}
