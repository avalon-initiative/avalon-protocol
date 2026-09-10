//! Exercises the guild message archive tier (issue #253, implementing
//! #193's decision) against a real, running `avalon-server` and Postgres.
//! Gated `--ignored` since it needs live infra — see `make test-live` /
//! `make start`. Same seeding pattern as `crates/server/tests/guild_channels.rs`.
//!
//! Cap-based pruning only fires once a channel holds more than
//! `GUILD_CHANNEL_MESSAGE_CAP` (10,000 by default) messages, which isn't
//! practical to drive through real HTTP requests in a test — that
//! "pruned rows land in the archive, not deleted outright" behavior is
//! covered as a pure-function test in `crates/server/src/guild_messages.rs`
//! instead. These tests seed rows directly into `guild_messages_archive`
//! via SQL (same "seed state directly, exercise only the endpoint under
//! test" pattern `guild_channels.rs` already uses for `guild_members`) and
//! focus on the two things that need a live server: the archive
//! read-access rule, and moderation deletion reaching an already-archived
//! row.

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
        .bind(format!("guild-message-archive-test-{identity_id}"))
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

fn unique_guild_body() -> serde_json::Value {
    let suffix = Uuid::new_v4().simple().to_string();
    serde_json::json!({
        "name": format!("Archive Test Guild {suffix}"),
        "tag": suffix[..4].to_uppercase(),
    })
}

async fn create_guild_with_general_channel(
    http: &reqwest::Client,
    base: &str,
    owner_token: &str,
) -> (String, String) {
    let create = auth(http.post(format!("{base}/guilds")), owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    assert!(create.status().is_success(), "{:?}", create.status());
    let guild: serde_json::Value = create.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap().to_string();

    let channels: serde_json::Value = auth(
        http.get(format!("{base}/guilds/{guild_id}/channels")),
        owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let general = channels
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "general")
        .expect("guild creation should seed a default `general` channel");
    let channel_id = general["id"].as_str().unwrap().to_string();

    (guild_id, channel_id)
}

/// `create_guild_with_general_channel` already makes the caller a real
/// `guild_members` owner row — no separate seed needed, and seeding it
/// again would collide on `guild_members`'s `(guild_id, identity_id)`
/// primary key (same fix as `guild_channels.rs`/`guild_events.rs`).
async fn seed_membership_and_guild(
    http: &reqwest::Client,
    base: &str,
    owner_token: &str,
) -> (String, String) {
    create_guild_with_general_channel(http, base, owner_token).await
}

/// Inserts a row directly into `guild_messages_archive` — see module doc
/// comment on why archive rows are seeded via SQL rather than driven
/// through the 10,000-message cap.
async fn seed_archived_message(
    pool: &PgPool,
    channel_id: Uuid,
    author: Uuid,
    body: &str,
    archived_at: OffsetDateTime,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO guild_messages_archive (id, channel_id, author, body, sent_at, archived_at) \
         VALUES ($1, $2, $3, $4, now(), $5)",
    )
    .bind(id)
    .bind(channel_id)
    .bind(author)
    .bind(body)
    .bind(archived_at)
    .execute(pool)
    .await
    .expect("failed to seed archived message");
    id
}

#[tokio::test]
#[ignore]
async fn a_current_member_can_read_the_archive() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (guild_id, channel_id) = seed_membership_and_guild(&http, &base, &owner_token).await;

    let archived_id = seed_archived_message(
        &pool,
        Uuid::parse_str(&channel_id).unwrap(),
        owner_id,
        "an old message",
        OffsetDateTime::now_utc(),
    )
    .await;

    let list: serde_json::Value = auth(
        http.get(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages/archive"
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&archived_id.to_string().as_str()));
}

/// The chosen archive read-access rule (see `guild_messages.rs`'s module
/// doc comment): current membership, same as the live channel — not
/// membership at the time the archived message was originally sent.
#[tokio::test]
#[ignore]
async fn a_non_member_cannot_read_the_archive() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_outsider_id, outsider_token) = seed_identity_session(&pool).await;
    let (guild_id, channel_id) = seed_membership_and_guild(&http, &base, &owner_token).await;

    seed_archived_message(
        &pool,
        Uuid::parse_str(&channel_id).unwrap(),
        owner_id,
        "an old message",
        OffsetDateTime::now_utc(),
    )
    .await;

    let list = auth(
        http.get(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages/archive"
        )),
        &outsider_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(list.status(), reqwest::StatusCode::FORBIDDEN);
}

/// Moderation-deletion/archive interaction (see `guild_messages.rs`'s
/// module doc comment): `delete_message` also purges an already-archived
/// copy of the same message id, so a moderator's takedown isn't defeated
/// just because cap-based pruning got there first.
#[tokio::test]
#[ignore]
async fn moderation_delete_purges_an_already_archived_message() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (guild_id, channel_id) = seed_membership_and_guild(&http, &base, &owner_token).await;

    let archived_id = seed_archived_message(
        &pool,
        Uuid::parse_str(&channel_id).unwrap(),
        owner_id,
        "abusive content",
        OffsetDateTime::now_utc(),
    )
    .await;

    let delete = auth(
        http.delete(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages/{archived_id}"
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(delete.status().is_success(), "{:?}", delete.status());

    let list: serde_json::Value = auth(
        http.get(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages/archive"
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(!ids.contains(&archived_id.to_string().as_str()));
}

#[tokio::test]
#[ignore]
async fn deleting_a_nonexistent_message_id_404s_even_with_an_archive_fallback() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (guild_id, channel_id) = seed_membership_and_guild(&http, &base, &owner_token).await;

    let delete = auth(
        http.delete(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages/{}",
            Uuid::new_v4()
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(delete.status(), reqwest::StatusCode::NOT_FOUND);
}
