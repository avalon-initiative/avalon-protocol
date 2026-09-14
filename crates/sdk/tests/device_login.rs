//! Exercises `AvalonClient::login()`/`DeviceLogin::wait()` (issue #398)
//! against a real, running `avalon-server` (and therefore a real
//! Postgres). Gated on `--ignored` since it needs live infra — see
//! `make test-live` / `make start`.
//!
//! The approving side (`POST /auth/device/approve`/`deny`) is exercised
//! directly over HTTP against a seeded identity/session, the same approach
//! `crates/server/tests/device_pairing.rs` uses — this test isn't about
//! the approver's own login ceremony, only about whether the SDK's waiting
//! side correctly turns an approval/denial into a `Session`/`SdkError`.

use avalon_sdk::{AvalonClient, AvalonConfig, SdkError};
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

/// Seeds an identity + already-authenticated session directly via SQL,
/// standing in for a real WebAuthn login on the approving device (the
/// ceremony itself is exercised in `crates/sdk/tests/authenticate.rs`, not
/// duplicated here).
async fn seed_identity_session(pool: &PgPool, display_name: &str) -> (Uuid, String) {
    let identity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(identity_id)
        .execute(pool)
        .await
        .expect("failed to seed identity");
    sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(display_name)
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

fn client(base: &str) -> AvalonClient {
    AvalonClient::new(AvalonConfig {
        server_url: base.to_string(),
        integrator_credential_key_id: "sdk-test".to_string(),
        integrator_slug: None,
        signing_key: None,
    })
}

#[tokio::test]
#[ignore]
async fn wait_resolves_to_a_real_session_once_approved() {
    let base = server_url();
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let display_name = format!("device-login-test-{}", Uuid::new_v4());
    let (_approver_id, approver_token) = seed_identity_session(&pool, &display_name).await;

    let client = client(&base);
    let pairing = client
        .login()
        .await
        .expect("login() should start a pairing");
    assert!(!pairing.user_code.is_empty());
    assert!(pairing.verification_uri.contains(&pairing.user_code));
    assert!(pairing.expires_in > 0);

    let user_code = pairing.user_code.clone();
    let base_for_approval = base.clone();
    // Approves shortly after the wait loop has had a chance to issue its
    // first poll, simulating a user completing approval on another device
    // (the Hub) while this one is already waiting.
    let approval = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let response = http
            .post(format!("{base_for_approval}/auth/device/approve"))
            .bearer_auth(&approver_token)
            .json(&serde_json::json!({ "user_code": user_code }))
            .send()
            .await
            .expect("approve request failed");
        assert!(response.status().is_success(), "{:?}", response.status());
    });

    let session = pairing.wait().await.expect("wait() should resolve");
    approval.await.expect("approval task panicked");

    assert_eq!(session.profile().display_name, display_name);
}

#[tokio::test]
#[ignore]
async fn wait_returns_a_typed_error_when_denied() {
    let base = server_url();
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let (_approver_id, approver_token) =
        seed_identity_session(&pool, &format!("device-login-deny-test-{}", Uuid::new_v4())).await;

    let client = client(&base);
    let pairing = client
        .login()
        .await
        .expect("login() should start a pairing");

    let user_code = pairing.user_code.clone();
    let base_for_denial = base.clone();
    let denial = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let response = http
            .post(format!("{base_for_denial}/auth/device/deny"))
            .bearer_auth(&approver_token)
            .json(&serde_json::json!({ "user_code": user_code }))
            .send()
            .await
            .expect("deny request failed");
        assert!(response.status().is_success(), "{:?}", response.status());
    });

    let result = pairing.wait().await;
    denial.await.expect("denial task panicked");

    assert!(matches!(result, Err(SdkError::DeviceLoginDenied)));
}
