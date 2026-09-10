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

/// Seeds an accepted friendship directly into `friendships`, which requires
/// `a < b` — ordered here rather than trusting caller order.
async fn seed_friendship(pool: &PgPool, x: Uuid, y: Uuid) {
    let (a, b) = if x < y { (x, y) } else { (y, x) };
    sqlx::query("INSERT INTO friendships (a, b) VALUES ($1, $2)")
        .bind(a)
        .bind(b)
        .execute(pool)
        .await
        .expect("failed to seed friendship");
}

/// Opts `identity_id` into global discoverability (issue #205).
async fn seed_discoverable(pool: &PgPool, identity_id: Uuid) {
    sqlx::query("INSERT INTO discovery_preferences (identity_id, discoverable) VALUES ($1, true)")
        .bind(identity_id)
        .execute(pool)
        .await
        .expect("failed to seed discovery preference");
}

/// Creates a guild owned by `owner_token`'s identity and returns its id.
async fn create_guild(client: &reqwest::Client, owner_token: &str) -> Uuid {
    let suffix = Uuid::new_v4().simple().to_string();
    let body = serde_json::json!({
        "name": format!("Conversations Test Guild {}", &suffix[..8]),
        "tag": suffix[..4].to_uppercase(),
        "description": "a guild created by a conversations integration test",
    });
    let created: serde_json::Value = auth(
        client.post(format!("{}/guilds", server_url())),
        owner_token,
    )
    .json(&body)
    .send()
    .await
    .expect("create guild request failed")
    .json()
    .await
    .expect("expected JSON guild body");
    Uuid::parse_str(created["id"].as_str().expect("expected guild id")).expect("guild id is a UUID")
}

/// Seeds a `guild_members` row directly at the `member` role (index 2).
async fn seed_guild_membership(pool: &PgPool, guild_id: Uuid, identity_id: Uuid) {
    sqlx::query(
        "INSERT INTO guild_members (guild_id, identity_id, role_index, joined_at) \
         VALUES ($1, $2, 2, now())",
    )
    .bind(guild_id)
    .bind(identity_id)
    .execute(pool)
    .await
    .expect("failed to seed guild membership");
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
    seed_friendship(&pool, alice_id, bob_id).await;

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
    seed_friendship(&pool, alice_id, bob_id).await;

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
    let (alice_id, alice_token) = seed_identity_session(&pool).await;
    let (bob_id, _bob_token) = seed_identity_session(&pool).await;
    let (_eve_id, eve_token) = seed_identity_session(&pool).await;
    seed_friendship(&pool, alice_id, bob_id).await;

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

/// The idempotency half of issue #111's deferred submission engine: a
/// `client_entry_id` on `POST /conversations/{id}/messages` lets a retried
/// request after a dropped response avoid double-applying (migration
/// `0037_conversation_message_idempotency`). This is the live, end-to-end
/// form of that guarantee against the real endpoint and a real Postgres
/// row count — `crates/sdk/src/submission.rs`'s
/// `a_retried_submission_after_a_dropped_response_does_not_double_apply`
/// test covers the engine's own retry/dedupe-threading logic without a live
/// server; this test covers the other half, that the server itself actually
/// prevents the duplicate.
#[tokio::test]
#[ignore]
async fn retrying_a_send_with_the_same_client_entry_id_does_not_double_apply() {
    let pool = test_pool().await;
    let client = reqwest::Client::new();
    let (alice_id, alice_token) = seed_identity_session(&pool).await;
    let (bob_id, _bob_token) = seed_identity_session(&pool).await;
    seed_friendship(&pool, alice_id, bob_id).await;

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

    let client_entry_id = Uuid::new_v4();

    // First attempt — as if the SDK's submission engine (crates/sdk/src/
    // submission.rs) sent this and never saw the response (a dropped
    // connection, a client crash before the response was read, ...).
    let first: serde_json::Value = auth(
        client.post(format!(
            "{}/conversations/{conversation_id}/messages",
            server_url()
        )),
        &alice_token,
    )
    .json(&serde_json::json!({
        "body": "hey bob, are you there?",
        "client_entry_id": client_entry_id,
    }))
    .send()
    .await
    .expect("first send request failed")
    .json()
    .await
    .expect("expected JSON message body");

    // Retry with the exact same client_entry_id, same as the submission
    // engine would do on its next drain cycle for the still-pending entry.
    let retried: serde_json::Value = auth(
        client.post(format!(
            "{}/conversations/{conversation_id}/messages",
            server_url()
        )),
        &alice_token,
    )
    .json(&serde_json::json!({
        "body": "hey bob, are you there?",
        "client_entry_id": client_entry_id,
    }))
    .send()
    .await
    .expect("retried send request failed")
    .json()
    .await
    .expect("expected JSON message body");

    // Same message, not a new one.
    assert_eq!(first["id"], retried["id"]);

    // A third, genuinely distinct message (its own client_entry_id) must
    // still go through normally — the dedupe key is per-entry, not a
    // conversation-wide throttle.
    let distinct: serde_json::Value = auth(
        client.post(format!(
            "{}/conversations/{conversation_id}/messages",
            server_url()
        )),
        &alice_token,
    )
    .json(&serde_json::json!({
        "body": "anyway, unrelated message",
        "client_entry_id": Uuid::new_v4(),
    }))
    .send()
    .await
    .expect("distinct send request failed")
    .json()
    .await
    .expect("expected JSON message body");
    assert_ne!(distinct["id"], first["id"]);

    // The authoritative check: exactly one row for the retried
    // client_entry_id, straight from Postgres, not just the HTTP responses
    // agreeing with each other.
    let row_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM conversation_messages WHERE conversation_id = $1 AND client_entry_id = $2",
    )
    .bind(Uuid::parse_str(conversation_id).expect("conversation_id is a UUID"))
    .bind(client_entry_id)
    .fetch_one(&pool)
    .await
    .expect("row count query failed");
    assert_eq!(
        row_count, 1,
        "exactly one resulting row for the retried client_entry_id"
    );

    // Two distinct messages total in the conversation (the retried one,
    // counted once, plus the genuinely distinct one) — not three.
    let total_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM conversation_messages WHERE conversation_id = $1")
            .bind(Uuid::parse_str(conversation_id).expect("conversation_id is a UUID"))
            .fetch_one(&pool)
            .await
            .expect("total row count query failed");
    assert_eq!(total_count, 2);
}

/// Issue #269: no relationship, no conversation.
#[tokio::test]
#[ignore]
async fn unrelated_participant_cannot_create_a_conversation() {
    let pool = test_pool().await;
    let client = reqwest::Client::new();
    let (_alice_id, alice_token) = seed_identity_session(&pool).await;
    let (bob_id, _bob_token) = seed_identity_session(&pool).await;

    let response = auth(
        client.post(format!("{}/conversations", server_url())),
        &alice_token,
    )
    .json(&serde_json::json!({ "participants": [bob_id] }))
    .send()
    .await
    .expect("create conversation request failed");
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}

/// Issue #269 / #205: opting into global search discoverability never
/// substitutes for an actual relationship.
#[tokio::test]
#[ignore]
async fn discoverable_but_unrelated_participant_is_still_rejected() {
    let pool = test_pool().await;
    let client = reqwest::Client::new();
    let (_alice_id, alice_token) = seed_identity_session(&pool).await;
    let (bob_id, _bob_token) = seed_identity_session(&pool).await;
    seed_discoverable(&pool, bob_id).await;

    let response = auth(
        client.post(format!("{}/conversations", server_url())),
        &alice_token,
    )
    .json(&serde_json::json!({ "participants": [bob_id] }))
    .send()
    .await
    .expect("create conversation request failed");
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}

/// Issue #269: shared guild membership satisfies the relationship gate just
/// like friendship does, with no friend request involved.
#[tokio::test]
#[ignore]
async fn shared_guild_membership_permits_conversation_creation() {
    let pool = test_pool().await;
    let client = reqwest::Client::new();
    let (_alice_id, alice_token) = seed_identity_session(&pool).await;
    let (bob_id, _bob_token) = seed_identity_session(&pool).await;

    let guild_id = create_guild(&client, &alice_token).await;
    seed_guild_membership(&pool, guild_id, bob_id).await;

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
    assert!(created["id"].as_str().is_some());
}

/// Issue #269: a nonexistent participant id and an existing-but-unrelated
/// one must be indistinguishable — same status, same body.
#[tokio::test]
#[ignore]
async fn nonexistent_and_unrelated_participant_get_identical_rejection() {
    let pool = test_pool().await;
    let client = reqwest::Client::new();
    let (_alice_id, alice_token) = seed_identity_session(&pool).await;
    let (unrelated_id, _unrelated_token) = seed_identity_session(&pool).await;
    let nonexistent_id = Uuid::new_v4();

    let unrelated_response = auth(
        client.post(format!("{}/conversations", server_url())),
        &alice_token,
    )
    .json(&serde_json::json!({ "participants": [unrelated_id] }))
    .send()
    .await
    .expect("create conversation request failed");
    let unrelated_status = unrelated_response.status();
    let unrelated_body: serde_json::Value = unrelated_response
        .json()
        .await
        .expect("expected JSON error body");

    let nonexistent_response = auth(
        client.post(format!("{}/conversations", server_url())),
        &alice_token,
    )
    .json(&serde_json::json!({ "participants": [nonexistent_id] }))
    .send()
    .await
    .expect("create conversation request failed");
    let nonexistent_status = nonexistent_response.status();
    let nonexistent_body: serde_json::Value = nonexistent_response
        .json()
        .await
        .expect("expected JSON error body");

    assert_eq!(unrelated_status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(unrelated_status, nonexistent_status);
    assert_eq!(unrelated_body, nonexistent_body);
}
