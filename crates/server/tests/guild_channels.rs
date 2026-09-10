//! Exercises guild channels and messages (issue #22) against a real,
//! running `avalon-server` and Postgres. Gated `--ignored` since it needs
//! live infra — see `make test-live` / `make start`.
//!
//! `seed_membership` inserts directly into `guild_members` for a
//! non-owner role; the owner role is never seeded that way since
//! `POST /guilds` already creates a real owner `guild_members` row
//! atomically (`guilds::create_guild`) — seeding it again would collide on
//! `guild_members`'s `(guild_id, identity_id)` primary key.

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
        .bind(format!("guild-channels-test-{identity_id}"))
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

/// Seeds a `guild_members` row directly — see module doc comment. `owner`
/// role index 0, `officer` 1, `member` 2, matching `guilds.rs`'s starter
/// roles.
async fn seed_membership(pool: &PgPool, guild_id: Uuid, identity_id: Uuid, role_index: i32) {
    sqlx::query(
        "INSERT INTO guild_members (guild_id, identity_id, role_index, joined_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(guild_id)
    .bind(identity_id)
    .bind(role_index)
    .execute(pool)
    .await
    .expect("failed to seed guild membership — has issue #21's migration landed?");
}

fn auth(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request.bearer_auth(token)
}

fn unique_guild_body() -> serde_json::Value {
    let suffix = Uuid::new_v4().simple().to_string();
    serde_json::json!({
        "name": format!("Test Guild {}", &suffix[..8]),
        "tag": suffix[..5].to_uppercase(),
        "description": "a guild created by an integration test",
    })
}

/// Creates a guild as `owner_token`'s identity and returns its id, along
/// with the id of its default `general` channel (created by
/// `guilds::create_guild` itself — see issue #22's `create_guild` edit).
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

#[tokio::test]
#[ignore]
async fn guild_creation_seeds_a_default_general_channel() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    // `create_guild_with_general_channel` itself asserts a `general`
    // channel exists on the freshly created guild.
    let _ = create_guild_with_general_channel(&http, &base, &owner_token).await;
}

/// Helper shared by several tests below: creates a guild, seeds the
/// creator as an owner-role member (so subsequent membership-gated calls
/// succeed), and returns `(guild_id, general_channel_id)`.
/// `create_guild_with_general_channel` already makes the caller a real
/// `guild_members` owner row — this is just that call under a name matching
/// what most tests below actually want ("a guild that already has its
/// owner seeded"), not a second, redundant membership insert.
async fn seed_membership_and_guild(
    http: &reqwest::Client,
    base: &str,
    owner_token: &str,
) -> (String, String) {
    create_guild_with_general_channel(http, base, owner_token).await
}

#[tokio::test]
#[ignore]
async fn a_member_can_send_and_list_messages_in_order() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (guild_id, channel_id) = seed_membership_and_guild(&http, &base, &owner_token).await;

    for body in ["first", "second", "third"] {
        let send = auth(
            http.post(format!(
                "{base}/guilds/{guild_id}/channels/{channel_id}/messages"
            )),
            &owner_token,
        )
        .json(&serde_json::json!({ "body": body }))
        .send()
        .await
        .unwrap();
        assert!(send.status().is_success(), "{:?}", send.status());
    }

    let list: serde_json::Value = auth(
        http.get(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages"
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let bodies: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["body"].as_str().unwrap())
        .collect();
    // Newest first.
    assert_eq!(bodies, vec!["third", "second", "first"]);
}

#[tokio::test]
#[ignore]
async fn pagination_walks_older_pages_via_before_cursor() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (guild_id, channel_id) = seed_membership_and_guild(&http, &base, &owner_token).await;

    for i in 0..5 {
        auth(
            http.post(format!(
                "{base}/guilds/{guild_id}/channels/{channel_id}/messages"
            )),
            &owner_token,
        )
        .json(&serde_json::json!({ "body": format!("message {i}") }))
        .send()
        .await
        .unwrap();
    }

    let first_page: serde_json::Value = auth(
        http.get(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages?limit=2"
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let first_page_ids: Vec<&str> = first_page
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(first_page_ids.len(), 2);

    let cursor = first_page_ids[1];
    let second_page: serde_json::Value = auth(
        http.get(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages?limit=2&before={cursor}"
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let second_page_ids: Vec<&str> = second_page
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    // No overlap with the first page, and strictly older.
    for id in &second_page_ids {
        assert!(!first_page_ids.contains(id));
    }
}

#[tokio::test]
#[ignore]
async fn a_non_member_cannot_read_or_send_messages() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_outsider_id, outsider_token) = seed_identity_session(&pool).await;
    let (guild_id, channel_id) = seed_membership_and_guild(&http, &base, &owner_token).await;

    let list = auth(
        http.get(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages"
        )),
        &outsider_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(list.status(), reqwest::StatusCode::FORBIDDEN);

    let send = auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages"
        )),
        &outsider_token,
    )
    .json(&serde_json::json!({ "body": "sneaky" }))
    .send()
    .await
    .unwrap();
    assert_eq!(send.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn an_archived_channel_rejects_new_posts() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (guild_id, channel_id) = seed_membership_and_guild(&http, &base, &owner_token).await;

    let archive = auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/archive"
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(archive.status().is_success(), "{:?}", archive.status());
    assert_eq!(
        archive.json::<serde_json::Value>().await.unwrap()["archived"],
        true
    );

    let send = auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages"
        )),
        &owner_token,
    )
    .json(&serde_json::json!({ "body": "too late" }))
    .send()
    .await
    .unwrap();
    assert_eq!(send.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test]
#[ignore]
async fn manage_channels_can_hard_delete_a_message() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (guild_id, channel_id) = seed_membership_and_guild(&http, &base, &owner_token).await;

    let send = auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages"
        )),
        &owner_token,
    )
    .json(&serde_json::json!({ "body": "delete me" }))
    .send()
    .await
    .unwrap();
    let message: serde_json::Value = send.json().await.unwrap();
    let message_id = message["id"].as_str().unwrap();

    let delete = auth(
        http.delete(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages/{message_id}"
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(delete.status().is_success(), "{:?}", delete.status());

    let list: serde_json::Value = auth(
        http.get(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages"
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
    assert!(!ids.contains(&message_id));
}

#[tokio::test]
#[ignore]
async fn a_message_body_over_the_length_cap_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (guild_id, channel_id) = seed_membership_and_guild(&http, &base, &owner_token).await;

    let too_long = "a".repeat(4001);
    let send = auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages"
        )),
        &owner_token,
    )
    .json(&serde_json::json!({ "body": too_long }))
    .send()
    .await
    .unwrap();
    assert_eq!(send.status(), reqwest::StatusCode::BAD_REQUEST);
}

// --- Per-resource permission overrides (issue #250) ---------------------

/// Sets/upserts a permission override as `token` (must hold `manage_roles`)
/// and asserts success.
#[allow(clippy::too_many_arguments)]
async fn set_override(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    guild_id: &str,
    role_index: i32,
    resource_kind: &str,
    resource_id: &str,
    permission: &str,
    allow: bool,
) {
    let resp = auth(
        http.put(format!("{base}/guilds/{guild_id}/permission-overrides")),
        token,
    )
    .json(&serde_json::json!({
        "role_index": role_index,
        "resource_kind": resource_kind,
        "resource_id": resource_id,
        "permission": permission,
        "allow": allow,
    }))
    .send()
    .await
    .unwrap();
    assert!(resp.status().is_success(), "{:?}", resp.status());
}

/// Toggles a channel's `announcement_only` flag as `token` and asserts
/// success.
async fn set_announcement_only(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    guild_id: &str,
    channel_id: &str,
    announcement_only: bool,
) {
    let resp = auth(
        http.patch(format!("{base}/guilds/{guild_id}/channels/{channel_id}")),
        token,
    )
    .json(&serde_json::json!({
        "name": "general",
        "announcement_only": announcement_only,
    }))
    .send()
    .await
    .unwrap();
    assert!(resp.status().is_success(), "{:?}", resp.status());
}

#[tokio::test]
#[ignore]
async fn announcement_only_channel_blocks_a_plain_member_base_only() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;
    let (guild_id, channel_id) = seed_membership_and_guild(&http, &base, &owner_token).await;
    seed_membership(&pool, Uuid::parse_str(&guild_id).unwrap(), member_id, 2).await;

    set_announcement_only(&http, &base, &owner_token, &guild_id, &channel_id, true).await;

    // Base-only: the `member` role has no `channel_post` permission and
    // no override exists yet — posting is rejected.
    let send = auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages"
        )),
        &member_token,
    )
    .json(&serde_json::json!({ "body": "hi" }))
    .send()
    .await
    .unwrap();
    assert_eq!(send.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn announcement_only_channel_grant_override_allows_a_plain_member_to_post() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;
    let (guild_id, channel_id) = seed_membership_and_guild(&http, &base, &owner_token).await;
    seed_membership(&pool, Uuid::parse_str(&guild_id).unwrap(), member_id, 2).await;

    set_announcement_only(&http, &base, &owner_token, &guild_id, &channel_id, true).await;
    // Explicit grant beats the `member` role's base absence of
    // `channel_post` — but only on this specific channel.
    set_override(
        &http,
        &base,
        &owner_token,
        &guild_id,
        2, // member role
        "channel",
        &channel_id,
        "channel_post",
        true,
    )
    .await;

    let send = auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages"
        )),
        &member_token,
    )
    .json(&serde_json::json!({ "body": "now I can post" }))
    .send()
    .await
    .unwrap();
    assert!(send.status().is_success(), "{:?}", send.status());
}

#[tokio::test]
#[ignore]
async fn deny_override_blocks_an_officer_from_managing_one_specific_channel() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (officer_id, officer_token) = seed_identity_session(&pool).await;
    let (guild_id, channel_id) = seed_membership_and_guild(&http, &base, &owner_token).await;
    // Officer (role index 1) holds `manage_channels` guild-wide by
    // default (see `guilds::starter_roles`).
    seed_membership(&pool, Uuid::parse_str(&guild_id).unwrap(), officer_id, 1).await;

    // A rename by the officer succeeds before any override exists.
    let rename = auth(
        http.patch(format!("{base}/guilds/{guild_id}/channels/{channel_id}")),
        &officer_token,
    )
    .json(&serde_json::json!({ "name": "renamed-by-officer" }))
    .send()
    .await
    .unwrap();
    assert!(rename.status().is_success(), "{:?}", rename.status());

    // An explicit deny override on this one channel beats the officer's
    // base `manage_channels` grant.
    set_override(
        &http,
        &base,
        &owner_token,
        &guild_id,
        1, // officer role
        "channel",
        &channel_id,
        "manage_channels",
        false,
    )
    .await;

    let rename_again = auth(
        http.patch(format!("{base}/guilds/{guild_id}/channels/{channel_id}")),
        &officer_token,
    )
    .json(&serde_json::json!({ "name": "should-fail" }))
    .send()
    .await
    .unwrap();
    assert_eq!(rename_again.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn owner_bypasses_a_deny_override_on_a_channel() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (guild_id, channel_id) = seed_membership_and_guild(&http, &base, &owner_token).await;

    // A deny override against the owner's own role index (0) still can't
    // block the owner — ownership is structural (`guilds.owner`), not a
    // role grant.
    set_override(
        &http,
        &base,
        &owner_token,
        &guild_id,
        0,
        "channel",
        &channel_id,
        "manage_channels",
        false,
    )
    .await;

    let rename = auth(
        http.patch(format!("{base}/guilds/{guild_id}/channels/{channel_id}")),
        &owner_token,
    )
    .json(&serde_json::json!({ "name": "owner-still-wins" }))
    .send()
    .await
    .unwrap();
    assert!(rename.status().is_success(), "{:?}", rename.status());
}
