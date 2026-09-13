//! Exercises `Profile::main_guild` (no ticket — see that field's own doc
//! comment in `crates/protocol/src/identity.rs`) against a real, running
//! `avalon-server` and Postgres. Gated `--ignored` since it needs live
//! infra — see `make test-live` / `make start`.
//!
//! Test identities are seeded directly via SQL, same reasoning
//! `crates/server/tests/guilds.rs` already documents.

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
        .bind(format!("main-guild-test-{identity_id}"))
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
        "name": format!("Main Guild Test {}", &suffix[..8]),
        "tag": suffix[..5].to_uppercase(),
        "description": "a guild created by a main_guild integration test",
    })
}

/// Creates a guild owned by `owner_token`, opens it, and has `member_token`
/// join it — returns the guild id. Used whenever a test needs a member who
/// isn't the guild's owner (an owner can't leave without transferring
/// first, so the leave-clears-`main_guild` test needs a real non-owner).
async fn create_open_guild_and_join(
    http: &reqwest::Client,
    base: &str,
    owner_token: &str,
    member_token: &str,
) -> String {
    let create = auth(http.post(format!("{base}/guilds")), owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    assert!(create.status().is_success(), "{:?}", create.status());
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap().to_string();

    let open = auth(http.patch(format!("{base}/guilds/{guild_id}")), owner_token)
        .json(&serde_json::json!({ "join_policy": "open" }))
        .send()
        .await
        .unwrap();
    assert!(open.status().is_success());

    let join = auth(
        http.post(format!("{base}/guilds/{guild_id}/join")),
        member_token,
    )
    .send()
    .await
    .unwrap();
    assert!(join.status().is_success(), "{:?}", join.status());

    guild_id
}

async fn get_me(http: &reqwest::Client, base: &str, token: &str) -> serde_json::Value {
    auth(http.get(format!("{base}/me")), token)
        .send()
        .await
        .expect("GET /me failed — is `make start` running?")
        .json()
        .await
        .unwrap()
}

#[tokio::test]
#[ignore]
async fn setting_main_guild_to_a_membership_succeeds_and_round_trips() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;

    // Creating a guild makes the creator a member of it.
    let create = auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap().to_string();

    let update = auth(http.patch(format!("{base}/me")), &owner_token)
        .json(&serde_json::json!({ "main_guild": guild_id }))
        .send()
        .await
        .unwrap();
    assert!(update.status().is_success(), "{:?}", update.status());
    let updated: serde_json::Value = update.json().await.unwrap();
    assert_eq!(updated["main_guild"].as_str().unwrap(), guild_id);
    assert_eq!(updated["effective_main_guild"].as_str().unwrap(), guild_id);

    // Round-trips through a fresh GET /me too, not just the PATCH response.
    let me = get_me(&http, &base, &owner_token).await;
    assert_eq!(me["main_guild"].as_str().unwrap(), guild_id);
    assert_eq!(me["effective_main_guild"].as_str().unwrap(), guild_id);
}

#[tokio::test]
#[ignore]
async fn setting_main_guild_to_a_guild_youre_not_a_member_of_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_other_owner_id, other_owner_token) = seed_identity_session(&pool).await;

    // A guild owned by a different identity — `owner_token`'s identity is
    // never a member of it.
    let create = auth(http.post(format!("{base}/guilds")), &other_owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap().to_string();

    let update = auth(http.patch(format!("{base}/me")), &owner_token)
        .json(&serde_json::json!({ "main_guild": guild_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(update.status(), reqwest::StatusCode::FORBIDDEN);

    // Never silently accepted or partially applied.
    let me = get_me(&http, &base, &owner_token).await;
    assert!(me["main_guild"].is_null());
}

#[tokio::test]
#[ignore]
async fn setting_main_guild_to_a_malformed_id_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_id, token) = seed_identity_session(&pool).await;

    let update = auth(http.patch(format!("{base}/me")), &token)
        .json(&serde_json::json!({ "main_guild": "not-a-uuid" }))
        .send()
        .await
        .unwrap();
    assert_eq!(update.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore]
async fn leaving_your_main_guild_clears_it() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_member_id, member_token) = seed_identity_session(&pool).await;

    let guild_id = create_open_guild_and_join(&http, &base, &owner_token, &member_token).await;

    let set = auth(http.patch(format!("{base}/me")), &member_token)
        .json(&serde_json::json!({ "main_guild": guild_id }))
        .send()
        .await
        .unwrap();
    assert!(set.status().is_success());
    let set_body: serde_json::Value = set.json().await.unwrap();
    assert_eq!(set_body["main_guild"].as_str().unwrap(), guild_id);

    let leave = auth(
        http.post(format!("{base}/guilds/{guild_id}/leave")),
        &member_token,
    )
    .send()
    .await
    .unwrap();
    assert!(leave.status().is_success(), "{:?}", leave.status());

    let me = get_me(&http, &base, &member_token).await;
    assert!(
        me["main_guild"].is_null(),
        "main_guild should have been cleared on leaving its guild, got {:?}",
        me["main_guild"]
    );
}

#[tokio::test]
#[ignore]
async fn leaving_a_guild_that_isnt_your_main_guild_leaves_it_untouched() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_a, owner_a_token) = seed_identity_session(&pool).await;
    let (_owner_b, owner_b_token) = seed_identity_session(&pool).await;
    let (_member_id, member_token) = seed_identity_session(&pool).await;

    let guild_a = create_open_guild_and_join(&http, &base, &owner_a_token, &member_token).await;
    let guild_b = create_open_guild_and_join(&http, &base, &owner_b_token, &member_token).await;

    let set = auth(http.patch(format!("{base}/me")), &member_token)
        .json(&serde_json::json!({ "main_guild": guild_a }))
        .send()
        .await
        .unwrap();
    assert!(set.status().is_success());

    let leave = auth(
        http.post(format!("{base}/guilds/{guild_b}/leave")),
        &member_token,
    )
    .send()
    .await
    .unwrap();
    assert!(leave.status().is_success());

    let me = get_me(&http, &base, &member_token).await;
    assert_eq!(me["main_guild"].as_str().unwrap(), guild_a);
}

#[tokio::test]
#[ignore]
async fn effective_main_guild_defaults_to_the_earliest_joined_membership_when_unset() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_a, owner_a_token) = seed_identity_session(&pool).await;
    let (_owner_b, owner_b_token) = seed_identity_session(&pool).await;
    let (_member_id, member_token) = seed_identity_session(&pool).await;

    // Joined strictly in this order — `guild_a` first.
    let guild_a = create_open_guild_and_join(&http, &base, &owner_a_token, &member_token).await;
    let guild_b = create_open_guild_and_join(&http, &base, &owner_b_token, &member_token).await;
    assert_ne!(guild_a, guild_b);

    let me = get_me(&http, &base, &member_token).await;
    assert!(
        me["main_guild"].is_null(),
        "main_guild was never explicitly set"
    );
    assert_eq!(
        me["effective_main_guild"].as_str().unwrap(),
        guild_a,
        "effective_main_guild should default to the earliest-joined membership"
    );
}

#[tokio::test]
#[ignore]
async fn main_guild_can_be_explicitly_cleared() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;

    let create = auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap().to_string();

    let set = auth(http.patch(format!("{base}/me")), &owner_token)
        .json(&serde_json::json!({ "main_guild": guild_id }))
        .send()
        .await
        .unwrap();
    assert!(set.status().is_success());

    let clear = auth(http.patch(format!("{base}/me")), &owner_token)
        .json(&serde_json::json!({ "main_guild": "" }))
        .send()
        .await
        .unwrap();
    assert!(clear.status().is_success());
    let cleared: serde_json::Value = clear.json().await.unwrap();
    assert!(cleared["main_guild"].is_null());
    // Falls back to the earliest-joined membership, not to null, since the
    // identity is still a member of the guild it just un-set.
    assert_eq!(cleared["effective_main_guild"].as_str().unwrap(), guild_id);
}
