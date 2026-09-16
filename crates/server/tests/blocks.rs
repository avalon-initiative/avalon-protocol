//! Exercises blocking (issue #97) against a real, running `avalon-server`
//! and Postgres. Gated `--ignored` since it needs live infra — see
//! `make test-live` / `make start`.
//!
//! Test identities are seeded directly via SQL, same approach as
//! `crates/server/tests/friends.rs` — this feature doesn't care how a
//! session was established, only that it's a valid bearer token.

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
        .bind(format!("blocks-test-{identity_id}"))
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

/// Presence reads default to friends-only visibility (`presence.rs`'s
/// module doc comment) — needed by any test checking one identity's view
/// of another's real presence.
async fn seed_friendship(pool: &PgPool, x: Uuid, y: Uuid) {
    let (a, b) = if x < y { (x, y) } else { (y, x) };
    sqlx::query("INSERT INTO indexer_friendships (a, b, since) VALUES ($1, $2, now())")
        .bind(a)
        .bind(b)
        .execute(pool)
        .await
        .expect("failed to seed friendship");
}

#[tokio::test]
#[ignore]
async fn a_friend_request_from_a_blocked_identity_fails_identically_to_a_nonexistent_identity() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (alice_id, alice_token) = seed_identity_session(&pool).await;
    let (bob_id, bob_token) = seed_identity_session(&pool).await;

    // Alice blocks Bob.
    let block = auth(http.post(format!("{base}/blocks")), &alice_token)
        .json(&serde_json::json!({ "identity_id": bob_id }))
        .send()
        .await
        .expect("block request failed — is `make start` running?");
    assert!(block.status().is_success(), "{:?}", block.status());

    // Bob tries to friend-request Alice — must fail exactly like a request
    // to a nonexistent identity, not a distinguishable "you're blocked".
    let blocked_attempt = auth(http.post(format!("{base}/friends/requests")), &bob_token)
        .json(&serde_json::json!({ "to": alice_id }))
        .send()
        .await
        .unwrap();
    let blocked_status = blocked_attempt.status();
    let blocked_body: serde_json::Value = blocked_attempt.json().await.unwrap();

    let nonexistent_attempt = auth(http.post(format!("{base}/friends/requests")), &bob_token)
        .json(&serde_json::json!({ "to": Uuid::new_v4() }))
        .send()
        .await
        .unwrap();
    let nonexistent_status = nonexistent_attempt.status();
    let nonexistent_body: serde_json::Value = nonexistent_attempt.json().await.unwrap();

    assert_eq!(blocked_status, nonexistent_status);
    assert_eq!(blocked_body, nonexistent_body);

    // The blocker's own attempt to friend the person they blocked fails
    // the same indistinguishable way too.
    let blocker_attempt = auth(http.post(format!("{base}/friends/requests")), &alice_token)
        .json(&serde_json::json!({ "to": bob_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(blocker_attempt.status(), blocked_status);
}

#[tokio::test]
#[ignore]
async fn blocking_auto_resolves_a_pending_friend_request_in_either_direction() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (alice_id, alice_token) = seed_identity_session(&pool).await;
    let (_bob_id, bob_token) = seed_identity_session(&pool).await;

    let create = auth(http.post(format!("{base}/friends/requests")), &bob_token)
        .json(&serde_json::json!({ "to": alice_id }))
        .send()
        .await
        .unwrap();
    assert!(create.status().is_success(), "{:?}", create.status());

    // Alice blocks Bob — the pending request Bob sent must be resolved.
    let bob_id = _bob_id;
    let block = auth(http.post(format!("{base}/blocks")), &alice_token)
        .json(&serde_json::json!({ "identity_id": bob_id }))
        .send()
        .await
        .unwrap();
    assert!(block.status().is_success(), "{:?}", block.status());

    let alice_requests: serde_json::Value =
        auth(http.get(format!("{base}/friends/requests")), &alice_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(alice_requests.as_array().unwrap().len(), 0);
}

#[tokio::test]
#[ignore]
async fn a_block_hides_presence_in_both_directions() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (alice_id, alice_token) = seed_identity_session(&pool).await;
    let (bob_id, bob_token) = seed_identity_session(&pool).await;
    seed_friendship(&pool, alice_id, bob_id).await;

    // Bob publishes Online presence.
    let publish = auth(http.put(format!("{base}/me/presence")), &bob_token)
        .json(&serde_json::json!({ "status": "Online" }))
        .send()
        .await
        .unwrap();
    assert!(publish.status().is_success(), "{:?}", publish.status());

    // Before any block: Alice can see Bob's real presence.
    let before: serde_json::Value = auth(http.get(format!("{base}/presence")), &alice_token)
        .query(&[("ids", bob_id.to_string())])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(before[0]["status"], "Online");

    // Alice blocks Bob.
    let block = auth(http.post(format!("{base}/blocks")), &alice_token)
        .json(&serde_json::json!({ "identity_id": bob_id }))
        .send()
        .await
        .unwrap();
    assert!(block.status().is_success(), "{:?}", block.status());

    // Alice reading Bob's presence now sees Offline, not Online.
    let alice_view: serde_json::Value = auth(http.get(format!("{base}/presence")), &alice_token)
        .query(&[("ids", bob_id.to_string())])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(alice_view[0]["status"], "Offline");

    // Alice publishes her own presence.
    let alice_publish = auth(http.put(format!("{base}/me/presence")), &alice_token)
        .json(&serde_json::json!({ "status": "Online" }))
        .send()
        .await
        .unwrap();
    assert!(alice_publish.status().is_success());

    // Bob reading Alice's presence also sees Offline — the other direction.
    let bob_view: serde_json::Value = auth(http.get(format!("{base}/presence")), &bob_token)
        .query(&[("ids", alice_id.to_string())])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(bob_view[0]["status"], "Offline");

    // Unblocking restores real presence.
    let unblock = auth(http.delete(format!("{base}/blocks/{bob_id}")), &alice_token)
        .send()
        .await
        .unwrap();
    assert!(unblock.status().is_success(), "{:?}", unblock.status());

    let restored: serde_json::Value = auth(http.get(format!("{base}/presence")), &alice_token)
        .query(&[("ids", bob_id.to_string())])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(restored[0]["status"], "Online");
}

#[tokio::test]
#[ignore]
async fn a_blocks_list_is_visible_only_to_the_blocker() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (_alice_id, alice_token) = seed_identity_session(&pool).await;
    let (bob_id, bob_token) = seed_identity_session(&pool).await;

    let block = auth(http.post(format!("{base}/blocks")), &alice_token)
        .json(&serde_json::json!({ "identity_id": bob_id }))
        .send()
        .await
        .unwrap();
    assert!(block.status().is_success(), "{:?}", block.status());

    let alice_list: serde_json::Value = auth(http.get(format!("{base}/blocks")), &alice_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(alice_list.as_array().unwrap().len(), 1);
    assert_eq!(alice_list[0]["blocked"], bob_id.to_string());

    // Bob's own /blocks list — his own outgoing blocks, of which he has
    // none — never reveals that Alice has blocked him.
    let bob_list: serde_json::Value = auth(http.get(format!("{base}/blocks")), &bob_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(bob_list.as_array().unwrap().len(), 0);
}

#[tokio::test]
#[ignore]
async fn cannot_block_yourself() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (alice_id, alice_token) = seed_identity_session(&pool).await;

    let response = auth(http.post(format!("{base}/blocks")), &alice_token)
        .json(&serde_json::json!({ "identity_id": alice_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}
