//! Exercises cross-device pairing (issue #307) against a real, running
//! `avalon-server` and Postgres. Gated `--ignored` since it needs live
//! infra — see `make test-live` / `make start`.
//!
//! Test identities/sessions are seeded directly via SQL, same approach as
//! `crates/server/tests/device_grants.rs`.

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

    let approve = auth(
        http.post(format!("{base}/auth/device/approve")),
        &approver_token,
    )
    .json(&serde_json::json!({ "user_code": user_code }))
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
