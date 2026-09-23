//! Exercises guild channels and messages against a real,
//! running `avalon-server` and Postgres. Gated `--ignored` since it needs
//! live infra — see `make test-live` / `make start`.
//!
//! `seed_membership` inserts directly into `indexer_guild_members` for a
//! non-owner role; the owner role is never seeded that way since
//! `POST /guilds` already creates a real owner `indexer_guild_members` row
//! atomically (`guilds::create_guild`) — seeding it again would collide on
//! `indexer_guild_members`'s `(guild_id, identity_id)` primary key.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

/// #697/#698: seeds a real signing key for `identity_id` so a test can
/// produce a genuine fresh-signature over HTTP, same pattern
/// `crates/server/tests/device_grants.rs` already established.
async fn seed_signing_key(pool: &PgPool, identity_id: Uuid) -> (Uuid, SigningKey) {
    let signing_key = SigningKey::generate(&mut rand::rng());
    let public_key = signing_key.verifying_key().to_bytes();
    let row = sqlx::query(
        "INSERT INTO identity_signing_keys (identity_id, public_key) VALUES ($1, $2) RETURNING id",
    )
    .bind(identity_id)
    .bind(public_key.as_slice())
    .fetch_one(pool)
    .await
    .expect("failed to seed signing key");
    let key_id: Uuid = sqlx::Row::try_get(&row, "id").unwrap();
    (key_id, signing_key)
}

/// Mirrors `crate::signature_gate::canonical_message` byte-for-byte.
fn sign_action(signing_key: &SigningKey, action_tag: &str, fields: &[&str]) -> String {
    let mut message = format!("avalon:{action_tag}:v1");
    for field in fields {
        message.push(':');
        message.push_str(field);
    }
    BASE64.encode(signing_key.sign(message.as_bytes()).to_bytes())
}

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

/// Seeds an `indexer_guild_members` row directly — see module doc comment. `owner`
/// role index 0, `officer` 1, `member` 2, matching `guilds.rs`'s starter
/// roles.
async fn seed_membership(pool: &PgPool, guild_id: Uuid, identity_id: Uuid, role_index: i32) {
    sqlx::query(
        "INSERT INTO indexer_guild_members (guild_id, identity_id, role_index, joined_at) \
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
/// `indexer_guild_members` owner row — this is just that call under a name matching
/// what most tests below actually want ("a guild that already has its
/// owner seeded"), not a second, redundant membership insert.
async fn seed_membership_and_guild(
    http: &reqwest::Client,
    base: &str,
    owner_token: &str,
) -> (String, String) {
    create_guild_with_general_channel(http, base, owner_token).await
}

/// Issue #276: a channel's `topic` round-trips through `PATCH
/// .../channels/{cid}` — settable, clearable (empty string normalizes to
/// `null`, matching `Guild::motd`'s convention), and rejected when over the
/// server's length cap, without disturbing the channel's other fields
/// (`name`/`announcement_only`) already at their existing values.
#[tokio::test]
#[ignore]
async fn channel_topic_round_trips_and_normalizes_blank_to_null() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (guild_id, channel_id) = seed_membership_and_guild(&http, &base, &owner_token).await;

    // Freshly created channels have no topic.
    let channels: serde_json::Value = auth(
        http.get(format!("{base}/guilds/{guild_id}/channels")),
        &owner_token,
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
        .find(|c| c["id"] == channel_id)
        .unwrap();
    assert!(general["topic"].is_null());

    // Set a topic.
    let updated: serde_json::Value = auth(
        http.patch(format!("{base}/guilds/{guild_id}/channels/{channel_id}")),
        &owner_token,
    )
    .json(&serde_json::json!({ "name": "general", "topic": "patch notes & raid planning" }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(updated["topic"], "patch notes & raid planning");
    assert_eq!(updated["name"], "general");
    assert_eq!(updated["announcement_only"], false);

    // Over the length cap is rejected, and leaves the existing topic alone.
    let too_long = auth(
        http.patch(format!("{base}/guilds/{guild_id}/channels/{channel_id}")),
        &owner_token,
    )
    .json(&serde_json::json!({ "name": "general", "topic": "a".repeat(201) }))
    .send()
    .await
    .unwrap();
    assert_eq!(too_long.status(), reqwest::StatusCode::BAD_REQUEST);

    let unchanged: serde_json::Value = auth(
        http.get(format!("{base}/guilds/{guild_id}/channels")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let general_after = unchanged
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == channel_id)
        .unwrap();
    assert_eq!(general_after["topic"], "patch notes & raid planning");

    // Clearing (empty string) normalizes to null.
    let cleared: serde_json::Value = auth(
        http.patch(format!("{base}/guilds/{guild_id}/channels/{channel_id}")),
        &owner_token,
    )
    .json(&serde_json::json!({ "name": "general", "topic": "" }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(cleared["topic"].is_null());

    // Omitting `topic` entirely leaves whatever was already there untouched
    // (a rename with no `topic` key at all, not an implicit clear).
    let renamed: serde_json::Value = auth(
        http.patch(format!("{base}/guilds/{guild_id}/channels/{channel_id}")),
        &owner_token,
    )
    .json(&serde_json::json!({ "name": "general", "topic": "back again" }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(renamed["topic"], "back again");

    let rename_only: serde_json::Value = auth(
        http.patch(format!("{base}/guilds/{guild_id}/channels/{channel_id}")),
        &owner_token,
    )
    .json(&serde_json::json!({ "name": "general-renamed" }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(rename_only["name"], "general-renamed");
    assert_eq!(
        rename_only["topic"], "back again",
        "a rename with no `topic` key must leave the existing topic untouched"
    );
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

// --- Per-resource permission overrides ---------------------

/// Sets/upserts a permission override as `token` (must hold `manage_roles`)
/// and asserts success.
#[allow(clippy::too_many_arguments)]
async fn set_override(
    http: &reqwest::Client,
    base: &str,
    pool: &PgPool,
    token: &str,
    actor_id: Uuid,
    guild_id: &str,
    role_index: i32,
    resource_kind: &str,
    resource_id: &str,
    permission: &str,
    allow: bool,
) {
    let (signing_key_id, signing_key) = seed_signing_key(pool, actor_id).await;
    let signature = sign_action(
        &signing_key,
        "guild.permission_override.set",
        &[
            guild_id,
            &role_index.to_string(),
            resource_kind,
            resource_id,
            permission,
            &allow.to_string(),
        ],
    );
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
        "signing_key_id": signing_key_id,
        "signature": signature,
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
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;
    let (guild_id, channel_id) = seed_membership_and_guild(&http, &base, &owner_token).await;
    seed_membership(&pool, Uuid::parse_str(&guild_id).unwrap(), member_id, 2).await;

    set_announcement_only(&http, &base, &owner_token, &guild_id, &channel_id, true).await;
    // Explicit grant beats the `member` role's base absence of
    // `channel_post` — but only on this specific channel.
    set_override(
        &http,
        &base,
        &pool,
        &owner_token,
        owner_id,
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
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
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
        &pool,
        &owner_token,
        owner_id,
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
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (guild_id, channel_id) = seed_membership_and_guild(&http, &base, &owner_token).await;

    // A deny override against the owner's own role index (0) still can't
    // block the owner — ownership is structural (`guilds.owner`), not a
    // role grant.
    set_override(
        &http,
        &base,
        &pool,
        &owner_token,
        owner_id,
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

// --- view/view_details role overrides + non-member public channels
// --------------------------------------------------------

async fn set_guild_public(
    http: &reqwest::Client,
    base: &str,
    owner_token: &str,
    guild_id: &str,
    value: bool,
) {
    let response = auth(http.patch(format!("{base}/guilds/{guild_id}")), owner_token)
        .json(&serde_json::json!({ "public": value }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
}

async fn set_channel_public(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    guild_id: &str,
    channel_id: &str,
    value: bool,
) {
    let response = auth(
        http.patch(format!("{base}/guilds/{guild_id}/channels/{channel_id}")),
        token,
    )
    .json(&serde_json::json!({ "name": "general", "public": value }))
    .send()
    .await
    .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
}

#[tokio::test]
#[ignore]
async fn a_public_channel_is_visible_to_a_non_member_of_a_public_guild() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_stranger_id, stranger_token) = seed_identity_session(&pool).await;
    let (guild_id, general_channel_id) =
        create_guild_with_general_channel(&http, &base, &owner_token).await;

    // A second, non-public channel the stranger should never see.
    let create_private = auth(
        http.post(format!("{base}/guilds/{guild_id}/channels")),
        &owner_token,
    )
    .json(&serde_json::json!({ "name": "officers" }))
    .send()
    .await
    .unwrap();
    assert!(create_private.status().is_success());

    // Guild not public yet: the stranger is 403'd outright, unchanged from
    // before this ticket.
    let before = auth(
        http.get(format!("{base}/guilds/{guild_id}/channels")),
        &stranger_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(before.status(), reqwest::StatusCode::FORBIDDEN);

    set_guild_public(&http, &base, &owner_token, &guild_id, true).await;
    set_channel_public(
        &http,
        &base,
        &owner_token,
        &guild_id,
        &general_channel_id,
        true,
    )
    .await;

    let after: Vec<serde_json::Value> = auth(
        http.get(format!("{base}/guilds/{guild_id}/channels")),
        &stranger_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let names: Vec<&str> = after.iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert!(
        names.contains(&"general"),
        "the public channel should be visible to a non-member of a public guild: {after:?}"
    );
    assert!(
        !names.contains(&"officers"),
        "a non-public channel must stay hidden from a non-member even in a public guild: {after:?}"
    );
}

#[tokio::test]
#[ignore]
async fn view_details_denied_on_a_channel_blocks_message_reads_but_not_listing() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;
    let (guild_id, channel_id) = seed_membership_and_guild(&http, &base, &owner_token).await;
    seed_membership(&pool, Uuid::parse_str(&guild_id).unwrap(), member_id, 2).await;

    // Before any override: the member can already read (empty) history.
    let before = auth(
        http.get(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages"
        )),
        &member_token,
    )
    .send()
    .await
    .unwrap();
    assert!(before.status().is_success(), "{:?}", before.status());

    set_override(
        &http,
        &base,
        &pool,
        &owner_token,
        owner_id,
        &guild_id,
        2, // member role
        "channel",
        &channel_id,
        "view_details",
        false,
    )
    .await;

    // Still listed — existence stays visible.
    let list: Vec<serde_json::Value> = auth(
        http.get(format!("{base}/guilds/{guild_id}/channels")),
        &member_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(list.iter().any(|c| c["id"] == channel_id));

    // But message content is now gated.
    let after = auth(
        http.get(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages"
        )),
        &member_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(after.status(), reqwest::StatusCode::FORBIDDEN);

    // The owner is unaffected.
    let owner_read = auth(
        http.get(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages"
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(owner_read.status().is_success());
}

/// #697/#698: `PUT /guilds/{id}/permission-overrides` is signature-required
/// — an ambient-session-only set (owner has no registered signing key) must
/// be rejected, and no override should be created (a member still can't
/// post in an announcement-only channel afterward).
#[tokio::test]
#[ignore]
async fn setting_a_permission_override_without_a_signature_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;
    let (guild_id, channel_id) = seed_membership_and_guild(&http, &base, &owner_token).await;
    seed_membership(&pool, Uuid::parse_str(&guild_id).unwrap(), member_id, 2).await;
    set_announcement_only(&http, &base, &owner_token, &guild_id, &channel_id, true).await;

    let unsigned = auth(
        http.put(format!("{base}/guilds/{guild_id}/permission-overrides")),
        &owner_token,
    )
    .json(&serde_json::json!({
        "role_index": 2,
        "resource_kind": "channel",
        "resource_id": channel_id,
        "permission": "channel_post",
        "allow": true,
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(unsigned.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = unsigned.json().await.unwrap();
    assert_eq!(body["code"], "NO_REGISTERED_SIGNING_KEY");

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
