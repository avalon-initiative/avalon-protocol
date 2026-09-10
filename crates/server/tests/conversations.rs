//! Exercises direct/small-group conversations (issue #102) against a real,
//! running `avalon-server` and Postgres. Gated `--ignored` since it needs
//! live infra — see `make test-live` / `make start`. Same "seed state via
//! SQL rather than a real ceremony/flow" pattern
//! `crates/server/tests/blocks.rs` / `crates/server/tests/guild_channels.rs`
//! already use for identities/sessions.

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
        .bind(format!("conversations-test-{identity_id}"))
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

/// Two identities create a conversation and exchange messages across two
/// independent sessions — the acceptance-criteria flow for #102's
/// endpoints end to end.
#[tokio::test]
#[ignore]
async fn two_identities_create_a_conversation_and_exchange_messages_across_sessions() {
    let pool = test_pool().await;
    let client = reqwest::Client::new();
    let (alice_id, alice_token) = seed_identity_session(&pool).await;
    let (bob_id, bob_token) = seed_identity_session(&pool).await;

    let created: serde_json::Value = auth(
        client.post(format!("{}/conversations", server_url())),
        &alice_token,
    )
    .json(&serde_json::json!({ "participants": [bob_id] }))
    .send()
    .await
    .expect("create conversation request failed")
    .json()
    .await
    .expect("expected JSON conversation body");
    let conversation_id = created["id"].as_str().expect("expected id");

    // Alice sends, Bob sends back — each from their own session.
    let sent = auth(
        client.post(format!(
            "{}/conversations/{conversation_id}/messages",
            server_url()
        )),
        &alice_token,
    )
    .json(&serde_json::json!({ "body": "hey bob" }))
    .send()
    .await
    .expect("alice's send request failed");
    assert!(sent.status().is_success());

    let reply = auth(
        client.post(format!(
            "{}/conversations/{conversation_id}/messages",
            server_url()
        )),
        &bob_token,
    )
    .json(&serde_json::json!({ "body": "hey alice" }))
    .send()
    .await
    .expect("bob's send request failed");
    assert!(reply.status().is_success());

    // Bob reads the conversation and sees both messages, newest first.
    let messages: Vec<serde_json::Value> = auth(
        client.get(format!(
            "{}/conversations/{conversation_id}/messages",
            server_url()
        )),
        &bob_token,
    )
    .send()
    .await
    .expect("list messages request failed")
    .json()
    .await
    .expect("expected JSON message list");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["body"], "hey alice");
    assert_eq!(messages[1]["body"], "hey bob");

    // Re-requesting a conversation for the same participant set returns
    // the same conversation id rather than creating a duplicate.
    let recreated: serde_json::Value = auth(
        client.post(format!("{}/conversations", server_url())),
        &bob_token,
    )
    .json(&serde_json::json!({ "participants": [alice_id] }))
    .send()
    .await
    .expect("re-create conversation request failed")
    .json()
    .await
    .expect("expected JSON conversation body");
    assert_eq!(recreated["id"], created["id"]);
}

/// A block created mid-conversation rejects the next send, silently — the
/// response carries no indication a block is the reason.
#[tokio::test]
#[ignore]
async fn a_block_created_mid_conversation_rejects_the_next_send() {
    let pool = test_pool().await;
    let client = reqwest::Client::new();
    let (alice_id, alice_token) = seed_identity_session(&pool).await;
    let (bob_id, bob_token) = seed_identity_session(&pool).await;

    let created: serde_json::Value = auth(
        client.post(format!("{}/conversations", server_url())),
        &alice_token,
    )
    .json(&serde_json::json!({ "participants": [bob_id] }))
    .send()
    .await
    .expect("create conversation request failed")
    .json()
    .await
    .expect("expected JSON conversation body");
    let conversation_id = created["id"].as_str().expect("expected id");

    // The conversation works fine before any block exists.
    let first_send = auth(
        client.post(format!(
            "{}/conversations/{conversation_id}/messages",
            server_url()
        )),
        &alice_token,
    )
    .json(&serde_json::json!({ "body": "still friendly" }))
    .send()
    .await
    .expect("first send request failed");
    assert!(first_send.status().is_success());

    // Bob blocks Alice mid-conversation.
    let block = auth(client.post(format!("{}/blocks", server_url())), &bob_token)
        .json(&serde_json::json!({ "identity_id": alice_id }))
        .send()
        .await
        .expect("block request failed");
    assert!(block.status().is_success());

    // Alice's next send is now rejected — with the same status/shape a
    // non-participant would get, not a distinguishable "blocked" error.
    let rejected = auth(
        client.post(format!(
            "{}/conversations/{conversation_id}/messages",
            server_url()
        )),
        &alice_token,
    )
    .json(&serde_json::json!({ "body": "hello?" }))
    .send()
    .await
    .expect("second send request failed");
    assert_eq!(rejected.status(), reqwest::StatusCode::FORBIDDEN);
    let body: serde_json::Value = rejected.json().await.expect("expected JSON error body");
    assert_eq!(body["error"], "not a participant in this conversation");

    // The read path is rejected identically — this is the gap an
    // independent review caught: a blocked-out participant getting a
    // successful GET followed by a rejected POST would deterministically
    // prove they'd been blocked, even with identical error codes on the
    // POST alone. Same status, same body, same endpoint-agnostic error as
    // a genuine non-participant would get.
    let read_rejected = auth(
        client.get(format!(
            "{}/conversations/{conversation_id}/messages",
            server_url()
        )),
        &alice_token,
    )
    .send()
    .await
    .expect("read-after-block request failed");
    assert_eq!(read_rejected.status(), reqwest::StatusCode::FORBIDDEN);
    let read_body: serde_json::Value = read_rejected
        .json()
        .await
        .expect("expected JSON error body");
    assert_eq!(read_body["error"], "not a participant in this conversation");

    // The conversation also drops out of Alice's own list — consistent
    // with her no longer being able to read or post to it.
    let listed: Vec<serde_json::Value> = auth(
        client.get(format!("{}/conversations", server_url())),
        &alice_token,
    )
    .send()
    .await
    .expect("list conversations request failed")
    .json()
    .await
    .expect("expected JSON conversation list");
    assert!(
        listed.iter().all(|c| c["id"] != conversation_id),
        "blocked-out conversation should not appear in the caller's list"
    );
}

/// A non-participant cannot read or post to a conversation they're not
/// part of.
#[tokio::test]
#[ignore]
async fn a_non_participant_cannot_read_or_post() {
    let pool = test_pool().await;
    let client = reqwest::Client::new();
    let (_alice_id, alice_token) = seed_identity_session(&pool).await;
    let (bob_id, _bob_token) = seed_identity_session(&pool).await;
    let (_eve_id, eve_token) = seed_identity_session(&pool).await;

    let created: serde_json::Value = auth(
        client.post(format!("{}/conversations", server_url())),
        &alice_token,
    )
    .json(&serde_json::json!({ "participants": [bob_id] }))
    .send()
    .await
    .expect("create conversation request failed")
    .json()
    .await
    .expect("expected JSON conversation body");
    let conversation_id = created["id"].as_str().expect("expected id");

    let read_attempt = auth(
        client.get(format!(
            "{}/conversations/{conversation_id}/messages",
            server_url()
        )),
        &eve_token,
    )
    .send()
    .await
    .expect("read request failed");
    assert_eq!(read_attempt.status(), reqwest::StatusCode::FORBIDDEN);

    let post_attempt = auth(
        client.post(format!(
            "{}/conversations/{conversation_id}/messages",
            server_url()
        )),
        &eve_token,
    )
    .json(&serde_json::json!({ "body": "sneaking in" }))
    .send()
    .await
    .expect("post request failed");
    assert_eq!(post_attempt.status(), reqwest::StatusCode::FORBIDDEN);
}
