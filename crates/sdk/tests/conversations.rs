//! Exercises `Session::conversations`/`.dm()`/`.conversation(id).messages()`/
//! `.send()` (issue #104) against a real, running `avalon-server`'s #102
//! conversation endpoints. Gated `--ignored`, same as `tests/social.rs` and
//! `tests/guilds.rs` — needs live infra (`make test-live` / `make start`).
//!
//! `authenticate()` always returns a `Session` with no grants (the
//! capability-grant system, #26–#28, isn't built yet), so these tests use
//! `Session::grant_for_testing` to exercise the capability-gated methods
//! against a real server — see that method's doc comment.

use avalon_protocol::ids::IdentityId;
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
/// approach `crates/sdk/tests/social.rs`/`tests/guilds.rs` use.
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

/// Issue #269: conversation creation now requires an existing relationship
/// between every participant, so these tests need one seeded first. Writes
/// `indexer_friendships` directly, not the old `friendships` table — that
/// table has been dead since issue #506 retargeted
/// `crates/server/src/friends.rs` to read the indexer projection instead,
/// and a row seeded there is invisible to `create_conversation`'s
/// relationship check now.
async fn seed_friendship(pool: &PgPool, x: Uuid, y: Uuid) {
    let (a, b) = if x < y { (x, y) } else { (y, x) };
    sqlx::query("INSERT INTO indexer_friendships (a, b, since) VALUES ($1, $2, now())")
        .bind(a)
        .bind(b)
        .execute(pool)
        .await
        .expect("failed to seed friendship");
}

fn client() -> AvalonClient {
    AvalonClient::new(AvalonConfig {
        server_url: server_url(),
        integrator_credential_key_id: "sdk-test".to_string(),
        integrator_slug: None,
        signing_key: None,
        retry: Default::default(),
    })
}

#[tokio::test]
#[ignore]
async fn dm_then_send_then_messages_round_trips_across_two_sessions() {
    let pool = test_pool().await;
    let client = client();

    let (alice_id, alice_token) =
        seed_identity_session(&pool, &format!("sdk-conv-alice-{}", Uuid::new_v4())).await;
    let (bob_id, bob_token) =
        seed_identity_session(&pool, &format!("sdk-conv-bob-{}", Uuid::new_v4())).await;
    seed_friendship(&pool, alice_id, bob_id).await;

    let alice = client
        .authenticate(&alice_token)
        .await
        .expect("authenticate should succeed")
        .grant_for_testing("messages.send")
        .grant_for_testing("messages.read");

    let conversation = alice
        .dm(IdentityId(bob_id))
        .await
        .expect("dm() should succeed");

    let sent = conversation
        .send("hello from alice")
        .await
        .expect("send() should succeed");
    assert_eq!(sent.author.0, alice_id);
    assert_eq!(sent.body, "hello from alice");

    let alice_messages = conversation
        .messages(None, None)
        .await
        .expect("messages() should succeed");
    assert_eq!(alice_messages.len(), 1);
    assert_eq!(alice_messages[0].id, sent.id);

    // Bob reads and replies through the same conversation id, discovered
    // via his own `conversations()` list rather than a second `dm()` call —
    // exercising `Session::conversation(id)` as the read-only entry point.
    let bob = client
        .authenticate(&bob_token)
        .await
        .expect("authenticate should succeed")
        .grant_for_testing("messages.read")
        .grant_for_testing("messages.send");

    let bob_conversations = bob
        .conversations()
        .await
        .expect("conversations() should succeed");
    let found = bob_conversations
        .iter()
        .find(|c| c.id == conversation.conversation_id())
        .expect("bob should see the conversation alice started");
    assert!(found.participants.contains(&IdentityId(alice_id)));
    assert!(found.participants.contains(&IdentityId(bob_id)));

    let bob_handle = bob.conversation(found.id);
    let bob_reply = bob_handle
        .send("hi alice")
        .await
        .expect("bob's send() should succeed");
    assert_eq!(bob_reply.author.0, bob_id);

    let bob_messages = bob_handle
        .messages(None, None)
        .await
        .expect("bob's messages() should succeed");
    assert_eq!(bob_messages.len(), 2);
}

/// Issue #438: a conversation subscription receives a new message pushed
/// by the other participant's send, without polling — same shape
/// `tests/guilds.rs`'s `subscribe_messages_receives_a_message_pushed_by_another_member`
/// exercises for guild chat.
#[tokio::test]
#[ignore]
async fn subscribe_messages_receives_a_message_pushed_by_the_other_participant() {
    let pool = test_pool().await;
    let client = client();

    let (alice_id, alice_token) =
        seed_identity_session(&pool, &format!("sdk-conv-ws-alice-{}", Uuid::new_v4())).await;
    let (bob_id, bob_token) =
        seed_identity_session(&pool, &format!("sdk-conv-ws-bob-{}", Uuid::new_v4())).await;
    seed_friendship(&pool, alice_id, bob_id).await;

    let alice = client
        .authenticate(&alice_token)
        .await
        .unwrap()
        .grant_for_testing("messages.send")
        .grant_for_testing("messages.read");
    let bob = client
        .authenticate(&bob_token)
        .await
        .unwrap()
        .grant_for_testing("messages.send")
        .grant_for_testing("messages.read");

    let conversation = alice
        .dm(IdentityId(bob_id))
        .await
        .expect("dm() should succeed");

    let mut updates = conversation
        .subscribe_messages()
        .await
        .expect("subscribe_messages should connect");

    let bob_handle = bob.conversation(conversation.conversation_id());
    bob_handle
        .send("hello from bob")
        .await
        .expect("bob's send should succeed");

    let pushed = tokio::time::timeout(std::time::Duration::from_secs(5), updates.recv())
        .await
        .expect("a pushed message should arrive without polling")
        .expect("channel should still be open");
    assert_eq!(pushed.body, "hello from bob");
    assert_eq!(pushed.author.0, bob_id);
}

#[tokio::test]
#[ignore]
async fn dm_without_grant_is_rejected_before_any_request_live() {
    let pool = test_pool().await;
    let (_id, token) =
        seed_identity_session(&pool, &format!("sdk-conv-nogrant-{}", Uuid::new_v4())).await;
    let client = client();

    let session = client.authenticate(&token).await.unwrap();
    let result = session.dm(IdentityId(Uuid::new_v4())).await;
    assert!(matches!(
        result,
        Err(avalon_sdk::SdkError::CapabilityNotGranted(_))
    ));
}

#[tokio::test]
#[ignore]
async fn a_non_participant_reading_another_pairs_conversation_is_rejected() {
    let pool = test_pool().await;
    let client = client();

    let (alice_id, alice_token) =
        seed_identity_session(&pool, &format!("sdk-conv-a-{}", Uuid::new_v4())).await;
    let (bob_id, _bob_token) =
        seed_identity_session(&pool, &format!("sdk-conv-b-{}", Uuid::new_v4())).await;
    let (_mallory_id, mallory_token) =
        seed_identity_session(&pool, &format!("sdk-conv-m-{}", Uuid::new_v4())).await;
    seed_friendship(&pool, alice_id, bob_id).await;

    let alice = client
        .authenticate(&alice_token)
        .await
        .unwrap()
        .grant_for_testing("messages.send");
    let conversation = alice.dm(IdentityId(bob_id)).await.unwrap();
    let conversation_id = conversation.conversation_id();

    let mallory = client
        .authenticate(&mallory_token)
        .await
        .unwrap()
        .grant_for_testing("messages.read")
        .grant_for_testing("messages.send");

    let result = mallory
        .conversation(conversation_id)
        .messages(None, None)
        .await;
    assert!(matches!(
        result,
        Err(avalon_sdk::SdkError::NotConversationParticipant)
    ));

    let result = mallory.conversation(conversation_id).send("intrude").await;
    assert!(matches!(
        result,
        Err(avalon_sdk::SdkError::NotConversationParticipant)
    ));
}
