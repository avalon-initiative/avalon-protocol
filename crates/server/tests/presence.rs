//! Exercises the presence publish/read flow (issue #16) against a real,
//! running `avalon-server`. Gated `--ignored` since it needs live infra —
//! see `make test-live` / `make start`.
//!
//! Expiry is exercised by starting the server with a short
//! `AVALON_PRESENCE_TTL_SECS` rather than sleeping the real 120s default —
//! set it before `make start` when running this file specifically, e.g.
//! `AVALON_PRESENCE_TTL_SECS=1 make start`. Without it, the expiry test
//! below is skipped rather than sleeping two real minutes.
//!
//! Test identities are seeded directly via SQL rather than through a real
//! WebAuthn ceremony — same approach as `crates/server/tests/friends.rs`,
//! since presence doesn't care how a session was established.

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
        .bind(format!("presence-test-{identity_id}"))
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
async fn publishing_presence_is_visible_via_get() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (alice_id, alice_token) = seed_identity_session(&pool).await;

    let publish = auth(http.put(format!("{base}/me/presence")), &alice_token)
        .json(&serde_json::json!({ "status": "Online" }))
        .send()
        .await
        .expect("publish presence failed — is `make start` running?");
    assert!(publish.status().is_success(), "{:?}", publish.status());

    let read: serde_json::Value = auth(
        http.get(format!("{base}/presence?ids={alice_id}")),
        &alice_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let entries = read.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["status"], "Online");
    assert_eq!(entries[0]["playing"], serde_json::Value::Null);
}

#[tokio::test]
#[ignore]
async fn a_player_cannot_claim_to_be_playing_a_game() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (alice_id, alice_token) = seed_identity_session(&pool).await;

    // The request body has no `playing` field at all in the wire format —
    // this is enforced by `UpdatePresenceRequest` only ever deserializing
    // `status`, not by rejecting an extra field, so this just confirms the
    // response never echoes a `playing` value regardless of what's sent.
    auth(http.put(format!("{base}/me/presence")), &alice_token)
        .json(&serde_json::json!({ "status": "Online", "playing": Uuid::new_v4() }))
        .send()
        .await
        .unwrap();

    let read: serde_json::Value = auth(
        http.get(format!("{base}/presence?ids={alice_id}")),
        &alice_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(read[0]["playing"], serde_json::Value::Null);
}

#[tokio::test]
#[ignore]
async fn missing_presence_reads_as_offline() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (_alice_id, alice_token) = seed_identity_session(&pool).await;
    let never_published = Uuid::new_v4();

    let read: serde_json::Value = auth(
        http.get(format!("{base}/presence?ids={never_published}")),
        &alice_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(read[0]["status"], "Offline");
}

#[tokio::test]
#[ignore]
async fn stale_presence_expires_to_offline() {
    let Ok(ttl) = std::env::var("AVALON_PRESENCE_TTL_SECS") else {
        eprintln!(
            "skipping: set AVALON_PRESENCE_TTL_SECS (e.g. 1) before `make start` to run this test"
        );
        return;
    };
    let ttl_secs: u64 = ttl
        .parse()
        .expect("AVALON_PRESENCE_TTL_SECS must be a number");

    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (alice_id, alice_token) = seed_identity_session(&pool).await;

    auth(http.put(format!("{base}/me/presence")), &alice_token)
        .json(&serde_json::json!({ "status": "Online" }))
        .send()
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_secs(ttl_secs + 1)).await;

    let read: serde_json::Value = auth(
        http.get(format!("{base}/presence?ids={alice_id}")),
        &alice_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(read[0]["status"], "Offline");
}
