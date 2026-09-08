//! Exercises `Session::friends`/`presence`/`presence_of`/`update_presence`
//! (issue #17) against a real, running `avalon-server`. Gated `--ignored`
//! since it needs live infra — see `make test-live` / `make start`.
//!
//! `authenticate()` always returns a `Session` with no grants (the
//! capability-grant system, #26–#28, isn't built yet), so these tests use
//! `Session::grant_for_testing` to exercise the capability-gated methods
//! against a real server — see that method's doc comment.
//!
//! Friend/presence fixtures are seeded directly via SQL, same approach as
//! `crates/server/tests/friends.rs` and `crates/server/tests/presence.rs` —
//! this test cares about the SDK's HTTP/deserialization layer, not how a
//! session or friendship came to exist.

use avalon_protocol::social::PresenceStatus;
use avalon_sdk::{AvalonClient, AvalonConfig};
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

/// Seeds a bare identity + session, bypassing WebAuthn entirely — same
/// approach `crates/server/tests/friends.rs` uses, since this test doesn't
/// care how a session was established.
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

fn client() -> AvalonClient {
    AvalonClient::new(AvalonConfig {
        server_url: server_url(),
        game_credential_key_id: "sdk-test".to_string(),
    })
}

#[tokio::test]
#[ignore]
async fn friends_returns_a_friendship_created_via_the_http_api() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let client = client();

    let (alice_id, alice_token) =
        seed_identity_session(&pool, &format!("sdk-friends-alice-{}", Uuid::new_v4())).await;
    let (bob_id, bob_token) =
        seed_identity_session(&pool, &format!("sdk-friends-bob-{}", Uuid::new_v4())).await;

    let create = http
        .post(format!("{base}/friends/requests"))
        .bearer_auth(&alice_token)
        .json(&serde_json::json!({ "to": bob_id }))
        .send()
        .await
        .expect("create friend request failed — is `make start` running?");
    assert!(create.status().is_success());
    let request_body: serde_json::Value = create.json().await.unwrap();
    let request_id = request_body["id"].as_str().unwrap();

    let accept = http
        .post(format!("{base}/friends/requests/{request_id}/accept"))
        .bearer_auth(&bob_token)
        .send()
        .await
        .expect("accept friend request failed");
    assert!(accept.status().is_success());

    let session = client
        .authenticate(&alice_token)
        .await
        .expect("authenticate should succeed")
        .grant_for_testing("friends.read");

    let friends = session.friends().await.expect("friends() should succeed");
    assert_eq!(friends.len(), 1);
    assert_eq!(friends[0].identity_id.0, bob_id);
    // presence.read wasn't granted, so no presence should be embedded even
    // though Bob has none published anyway.
    assert!(friends[0].presence.is_none());

    let _ = alice_id;
}

#[tokio::test]
#[ignore]
async fn friends_without_presence_read_never_embeds_presence() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let client = client();

    let (_alice_id, alice_token) =
        seed_identity_session(&pool, &format!("sdk-nopresence-alice-{}", Uuid::new_v4())).await;
    let (bob_id, bob_token) =
        seed_identity_session(&pool, &format!("sdk-nopresence-bob-{}", Uuid::new_v4())).await;

    let create = http
        .post(format!("{base}/friends/requests"))
        .bearer_auth(&alice_token)
        .json(&serde_json::json!({ "to": bob_id }))
        .send()
        .await
        .unwrap();
    let request_body: serde_json::Value = create.json().await.unwrap();
    let request_id = request_body["id"].as_str().unwrap();
    http.post(format!("{base}/friends/requests/{request_id}/accept"))
        .bearer_auth(&bob_token)
        .send()
        .await
        .unwrap();

    // Bob publishes presence, but Alice's session below is only granted
    // friends.read, not presence.read.
    http.put(format!("{base}/me/presence"))
        .bearer_auth(&bob_token)
        .json(&serde_json::json!({ "status": "Online" }))
        .send()
        .await
        .unwrap();

    let session = client
        .authenticate(&alice_token)
        .await
        .unwrap()
        .grant_for_testing("friends.read");

    let friends = session.friends().await.expect("friends() should succeed");
    assert_eq!(friends.len(), 1);
    assert!(
        friends[0].presence.is_none(),
        "presence must stay None without presence.read, even though the friend has published it"
    );
}

#[tokio::test]
#[ignore]
async fn update_presence_then_presence_reflects_it() {
    let pool = test_pool().await;
    let (identity_id, token) =
        seed_identity_session(&pool, &format!("sdk-presence-{}", Uuid::new_v4())).await;
    let client = client();

    let session = client
        .authenticate(&token)
        .await
        .unwrap()
        .grant_for_testing("presence.read");

    session
        .update_presence(PresenceStatus::Away)
        .await
        .expect("update_presence should succeed");

    let mine = session.presence().await.expect("presence() should succeed");
    assert_eq!(mine.identity_id.0, identity_id);
    assert_eq!(mine.status, PresenceStatus::Away);
}

#[tokio::test]
#[ignore]
async fn presence_of_reflects_multiple_published_statuses() {
    let pool = test_pool().await;
    let (alice_id, alice_token) =
        seed_identity_session(&pool, &format!("sdk-multi-alice-{}", Uuid::new_v4())).await;
    let (bob_id, bob_token) =
        seed_identity_session(&pool, &format!("sdk-multi-bob-{}", Uuid::new_v4())).await;
    let client = client();

    let alice_session = client
        .authenticate(&alice_token)
        .await
        .unwrap()
        .grant_for_testing("presence.read");
    let bob_session = client
        .authenticate(&bob_token)
        .await
        .unwrap()
        .grant_for_testing("presence.read");

    alice_session
        .update_presence(PresenceStatus::Online)
        .await
        .unwrap();
    bob_session
        .update_presence(PresenceStatus::Away)
        .await
        .unwrap();

    let both = alice_session
        .presence_of(&[
            avalon_protocol::ids::IdentityId(alice_id),
            avalon_protocol::ids::IdentityId(bob_id),
        ])
        .await
        .expect("presence_of should succeed");
    assert_eq!(both.len(), 2);
    let alice_view = both.iter().find(|p| p.identity_id.0 == alice_id).unwrap();
    let bob_view = both.iter().find(|p| p.identity_id.0 == bob_id).unwrap();
    assert_eq!(alice_view.status, PresenceStatus::Online);
    assert_eq!(bob_view.status, PresenceStatus::Away);
}
