//! Exercises `GET /identities/profiles` (issue #161) against a real,
//! running `avalon-server`. Gated `--ignored` since it needs live infra —
//! see `make test-live` / `make start`. Test identities are seeded directly
//! via SQL rather than through a real WebAuthn ceremony — same approach as
//! `crates/server/tests/presence.rs`, since this endpoint doesn't care how
//! a session was established.

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

fn auth(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request.bearer_auth(token)
}

#[tokio::test]
#[ignore]
async fn resolves_display_names_for_a_batch_of_ids() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (alice_id, alice_token) = seed_identity_session(&pool, "alice-profiles-test").await;
    let (bob_id, _bob_token) = seed_identity_session(&pool, "bob-profiles-test").await;

    let response: serde_json::Value = auth(
        http.get(format!(
            "{base}/identities/profiles?ids={alice_id},{bob_id}"
        )),
        &alice_token,
    )
    .send()
    .await
    .expect("request failed — is `make start` running?")
    .json()
    .await
    .unwrap();

    let entries = response.as_array().unwrap();
    assert_eq!(entries.len(), 2);
    let names: Vec<&str> = entries
        .iter()
        .map(|e| e["display_name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"alice-profiles-test"));
    assert!(names.contains(&"bob-profiles-test"));
}

#[tokio::test]
#[ignore]
async fn unknown_ids_are_omitted_not_errors() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (alice_id, alice_token) = seed_identity_session(&pool, "alice-unknown-id-test").await;
    let missing_id = Uuid::new_v4();

    let response = auth(
        http.get(format!(
            "{base}/identities/profiles?ids={alice_id},{missing_id}"
        )),
        &alice_token,
    )
    .send()
    .await
    .unwrap();
    assert!(response.status().is_success());

    let entries: Vec<serde_json::Value> = response.json().await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["identity_id"], alice_id.to_string());
}

#[tokio::test]
#[ignore]
async fn requires_a_session() {
    let http = reqwest::Client::new();
    let base = server_url();
    let some_id = Uuid::new_v4();

    let response = http
        .get(format!("{base}/identities/profiles?ids={some_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}
