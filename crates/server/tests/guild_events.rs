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
        "INSERT INTO indexer_guild_members (guild_id, identity_id, role_index, joined_at) \
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
        "tag": suffix[..5].to_uppercase(),
        "description": "a guild created by an integration test",
    })
}

/// Creates a guild as `owner_token`'s identity and returns the guild id.
/// `POST /guilds` already makes the creator a real owner `indexer_guild_members`
/// row atomically (`guilds::create_guild`) — no separate seed needed, and
/// seeding it again would collide on `indexer_guild_members`'s `(guild_id,
/// identity_id)` primary key.
async fn create_guild_with_owner(http: &reqwest::Client, base: &str, owner_token: &str) -> String {
    let create = auth(http.post(format!("{base}/guilds")), owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    assert!(create.status().is_success(), "{:?}", create.status());
    let guild: serde_json::Value = create.json().await.unwrap();
    guild["id"].as_str().unwrap().to_string()
}

fn event_body(title: &str) -> serde_json::Value {
    let starts_at = OffsetDateTime::now_utc() + time::Duration::days(1);
    serde_json::json!({
        "title": title,
        "description": "bring your An integrator",
        "starts_at": starts_at.format(&time::format_description::well_known::Rfc3339).unwrap(),
    })
}

/// Same as [`event_body`] but with an explicit `public` flag (issue #448).
fn event_body_with_public(title: &str, public: bool) -> serde_json::Value {
    let mut body = event_body(title);
    body["public"] = serde_json::json!(public);
    body
}

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

#[tokio::test]
#[ignore]
async fn create_rsvp_as_two_members_and_list_shows_both_responses() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&http, &base, &owner_token).await;
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
    // Issue #463: the owner's own list sees their own "going" RSVP, never
    // the member's "maybe" — my_rsvp is always caller-scoped.
    assert_eq!(listed["my_rsvp"], "going");

    let member_list: serde_json::Value = auth(
        http.get(format!("{base}/guilds/{guild_id}/events")),
        &member_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let member_listed = member_list
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == event_id)
        .expect("created event should be listed");
    assert_eq!(member_listed["my_rsvp"], "maybe");
}

#[tokio::test]
#[ignore]
async fn my_rsvp_is_null_until_the_caller_responds() {
    let http = reqwest::Client::new();
    let base = server_url();
    let (_owner_id, owner_token) = seed_identity_session(&test_pool().await).await;
    let guild_id = create_guild_with_owner(&http, &base, &owner_token).await;

    let create = auth(
        http.post(format!("{base}/guilds/{guild_id}/events")),
        &owner_token,
    )
    .json(&event_body("Scrim night"))
    .send()
    .await
    .unwrap();
    let event: serde_json::Value = create.json().await.unwrap();
    let event_id = event["id"].as_str().unwrap();
    assert_eq!(event["my_rsvp"], serde_json::Value::Null);

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
    assert_eq!(listed["my_rsvp"], serde_json::Value::Null);
}

#[tokio::test]
#[ignore]
async fn rsvp_is_idempotent_per_event_and_identity() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&http, &base, &owner_token).await;

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
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&http, &base, &owner_token).await;

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
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_outsider_id, outsider_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&http, &base, &owner_token).await;

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
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&http, &base, &owner_token).await;
    seed_membership(
        &pool,
        Uuid::parse_str(&guild_id).unwrap(),
        member_id,
        2, // plain member role index, no event_manage (issue #250)
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

// --- Per-resource permission overrides (issue #250) ---------------------

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

async fn create_event(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    guild_id: &str,
    title: &str,
) -> String {
    let create = auth(http.post(format!("{base}/guilds/{guild_id}/events")), token)
        .json(&event_body(title))
        .send()
        .await
        .unwrap();
    assert!(create.status().is_success(), "{:?}", create.status());
    let event: serde_json::Value = create.json().await.unwrap();
    event["id"].as_str().unwrap().to_string()
}

#[tokio::test]
#[ignore]
async fn grant_override_lets_a_plain_member_manage_one_specific_event() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&http, &base, &owner_token).await;
    seed_membership(&pool, Uuid::parse_str(&guild_id).unwrap(), member_id, 2).await;

    let event_id = create_event(&http, &base, &owner_token, &guild_id, "Raid Night").await;

    // Base-only: the plain member has no `event_manage` — an edit is
    // rejected before any override exists.
    let edit_before = auth(
        http.patch(format!("{base}/guilds/{guild_id}/events/{event_id}")),
        &member_token,
    )
    .json(&event_body("Raid Night (rescheduled)"))
    .send()
    .await
    .unwrap();
    assert_eq!(edit_before.status(), reqwest::StatusCode::FORBIDDEN);

    // An explicit grant on this one event beats the member role's base
    // absence of `event_manage`.
    set_override(
        &http,
        &base,
        &owner_token,
        &guild_id,
        2, // member role
        "event",
        &event_id,
        "event_manage",
        true,
    )
    .await;

    let edit_after = auth(
        http.patch(format!("{base}/guilds/{guild_id}/events/{event_id}")),
        &member_token,
    )
    .json(&event_body("Raid Night (rescheduled)"))
    .send()
    .await
    .unwrap();
    assert!(
        edit_after.status().is_success(),
        "{:?}",
        edit_after.status()
    );
}

#[tokio::test]
#[ignore]
async fn deny_override_blocks_an_officer_from_deleting_one_specific_event() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (officer_id, officer_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&http, &base, &owner_token).await;
    // Officer (role index 1) holds `event_manage` guild-wide by default
    // since #250's migration backfills it alongside `manage_channels`.
    seed_membership(&pool, Uuid::parse_str(&guild_id).unwrap(), officer_id, 1).await;

    let event_id = create_event(&http, &base, &owner_token, &guild_id, "Tournament Prep").await;

    // An explicit deny override on this one event beats the officer's
    // base `event_manage` grant.
    set_override(
        &http,
        &base,
        &owner_token,
        &guild_id,
        1, // officer role
        "event",
        &event_id,
        "event_manage",
        false,
    )
    .await;

    let delete = auth(
        http.delete(format!("{base}/guilds/{guild_id}/events/{event_id}")),
        &officer_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(delete.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn owner_bypasses_a_deny_override_on_an_event() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&http, &base, &owner_token).await;

    let event_id = create_event(&http, &base, &owner_token, &guild_id, "Owner Event").await;

    set_override(
        &http,
        &base,
        &owner_token,
        &guild_id,
        0,
        "event",
        &event_id,
        "event_manage",
        false,
    )
    .await;

    let delete = auth(
        http.delete(format!("{base}/guilds/{guild_id}/events/{event_id}")),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(delete.status().is_success(), "{:?}", delete.status());
}

#[tokio::test]
#[ignore]
async fn override_on_a_deleted_event_is_inert_not_an_error() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&http, &base, &owner_token).await;
    seed_membership(&pool, Uuid::parse_str(&guild_id).unwrap(), member_id, 2).await;

    let event_id = create_event(&http, &base, &owner_token, &guild_id, "Short-Lived Event").await;
    set_override(
        &http,
        &base,
        &owner_token,
        &guild_id,
        2,
        "event",
        &event_id,
        "event_manage",
        true,
    )
    .await;

    let delete = auth(
        http.delete(format!("{base}/guilds/{guild_id}/events/{event_id}")),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(delete.status().is_success(), "{:?}", delete.status());

    // The event (and, per the ticket's invariant, its now-orphaned
    // override row) is gone — a caller trying to act on it again gets a
    // plain 404, never a server error from a dangling override lookup.
    let edit_after_delete = auth(
        http.patch(format!("{base}/guilds/{guild_id}/events/{event_id}")),
        &member_token,
    )
    .json(&event_body("edit a ghost"))
    .send()
    .await
    .unwrap();
    assert_eq!(edit_after_delete.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore]
async fn rsvp_roster_lists_every_response_with_identity_and_status() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&http, &base, &owner_token).await;
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
    .json(&event_body("Guild social"))
    .send()
    .await
    .unwrap();
    assert!(create.status().is_success(), "{:?}", create.status());
    let event: serde_json::Value = create.json().await.unwrap();
    let event_id = event["id"].as_str().unwrap();

    auth(
        http.put(format!("{base}/guilds/{guild_id}/events/{event_id}/rsvp")),
        &owner_token,
    )
    .json(&serde_json::json!({ "status": "going" }))
    .send()
    .await
    .unwrap();
    auth(
        http.put(format!("{base}/guilds/{guild_id}/events/{event_id}/rsvp")),
        &member_token,
    )
    .json(&serde_json::json!({ "status": "maybe" }))
    .send()
    .await
    .unwrap();

    let roster = auth(
        http.get(format!("{base}/guilds/{guild_id}/events/{event_id}/rsvps")),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(roster.status().is_success(), "{:?}", roster.status());
    let roster: serde_json::Value = roster.json().await.unwrap();
    let roster = roster.as_array().unwrap();
    assert_eq!(roster.len(), 2);

    let owner_entry = roster
        .iter()
        .find(|r| r["identity_id"] == owner_id.to_string())
        .expect("owner's rsvp should be in the roster");
    assert_eq!(owner_entry["status"], "going");
    assert!(owner_entry["responded_at"].is_string());

    let member_entry = roster
        .iter()
        .find(|r| r["identity_id"] == member_id.to_string())
        .expect("member's rsvp should be in the roster");
    assert_eq!(member_entry["status"], "maybe");
}

#[tokio::test]
#[ignore]
async fn rsvp_roster_is_empty_when_nobody_has_responded() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&http, &base, &owner_token).await;

    let create = auth(
        http.post(format!("{base}/guilds/{guild_id}/events")),
        &owner_token,
    )
    .json(&event_body("Empty roster event"))
    .send()
    .await
    .unwrap();
    let event: serde_json::Value = create.json().await.unwrap();
    let event_id = event["id"].as_str().unwrap();

    let roster = auth(
        http.get(format!("{base}/guilds/{guild_id}/events/{event_id}/rsvps")),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(roster.status().is_success(), "{:?}", roster.status());
    let roster: serde_json::Value = roster.json().await.unwrap();
    assert_eq!(roster.as_array().unwrap().len(), 0);
}

#[tokio::test]
#[ignore]
async fn a_non_member_cannot_view_the_rsvp_roster() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_outsider_id, outsider_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&http, &base, &owner_token).await;

    let create = auth(
        http.post(format!("{base}/guilds/{guild_id}/events")),
        &owner_token,
    )
    .json(&event_body("Members-only roster"))
    .send()
    .await
    .unwrap();
    let event: serde_json::Value = create.json().await.unwrap();
    let event_id = event["id"].as_str().unwrap();

    let roster = auth(
        http.get(format!("{base}/guilds/{guild_id}/events/{event_id}/rsvps")),
        &outsider_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(roster.status(), reqwest::StatusCode::FORBIDDEN);
}

/// An event id from a DIFFERENT guild than the one in the URL must 404,
/// not resolve — otherwise a member of guild B could read guild A's RSVP
/// roster just by pasting A's event id under B's guild_id in the path.
#[tokio::test]
#[ignore]
async fn requesting_another_guilds_event_id_under_a_different_guild_is_not_found() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_a_id, owner_a_token) = seed_identity_session(&pool).await;
    let (_owner_b_id, owner_b_token) = seed_identity_session(&pool).await;
    let guild_a_id = create_guild_with_owner(&http, &base, &owner_a_token).await;
    let guild_b_id = create_guild_with_owner(&http, &base, &owner_b_token).await;

    let create = auth(
        http.post(format!("{base}/guilds/{guild_a_id}/events")),
        &owner_a_token,
    )
    .json(&event_body("Guild A's event"))
    .send()
    .await
    .unwrap();
    let event: serde_json::Value = create.json().await.unwrap();
    let event_id = event["id"].as_str().unwrap();

    let cross_guild = auth(
        http.get(format!(
            "{base}/guilds/{guild_b_id}/events/{event_id}/rsvps"
        )),
        &owner_b_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(cross_guild.status(), reqwest::StatusCode::NOT_FOUND);
}

/// Issue #448: a non-member of a `public` guild (independent of
/// `recruiting`, #449) sees only that guild's `public` events via
/// `GET /guilds/{id}/events`, not its member-only ones — and still can't
/// RSVP or view the roster, since `public` only widens the *list*, never
/// the RSVP/roster endpoints (those stay member-only per the ticket's
/// invariants).
#[tokio::test]
#[ignore]
async fn a_non_member_of_a_public_guild_sees_only_public_events() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_outsider_id, outsider_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&http, &base, &owner_token).await;

    let public_event = auth(
        http.post(format!("{base}/guilds/{guild_id}/events")),
        &owner_token,
    )
    .json(&event_body_with_public("Community mixer", true))
    .send()
    .await
    .unwrap();
    assert!(
        public_event.status().is_success(),
        "{:?}",
        public_event.status()
    );
    let public_event: serde_json::Value = public_event.json().await.unwrap();
    let public_event_id = public_event["id"].as_str().unwrap().to_string();
    assert!(public_event["public"].as_bool().unwrap());

    let private_event = auth(
        http.post(format!("{base}/guilds/{guild_id}/events")),
        &owner_token,
    )
    .json(&event_body("Officer planning"))
    .send()
    .await
    .unwrap();
    let private_event: serde_json::Value = private_event.json().await.unwrap();
    assert!(!private_event["public"].as_bool().unwrap());

    // Not yet public: the outsider is still 403'd, exactly like #391/#448's
    // "unchanged for a non-public guild" invariant.
    let still_forbidden = auth(
        http.get(format!("{base}/guilds/{guild_id}/events")),
        &outsider_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(still_forbidden.status(), reqwest::StatusCode::FORBIDDEN);

    set_guild_public(&http, &base, &owner_token, &guild_id, true).await;

    let list = auth(
        http.get(format!("{base}/guilds/{guild_id}/events")),
        &outsider_token,
    )
    .send()
    .await
    .unwrap();
    assert!(list.status().is_success(), "{:?}", list.status());
    let events: Vec<serde_json::Value> = list.json().await.unwrap();
    let ids: Vec<&str> = events.iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        vec![public_event_id.as_str()],
        "only the public event should be listed"
    );

    // Public only widens the list — RSVP and roster stay member-only.
    let rsvp = auth(
        http.put(format!(
            "{base}/guilds/{guild_id}/events/{public_event_id}/rsvp"
        )),
        &outsider_token,
    )
    .json(&serde_json::json!({ "status": "going" }))
    .send()
    .await
    .unwrap();
    assert_eq!(rsvp.status(), reqwest::StatusCode::FORBIDDEN);

    // Turning `public` back off restores the 403, same as the roster
    // override's own "off restores today's behavior" invariant.
    set_guild_public(&http, &base, &owner_token, &guild_id, false).await;
    let forbidden_again = auth(
        http.get(format!("{base}/guilds/{guild_id}/events")),
        &outsider_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(forbidden_again.status(), reqwest::StatusCode::FORBIDDEN);
}

/// A member sees every event regardless of its `public` flag or the
/// guild's own `public` setting — `public` only ever narrows what a
/// *non-member* sees.
#[tokio::test]
#[ignore]
async fn a_member_sees_every_event_regardless_of_public_flag() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&http, &base, &owner_token).await;
    seed_membership(&pool, guild_id.parse().unwrap(), member_id, 2).await;

    auth(
        http.post(format!("{base}/guilds/{guild_id}/events")),
        &owner_token,
    )
    .json(&event_body_with_public("Community mixer", true))
    .send()
    .await
    .unwrap();
    auth(
        http.post(format!("{base}/guilds/{guild_id}/events")),
        &owner_token,
    )
    .json(&event_body("Officer planning"))
    .send()
    .await
    .unwrap();

    // Guild is not `public` at all, and the member still sees both events.
    let list = auth(
        http.get(format!("{base}/guilds/{guild_id}/events")),
        &member_token,
    )
    .send()
    .await
    .unwrap();
    assert!(list.status().is_success(), "{:?}", list.status());
    let events: Vec<serde_json::Value> = list.json().await.unwrap();
    assert_eq!(events.len(), 2);
}

// --- view/view_details role overrides (issue #458) -----------------------

#[tokio::test]
#[ignore]
async fn denying_view_on_one_event_hides_it_from_that_member_only() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&http, &base, &owner_token).await;
    seed_membership(&pool, Uuid::parse_str(&guild_id).unwrap(), member_id, 2).await;

    let secret_event_id =
        create_event(&http, &base, &owner_token, &guild_id, "Officers only").await;
    let public_event_id = create_event(&http, &base, &owner_token, &guild_id, "Everyone").await;

    // Before any override: the member sees both.
    let before: Vec<serde_json::Value> = auth(
        http.get(format!("{base}/guilds/{guild_id}/events")),
        &member_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(before.len(), 2);

    set_override(
        &http,
        &base,
        &owner_token,
        &guild_id,
        2, // member role
        "event",
        &secret_event_id,
        "view",
        false,
    )
    .await;

    let after: Vec<serde_json::Value> = auth(
        http.get(format!("{base}/guilds/{guild_id}/events")),
        &member_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let ids: Vec<&str> = after.iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert!(
        !ids.contains(&secret_event_id.as_str()),
        "the denied event must not appear in the member's own list: {after:?}"
    );
    assert!(ids.contains(&public_event_id.as_str()));

    // The owner is unaffected by the override.
    let owner_view: Vec<serde_json::Value> = auth(
        http.get(format!("{base}/guilds/{guild_id}/events")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(owner_view.len(), 2);
}

#[tokio::test]
#[ignore]
async fn denying_view_details_leaves_the_event_visible_but_strips_its_content() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;
    let guild_id = create_guild_with_owner(&http, &base, &owner_token).await;
    seed_membership(&pool, Uuid::parse_str(&guild_id).unwrap(), member_id, 2).await;

    let event_id = create_event(&http, &base, &owner_token, &guild_id, "Raid night").await;

    set_override(
        &http,
        &base,
        &owner_token,
        &guild_id,
        2,
        "event",
        &event_id,
        "view_details",
        false,
    )
    .await;

    let list: Vec<serde_json::Value> = auth(
        http.get(format!("{base}/guilds/{guild_id}/events")),
        &member_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let event = list
        .iter()
        .find(|e| e["id"] == event_id)
        .expect("the event should still be visible — only its details are gated");
    assert_eq!(event["title"], "Raid night");
    assert_eq!(event["details_visible"], false);
    assert!(event["description"].is_null());
    assert!(event["channel_id"].is_null());
    assert!(event["my_rsvp"].is_null());
    assert_eq!(event["rsvp_counts"]["going"], 0);
    assert_eq!(event["rsvp_counts"]["maybe"], 0);
    assert_eq!(event["rsvp_counts"]["not_going"], 0);

    // The owner still sees full detail.
    let owner_list: Vec<serde_json::Value> = auth(
        http.get(format!("{base}/guilds/{guild_id}/events")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let owner_event = owner_list.iter().find(|e| e["id"] == event_id).unwrap();
    assert_eq!(owner_event["details_visible"], true);
    assert_eq!(owner_event["description"], "bring your An integrator");
}
