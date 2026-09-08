//! Exercises the device-registration/linked-device grant model (issue
//! #135) against a real, running `avalon-server` and Postgres. Gated
//! `--ignored` since it needs live infra — see `make test-live` / `make
//! start`.
//!
//! Test identities/sessions are seeded directly via SQL, same approach as
//! `crates/server/tests/friends.rs` — a signing key is seeded the same way
//! too, but with a real, known `ed25519_dalek::SigningKey` kept in the test
//! so it can produce a genuine approval signature over HTTP, exercising the
//! real verification path rather than bypassing it.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
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
        .bind(format!("device-grants-test-{identity_id}"))
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

/// Seeds a real signing key row for `identity_id` and returns its id plus
/// the actual private key, so the test can sign a genuine grant approval
/// with it — exercising `devices::approve_device_grant`'s real
/// `verify_event_signature` check, not a bypass.
async fn seed_signing_key(pool: &PgPool, identity_id: Uuid) -> (Uuid, SigningKey) {
    let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
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

fn auth(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request.bearer_auth(token)
}

/// Mirrors `crates/server/src/devices.rs`'s private
/// `device_grant_approval_signing_bytes` exactly — duplicated here since
/// this test doesn't depend on `avalon-server`'s internals.
fn device_grant_approval_signing_bytes(
    grant_id: Uuid,
    identity_id: Uuid,
    requested_signing_public_key: &[u8],
) -> Vec<u8> {
    format!(
        "avalon:device_grant.approved:v1:{grant_id}:{identity_id}:{}",
        BASE64.encode(requested_signing_public_key)
    )
    .into_bytes()
}

#[tokio::test]
#[ignore]
async fn a_grant_approved_by_a_valid_trusted_key_succeeds_and_the_new_key_is_registered() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, token) = seed_identity_session(&pool).await;
    let (trusted_key_id, trusted_key) = seed_signing_key(&pool, identity_id).await;

    let devices_before: serde_json::Value = auth(http.get(format!("{base}/me/devices")), &token)
        .send()
        .await
        .expect("list devices failed — is `make start` running?")
        .json()
        .await
        .unwrap();
    assert_eq!(devices_before.as_array().unwrap().len(), 1);

    let new_device_signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
    let new_device_public_key = new_device_signing_key.verifying_key().to_bytes();

    let request_body = serde_json::json!({
        "requested_signing_public_key": BASE64.encode(new_device_public_key),
        "device_label": "second device",
    });
    let grant: serde_json::Value = auth(http.post(format!("{base}/me/devices/grants")), &token)
        .json(&request_body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(grant["status"], "pending");
    let grant_id: Uuid = grant["id"].as_str().unwrap().parse().unwrap();

    let signing_bytes =
        device_grant_approval_signing_bytes(grant_id, identity_id, &new_device_public_key);
    let signature = trusted_key.sign(&signing_bytes);

    let approve_body = serde_json::json!({
        "approver_signing_key_id": trusted_key_id,
        "signature": BASE64.encode(signature.to_bytes()),
    });
    let approve = auth(
        http.post(format!("{base}/me/devices/grants/{grant_id}/approve")),
        &token,
    )
    .json(&approve_body)
    .send()
    .await
    .unwrap();
    assert!(approve.status().is_success(), "{:?}", approve.status());

    let grant_after: serde_json::Value = auth(
        http.get(format!("{base}/me/devices/grants/{grant_id}")),
        &token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(grant_after["status"], "approved");

    let devices_after: serde_json::Value = auth(http.get(format!("{base}/me/devices")), &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(devices_after.as_array().unwrap().len(), 2);
}

#[tokio::test]
#[ignore]
async fn an_approval_attempt_from_a_revoked_key_is_rejected() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, token) = seed_identity_session(&pool).await;
    let (revoked_key_id, revoked_key) = seed_signing_key(&pool, identity_id).await;
    sqlx::query("UPDATE identity_signing_keys SET revoked_at = now() WHERE id = $1")
        .bind(revoked_key_id)
        .execute(&pool)
        .await
        .unwrap();

    let new_device_signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
    let new_device_public_key = new_device_signing_key.verifying_key().to_bytes();
    let request_body = serde_json::json!({
        "requested_signing_public_key": BASE64.encode(new_device_public_key),
        "device_label": null,
    });
    let grant: serde_json::Value = auth(http.post(format!("{base}/me/devices/grants")), &token)
        .json(&request_body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let grant_id: Uuid = grant["id"].as_str().unwrap().parse().unwrap();

    let signing_bytes =
        device_grant_approval_signing_bytes(grant_id, identity_id, &new_device_public_key);
    let signature = revoked_key.sign(&signing_bytes);
    let approve_body = serde_json::json!({
        "approver_signing_key_id": revoked_key_id,
        "signature": BASE64.encode(signature.to_bytes()),
    });
    let approve = auth(
        http.post(format!("{base}/me/devices/grants/{grant_id}/approve")),
        &token,
    )
    .json(&approve_body)
    .send()
    .await
    .unwrap();
    assert_eq!(approve.status(), reqwest::StatusCode::UNAUTHORIZED);

    let grant_after: serde_json::Value = auth(
        http.get(format!("{base}/me/devices/grants/{grant_id}")),
        &token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(grant_after["status"], "pending");
}

#[tokio::test]
#[ignore]
async fn revoking_one_device_does_not_affect_another() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, token) = seed_identity_session(&pool).await;
    let (key_a_id, _key_a) = seed_signing_key(&pool, identity_id).await;
    let (key_b_id, _key_b) = seed_signing_key(&pool, identity_id).await;

    let revoke = auth(
        http.post(format!("{base}/me/devices/{key_a_id}/revoke")),
        &token,
    )
    .send()
    .await
    .unwrap();
    assert!(revoke.status().is_success(), "{:?}", revoke.status());

    let devices: serde_json::Value = auth(http.get(format!("{base}/me/devices")), &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let devices = devices.as_array().unwrap();
    let device_a = devices
        .iter()
        .find(|d| d["id"].as_str().unwrap() == key_a_id.to_string())
        .unwrap();
    let device_b = devices
        .iter()
        .find(|d| d["id"].as_str().unwrap() == key_b_id.to_string())
        .unwrap();
    assert!(!device_a["revoked_at"].is_null());
    assert!(device_b["revoked_at"].is_null());

    // Revoking again should be a no-op failure, not a second success — the
    // row is already revoked.
    let second_revoke = auth(
        http.post(format!("{base}/me/devices/{key_a_id}/revoke")),
        &token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(second_revoke.status(), reqwest::StatusCode::NOT_FOUND);
}
