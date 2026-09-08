//! Exercises guild CRUD, roles, and ownership transfer (issue #20) against
//! a real, running `avalon-server` and Postgres. Gated `--ignored` since it
//! needs live infra — see `make test-live` / `make start`.
//!
//! Test identities are seeded directly via SQL rather than through a real
//! WebAuthn ceremony, same reasoning `crates/server/tests/friends.rs`
//! already documents — guild endpoints don't care how a session was
//! established, only that it's a valid bearer token.

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

/// Seeds a bare identity + session, bypassing WebAuthn entirely, and
/// returns the session's bearer token.
async fn seed_identity_session(pool: &PgPool) -> (Uuid, String) {
    let identity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(identity_id)
        .execute(pool)
        .await
        .expect("failed to seed identity");
    sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(format!("guilds-test-{identity_id}"))
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

/// A short random tag/name pair so concurrent test runs never collide on
/// the case-insensitive uniqueness constraints.
fn unique_guild_body() -> serde_json::Value {
    let suffix = Uuid::new_v4().simple().to_string();
    serde_json::json!({
        "name": format!("Test Guild {}", &suffix[..8]),
        "tag": suffix[..4].to_uppercase(),
        "description": "a guild created by an integration test",
    })
}

#[tokio::test]
#[ignore]
async fn creating_a_guild_makes_the_creator_the_owner() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (owner_id, token) = seed_identity_session(&pool).await;

    let create = auth(http.post(format!("{base}/guilds")), &token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    assert!(create.status().is_success(), "{:?}", create.status());
    let body: serde_json::Value = create.json().await.unwrap();
    assert_eq!(body["owner"].as_str().unwrap(), owner_id.to_string());
    assert_eq!(body["member_count"].as_i64().unwrap(), 1);

    let roles: serde_json::Value = auth(
        http.get(format!(
            "{base}/guilds/{}/roles",
            body["id"].as_str().unwrap()
        )),
        &token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let role_names: Vec<&str> = roles
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(role_names, vec!["owner", "officer", "member"]);
}

#[tokio::test]
#[ignore]
async fn guild_name_is_unique_case_insensitively() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_id, token) = seed_identity_session(&pool).await;

    let mut body = unique_guild_body();
    let first = auth(http.post(format!("{base}/guilds")), &token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success());

    // Same name, different case, different tag — should still collide on name.
    body["name"] = serde_json::json!(body["name"].as_str().unwrap().to_uppercase());
    body["tag"] = serde_json::json!(Uuid::new_v4().simple().to_string()[..4].to_uppercase());
    let second = auth(http.post(format!("{base}/guilds")), &token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test]
#[ignore]
async fn guild_tag_is_unique_case_insensitively() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_id, token) = seed_identity_session(&pool).await;

    let mut body = unique_guild_body();
    let first = auth(http.post(format!("{base}/guilds")), &token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success());

    // Same tag, different case, different name — should still collide on tag.
    body["tag"] = serde_json::json!(body["tag"].as_str().unwrap().to_lowercase());
    body["name"] = serde_json::json!(format!("Different Name {}", Uuid::new_v4()));
    let second = auth(http.post(format!("{base}/guilds")), &token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test]
#[ignore]
async fn transferring_ownership_leaves_exactly_one_owner() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (new_owner_id, _new_owner_token) = seed_identity_session(&pool).await;

    let create = auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap();

    let transfer = auth(
        http.post(format!("{base}/guilds/{guild_id}/transfer-ownership")),
        &owner_token,
    )
    .json(&serde_json::json!({ "to": new_owner_id }))
    .send()
    .await
    .unwrap();
    assert!(transfer.status().is_success(), "{:?}", transfer.status());
    let transferred: serde_json::Value = transfer.json().await.unwrap();
    assert_eq!(
        transferred["owner"].as_str().unwrap(),
        new_owner_id.to_string()
    );

    let fetched: serde_json::Value =
        auth(http.get(format!("{base}/guilds/{guild_id}")), &owner_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(fetched["owner"].as_str().unwrap(), new_owner_id.to_string());
    assert_ne!(fetched["owner"].as_str().unwrap(), owner_id.to_string());

    // The old owner no longer has owner-only authority.
    let second_transfer = auth(
        http.post(format!("{base}/guilds/{guild_id}/transfer-ownership")),
        &owner_token,
    )
    .json(&serde_json::json!({ "to": owner_id }))
    .send()
    .await
    .unwrap();
    assert_eq!(second_transfer.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn a_role_change_by_a_non_manager_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_other_id, other_token) = seed_identity_session(&pool).await;

    let create = auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap();

    // Index 1 is "officer" — see the starter-role ordering asserted in
    // `creating_a_guild_makes_the_creator_the_owner`.
    let update = auth(
        http.patch(format!("{base}/guilds/{guild_id}/roles/1")),
        &other_token,
    )
    .json(&serde_json::json!({ "name": "hijacked" }))
    .send()
    .await
    .unwrap();
    assert_eq!(update.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn guild_endpoints_require_a_session_token() {
    let http = reqwest::Client::new();
    let base = server_url();

    let create = http
        .post(format!("{base}/guilds"))
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    assert_eq!(create.status(), reqwest::StatusCode::UNAUTHORIZED);

    let get = http
        .get(format!("{base}/guilds/{}", Uuid::new_v4()))
        .send()
        .await
        .unwrap();
    assert_eq!(get.status(), reqwest::StatusCode::UNAUTHORIZED);
}
