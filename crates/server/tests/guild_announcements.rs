//! Exercises `GET /me/guild-announcements` (issue #280) against a real,
//! running `avalon-server` and Postgres. Gated `--ignored` since it needs
//! live infra — see `make test-live` / `make start`.

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
        .bind(format!("guild-announcements-test-{identity_id}"))
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
    .expect("failed to seed membership");
}

fn auth(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request.bearer_auth(token)
}

fn unique_guild_body() -> serde_json::Value {
    let suffix = Uuid::new_v4().simple().to_string();
    serde_json::json!({
        "name": format!("Announcements Test Guild {}", &suffix[..8]),
        "tag": suffix[..5].to_uppercase(),
        "description": "a guild created by an integration test",
    })
}

/// Creates a guild as `owner_token`'s identity and returns
/// `(guild_id, general_channel_id)` — mirrors
/// `crates/server/tests/guild_channels.rs`'s own helper of the same shape.
async fn create_guild_with_general_channel(
    http: &reqwest::Client,
    base: &str,
    owner_token: &str,
) -> (Uuid, Uuid) {
    let create = auth(http.post(format!("{base}/guilds")), owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    assert!(create.status().is_success(), "{:?}", create.status());
    let guild: serde_json::Value = create.json().await.unwrap();
    let guild_id: Uuid = guild["id"].as_str().unwrap().parse().unwrap();

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
    let channel_id: Uuid = general["id"].as_str().unwrap().parse().unwrap();

    (guild_id, channel_id)
}

async fn set_announcement_only(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    guild_id: Uuid,
    channel_id: Uuid,
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

async fn post_message(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    guild_id: Uuid,
    channel_id: Uuid,
    body: &str,
) {
    let resp = auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/channels/{channel_id}/messages"
        )),
        token,
    )
    .json(&serde_json::json!({ "body": body }))
    .send()
    .await
    .unwrap();
    assert!(resp.status().is_success(), "{:?}", resp.status());
}

async fn my_guild_announcements(
    http: &reqwest::Client,
    base: &str,
    token: &str,
) -> Vec<serde_json::Value> {
    auth(http.get(format!("{base}/me/guild-announcements")), token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
#[ignore]
async fn a_post_in_an_announcement_channel_shows_up_for_a_fellow_member() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;

    let (guild_id, channel_id) =
        create_guild_with_general_channel(&http, &base, &owner_token).await;
    seed_membership(&pool, guild_id, member_id, 2).await;
    set_announcement_only(&http, &base, &owner_token, guild_id, channel_id, true).await;
    post_message(
        &http,
        &base,
        &owner_token,
        guild_id,
        channel_id,
        "server maintenance tonight",
    )
    .await;

    let alerts = my_guild_announcements(&http, &base, &member_token).await;
    assert!(
        alerts
            .iter()
            .any(|a| a["body"] == "server maintenance tonight"
                && a["guild_id"] == guild_id.to_string()),
        "the member should see the owner's announcement post: {alerts:?}"
    );
}

#[tokio::test]
#[ignore]
async fn a_post_in_a_regular_non_announcement_channel_is_not_included() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;

    let (guild_id, channel_id) =
        create_guild_with_general_channel(&http, &base, &owner_token).await;
    seed_membership(&pool, guild_id, member_id, 2).await;
    // Deliberately left as a regular channel — announcement_only stays false.
    post_message(
        &http,
        &base,
        &owner_token,
        guild_id,
        channel_id,
        "just chatting",
    )
    .await;

    let alerts = my_guild_announcements(&http, &base, &member_token).await;
    assert!(
        alerts.iter().all(|a| a["body"] != "just chatting"),
        "an ordinary channel's posts must never appear as announcement alerts: {alerts:?}"
    );
}

#[tokio::test]
#[ignore]
async fn leaving_a_guild_stops_its_announcements_from_appearing() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;

    let (guild_id, channel_id) =
        create_guild_with_general_channel(&http, &base, &owner_token).await;
    seed_membership(&pool, guild_id, member_id, 2).await;
    set_announcement_only(&http, &base, &owner_token, guild_id, channel_id, true).await;
    post_message(
        &http,
        &base,
        &owner_token,
        guild_id,
        channel_id,
        "before leaving",
    )
    .await;

    let alerts = my_guild_announcements(&http, &base, &member_token).await;
    assert!(alerts.iter().any(|a| a["body"] == "before leaving"));

    // Leave the guild directly via SQL (no "leave guild" HTTP endpoint
    // exercised elsewhere by this test — the invariant under test is what
    // the read does with membership state, not how membership ends).
    sqlx::query("DELETE FROM indexer_guild_members WHERE guild_id = $1 AND identity_id = $2")
        .bind(guild_id)
        .bind(member_id)
        .execute(&pool)
        .await
        .expect("failed to remove membership");

    let alerts_after_leaving = my_guild_announcements(&http, &base, &member_token).await;
    assert!(
        alerts_after_leaving.iter().all(|a| a["body"] != "before leaving"),
        "a guild's announcements must stop appearing the moment membership ends: {alerts_after_leaving:?}"
    );
}
