//! Exercises the friend request/accept/remove flow (issue #15) against a
//! real, running `avalon-server` and Postgres. Gated `--ignored` since it
//! needs live infra — see `make test-live` / `make start`.
//!
//! Test identities are seeded directly via SQL rather than through a real
//! WebAuthn ceremony (see `crates/sdk/tests/authenticate.rs` for that flow)
//! — the friends feature doesn't care how a session was established, only
//! that it's a valid bearer token, so seeding `identities`/`sessions` rows
//! directly keeps this test focused on what it's actually verifying.

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

/// Seeds a bare identity + session, bypassing WebAuthn entirely, and
/// returns the session's bearer token.
async fn seed_identity_session(pool: &PgPool) -> (Uuid, String) {
    let identity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(identity_id)
        .execute(pool)
        .await
        .expect("failed to seed identity");
    sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(format!("friends-test-{identity_id}"))
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
async fn request_then_accept_creates_exactly_one_friendship() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (alice_id, alice_token) = seed_identity_session(&pool).await;
    let (bob_id, bob_token) = seed_identity_session(&pool).await;

    let create = auth(http.post(format!("{base}/friends/requests")), &alice_token)
        .json(&serde_json::json!({ "to": bob_id }))
        .send()
        .await
        .expect("create friend request failed — is `make start` running?");
    assert!(create.status().is_success(), "{:?}", create.status());
    let request_body: serde_json::Value = create.json().await.unwrap();
    let request_id = request_body["id"].as_str().unwrap();

    let accept = auth(
        http.post(format!("{base}/friends/requests/{request_id}/accept")),
        &bob_token,
    )
    .send()
    .await
    .expect("accept friend request failed");
    assert!(accept.status().is_success(), "{:?}", accept.status());

    let alice_friends: serde_json::Value = auth(http.get(format!("{base}/friends")), &alice_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bob_friends: serde_json::Value = auth(http.get(format!("{base}/friends")), &bob_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(alice_friends.as_array().unwrap().len(), 1);
    assert_eq!(bob_friends.as_array().unwrap().len(), 1);

    let _ = alice_id;
}

#[tokio::test]
#[ignore]
async fn declining_a_request_leaves_no_friendship() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (_alice_id, alice_token) = seed_identity_session(&pool).await;
    let (bob_id, bob_token) = seed_identity_session(&pool).await;

    let create = auth(http.post(format!("{base}/friends/requests")), &alice_token)
        .json(&serde_json::json!({ "to": bob_id }))
        .send()
        .await
        .unwrap();
    let request_body: serde_json::Value = create.json().await.unwrap();
    let request_id = request_body["id"].as_str().unwrap();

    let decline = auth(
        http.delete(format!("{base}/friends/requests/{request_id}")),
        &bob_token,
    )
    .send()
    .await
    .unwrap();
    assert!(decline.status().is_success(), "{:?}", decline.status());

    let alice_friends: serde_json::Value = auth(http.get(format!("{base}/friends")), &alice_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(alice_friends.as_array().unwrap().len(), 0);
}

#[tokio::test]
#[ignore]
async fn removing_a_friendship_is_visible_to_both_parties() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (alice_id, alice_token) = seed_identity_session(&pool).await;
    let (bob_id, bob_token) = seed_identity_session(&pool).await;

    let create = auth(http.post(format!("{base}/friends/requests")), &alice_token)
        .json(&serde_json::json!({ "to": bob_id }))
        .send()
        .await
        .unwrap();
    let request_body: serde_json::Value = create.json().await.unwrap();
    let request_id = request_body["id"].as_str().unwrap();
    auth(
        http.post(format!("{base}/friends/requests/{request_id}/accept")),
        &bob_token,
    )
    .send()
    .await
    .unwrap();

    let remove = auth(
        http.delete(format!("{base}/friends/{alice_id}")),
        &bob_token,
    )
    .send()
    .await
    .unwrap();
    assert!(remove.status().is_success(), "{:?}", remove.status());

    let alice_friends: serde_json::Value = auth(http.get(format!("{base}/friends")), &alice_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bob_friends: serde_json::Value = auth(http.get(format!("{base}/friends")), &bob_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(alice_friends.as_array().unwrap().len(), 0);
    assert_eq!(bob_friends.as_array().unwrap().len(), 0);
}
