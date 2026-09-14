//! Exercises `Session::guilds`/`guild(id).roster()`/`.channels()`/
//! `.channel(cid).messages()`/`.send()` (issue #23) against a real, running
//! `avalon-server`. Gated `--ignored`, same as `tests/social.rs` — needs
//! live infra (`make test-live` / `make start`).
//!
//! `authenticate()` always returns a `Session` with no grants (the
//! capability-grant system, #26–#28, isn't built yet), so these tests use
//! `Session::grant_for_testing` to exercise the capability-gated methods
//! against a real server — see that method's doc comment.
//!
//! Unlike `tests/social.rs` (which seeds friendships directly via SQL),
//! guild fixtures here are created through the real HTTP API
//! (`POST /guilds`): creating a guild atomically makes the creator its
//! owner (a real `guild_members` row) and seeds a default `general`
//! channel (`crates/server/src/guilds.rs::create_guild`), which is exactly
//! the flow this test wants to exercise the SDK against — a real
//! #20/#21/#22 guild, not a hand-seeded row.

use avalon_protocol::ids::GuildId;
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
/// approach `crates/sdk/tests/social.rs` uses.
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
        integrator_credential_key_id: "sdk-test".to_string(),
        integrator_slug: None,
        signing_key: None,
        retry: Default::default(),
    })
}

/// Creates a guild as `token` via the real HTTP API and returns its id.
/// `tag` must be 2-5 chars (`validate_tag` in `crates/server/src/guilds.rs`).
async fn create_guild(http: &reqwest::Client, base: &str, token: &str, tag: &str) -> Uuid {
    let response = http
        .post(format!("{base}/guilds"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "name": format!("Guild {tag}"),
            "tag": tag,
            "description": "sdk guild test fixture",
        }))
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    assert!(response.status().is_success());
    let body: serde_json::Value = response.json().await.unwrap();
    Uuid::parse_str(body["id"].as_str().unwrap()).unwrap()
}

#[tokio::test]
#[ignore]
async fn guilds_lists_a_membership_created_via_the_http_api() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let client = client();

    let (alice_id, alice_token) =
        seed_identity_session(&pool, &format!("sdk-guilds-alice-{}", Uuid::new_v4())).await;
    let tag = format!("G{}", &Uuid::new_v4().simple().to_string()[..4]);
    let guild_id = create_guild(&http, &base, &alice_token, &tag).await;

    let session = client
        .authenticate(&alice_token)
        .await
        .expect("authenticate should succeed")
        .grant_for_testing("guilds.read");

    let memberships = session.guilds().await.expect("guilds() should succeed");
    let membership = memberships
        .iter()
        .find(|m| m.guild.id == GuildId(guild_id))
        .expect("the guild just created should be in the caller's memberships");
    assert_eq!(membership.guild.owner.0, alice_id);
    assert_eq!(membership.guild.tag, tag);
}

#[tokio::test]
#[ignore]
async fn guilds_without_grant_is_rejected_before_any_request_live() {
    let pool = test_pool().await;
    let (_id, token) =
        seed_identity_session(&pool, &format!("sdk-guilds-nogrant-{}", Uuid::new_v4())).await;
    let client = client();

    let session = client.authenticate(&token).await.unwrap();
    let result = session.guilds().await;
    assert!(matches!(
        result,
        Err(avalon_sdk::SdkError::CapabilityNotGranted(_))
    ));
}

#[tokio::test]
#[ignore]
async fn roster_returns_the_owner_as_a_member() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let client = client();

    let (alice_id, alice_token) =
        seed_identity_session(&pool, &format!("sdk-roster-alice-{}", Uuid::new_v4())).await;
    let tag = format!("R{}", &Uuid::new_v4().simple().to_string()[..4]);
    let guild_id = create_guild(&http, &base, &alice_token, &tag).await;

    let session = client
        .authenticate(&alice_token)
        .await
        .unwrap()
        .grant_for_testing("guilds.read");

    let roster = session
        .guild(GuildId(guild_id))
        .roster()
        .await
        .expect("roster() should succeed");
    assert_eq!(roster.len(), 1);
    assert_eq!(roster[0].member.identity_id.0, alice_id);
    // presence.read wasn't granted, so no presence should be embedded.
    assert!(roster[0].presence.is_none());
}

#[tokio::test]
#[ignore]
async fn send_then_messages_round_trips_a_message() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let client = client();

    let (alice_id, alice_token) =
        seed_identity_session(&pool, &format!("sdk-chat-alice-{}", Uuid::new_v4())).await;
    let tag = format!("C{}", &Uuid::new_v4().simple().to_string()[..4]);
    let guild_id = create_guild(&http, &base, &alice_token, &tag).await;

    let session = client
        .authenticate(&alice_token)
        .await
        .unwrap()
        .grant_for_testing("guilds.chat");

    let channels = session
        .guild(GuildId(guild_id))
        .channels()
        .await
        .expect("channels() should succeed");
    let general = channels
        .iter()
        .find(|c| c.name == "general")
        .expect("every guild is seeded with a default general channel");

    let sent = session
        .guild(GuildId(guild_id))
        .channel(general.id)
        .send("hello from the sdk")
        .await
        .expect("send() should succeed");
    assert_eq!(sent.author.0, alice_id);
    assert_eq!(sent.body, "hello from the sdk");

    let messages = session
        .guild(GuildId(guild_id))
        .channel(general.id)
        .messages(None, None)
        .await
        .expect("messages() should succeed");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].id, sent.id);
    assert_eq!(messages[0].body, "hello from the sdk");
}

#[tokio::test]
#[ignore]
async fn channels_without_guilds_chat_is_rejected_even_with_guilds_read() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let client = client();

    let (_alice_id, alice_token) =
        seed_identity_session(&pool, &format!("sdk-chat-nogrant-{}", Uuid::new_v4())).await;
    let tag = format!("N{}", &Uuid::new_v4().simple().to_string()[..4]);
    let guild_id = create_guild(&http, &base, &alice_token, &tag).await;

    // Only guilds.read granted, not guilds.chat — there is no guilds.*
    // blanket check.
    let session = client
        .authenticate(&alice_token)
        .await
        .unwrap()
        .grant_for_testing("guilds.read");

    let result = session.guild(GuildId(guild_id)).channels().await;
    assert!(matches!(
        result,
        Err(avalon_sdk::SdkError::CapabilityNotGranted(_))
    ));
}
