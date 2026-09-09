//! Exercises the guild events calendar + RSVP (issue #169) against a real,
//! running `avalon-server` and Postgres. Gated `--ignored` since it needs
//! live infra — see `make test-live` / `make start`.
//!
//! Seeds identities/sessions/membership directly via SQL, same pattern
//! `crates/server/tests/guild_channels.rs` uses.

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
        .bind(format!("guild-events-test-{identity_id}"))
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
        "INSERT INTO guild_members (guild_id, identity_id, role_index, joined_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(guild_id)
    .bind(identity_id)
    .bind(role_index)
    .execute(pool)
    .await
    .expect("failed to seed guild membership");
}

fn auth(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request.bearer_auth(token)
}

fn unique_guild_body() -> serde_json::Value {
    let suffix = Uuid::new_v4().simple().to_string();
    serde_json::json!({
        "name": format!("Test Guild {}", &suffix[..8]),
        "tag": suffix[..4].to_uppercase(),
        "description": "a guild created by an integration test",
    })
}

/// Creates a guild as `owner_token`'s identity, seeds the creator as an
/// owner-role member, and returns the guild id.
async fn create_guild_with_owner(
    pool: &PgPool,
    http: &reqwest::Client,
    base: &str,
    owner_id: Uuid,
    owner_token: &str,
) -> String {
    let create = auth(http.post(format!("{base}/guilds")), owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    assert!(create.status().is_success(), "{:?}", create.status());
    let guild: serde_json::Value = create.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap().to_string();

    seed_membership(
        pool,
        Uuid::parse_str(&guild_id).unwrap(),
        owner_id,
        0, // owner role index
    )
    .await;

    guild_id
}

fn event_body(title: &str) -> serde_json::Value {
    let starts_at = OffsetDateTime::now_utc() + time::Duration::days(1);
    serde_json::json!({
        "title": title,
        "description": "bring your A game",
        "starts_at": starts_at.format(&time::format_description::well_known::Rfc3339).unwrap(),
    })
}

#[tokio::test]
#[ignore]
async fn create_rsvp_as_two_members_and_list_shows_both_responses() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&pool, &http, &base, owner_id, &owner_token).await;
    seed_membership(
        &pool,
        Uuid::parse_str(&guild_id).unwrap(),
        member_id,
        2, // member role index
    )
    .await;

    let create = auth(
        http.post(format!("{base}/guilds/{guild_id}/events")),
        &owner_token,
    )
    .json(&event_body("Raid night"))
    .send()
    .await
    .unwrap();
    assert!(create.status().is_success(), "{:?}", create.status());
    let event: serde_json::Value = create.json().await.unwrap();
    let event_id = event["id"].as_str().unwrap();

    let owner_rsvp = auth(
        http.put(format!("{base}/guilds/{guild_id}/events/{event_id}/rsvp")),
        &owner_token,
    )
    .json(&serde_json::json!({ "status": "going" }))
    .send()
    .await
    .unwrap();
    assert!(
        owner_rsvp.status().is_success(),
        "{:?}",
        owner_rsvp.status()
    );

    let member_rsvp = auth(
        http.put(format!("{base}/guilds/{guild_id}/events/{event_id}/rsvp")),
        &member_token,
    )
    .json(&serde_json::json!({ "status": "maybe" }))
    .send()
    .await
    .unwrap();
    assert!(
        member_rsvp.status().is_success(),
        "{:?}",
        member_rsvp.status()
    );

    let list: serde_json::Value = auth(
        http.get(format!("{base}/guilds/{guild_id}/events")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let listed = list
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == event_id)
        .expect("created event should be listed");
    assert_eq!(listed["rsvp_counts"]["going"], 1);
    assert_eq!(listed["rsvp_counts"]["maybe"], 1);
    assert_eq!(listed["rsvp_counts"]["not_going"], 0);
}

#[tokio::test]
#[ignore]
async fn rsvp_is_idempotent_per_event_and_identity() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&pool, &http, &base, owner_id, &owner_token).await;

    let create = auth(
        http.post(format!("{base}/guilds/{guild_id}/events")),
        &owner_token,
    )
    .json(&event_body("Tournament prep"))
    .send()
    .await
    .unwrap();
    let event: serde_json::Value = create.json().await.unwrap();
    let event_id = event["id"].as_str().unwrap();

    for status in ["going", "maybe", "not_going"] {
        let rsvp = auth(
            http.put(format!("{base}/guilds/{guild_id}/events/{event_id}/rsvp")),
            &owner_token,
        )
        .json(&serde_json::json!({ "status": status }))
        .send()
        .await
        .unwrap();
        assert!(rsvp.status().is_success(), "{:?}", rsvp.status());
    }

    let list: serde_json::Value = auth(
        http.get(format!("{base}/guilds/{guild_id}/events")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let listed = list
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == event_id)
        .unwrap();
    // Only ever one row per (event, identity) — the last status wins, no
    // accumulation across the three calls above.
    assert_eq!(listed["rsvp_counts"]["going"], 0);
    assert_eq!(listed["rsvp_counts"]["maybe"], 0);
    assert_eq!(listed["rsvp_counts"]["not_going"], 1);
}

#[tokio::test]
#[ignore]
async fn deleting_an_event_removes_its_rsvps() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&pool, &http, &base, owner_id, &owner_token).await;

    let create = auth(
        http.post(format!("{base}/guilds/{guild_id}/events")),
        &owner_token,
    )
    .json(&event_body("Meetup"))
    .send()
    .await
    .unwrap();
    let event: serde_json::Value = create.json().await.unwrap();
    let event_id = event["id"].as_str().unwrap().to_string();

    auth(
        http.put(format!("{base}/guilds/{guild_id}/events/{event_id}/rsvp")),
        &owner_token,
    )
    .json(&serde_json::json!({ "status": "going" }))
    .send()
    .await
    .unwrap();

    let delete = auth(
        http.delete(format!("{base}/guilds/{guild_id}/events/{event_id}")),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(delete.status().is_success(), "{:?}", delete.status());

    let rsvp_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM guild_event_rsvps WHERE event_id = $1::uuid")
            .bind(&event_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(rsvp_count, 0);
}

#[tokio::test]
#[ignore]
async fn a_non_member_cannot_view_or_rsvp() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_outsider_id, outsider_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&pool, &http, &base, owner_id, &owner_token).await;

    let create = auth(
        http.post(format!("{base}/guilds/{guild_id}/events")),
        &owner_token,
    )
    .json(&event_body("Officers-only strategy session"))
    .send()
    .await
    .unwrap();
    let event: serde_json::Value = create.json().await.unwrap();
    let event_id = event["id"].as_str().unwrap();

    let list = auth(
        http.get(format!("{base}/guilds/{guild_id}/events")),
        &outsider_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(list.status(), reqwest::StatusCode::FORBIDDEN);

    let rsvp = auth(
        http.put(format!("{base}/guilds/{guild_id}/events/{event_id}/rsvp")),
        &outsider_token,
    )
    .json(&serde_json::json!({ "status": "going" }))
    .send()
    .await
    .unwrap();
    assert_eq!(rsvp.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn a_member_without_manage_channels_cannot_create_events() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&pool, &http, &base, owner_id, &owner_token).await;
    seed_membership(
        &pool,
        Uuid::parse_str(&guild_id).unwrap(),
        member_id,
        2, // plain member role index, no manage_channels
    )
    .await;

    let create = auth(
        http.post(format!("{base}/guilds/{guild_id}/events")),
        &member_token,
    )
    .json(&event_body("Unauthorized event"))
    .send()
    .await
    .unwrap();
    assert_eq!(create.status(), reqwest::StatusCode::FORBIDDEN);
}
