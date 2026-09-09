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

/// Issue #152: creating a role with a description/badge round-trips
/// through `GET /guilds/{id}/roles`.
#[tokio::test]
#[ignore]
async fn creating_a_role_with_description_and_badge_round_trips() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, token) = seed_identity_session(&pool).await;

    let create_guild = auth(http.post(format!("{base}/guilds")), &token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    assert!(create_guild.status().is_success());
    let guild: serde_json::Value = create_guild.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    let create_role = auth(http.post(format!("{base}/guilds/{guild_id}/roles")), &token)
        .json(&serde_json::json!({
            "name": "Raid Leader",
            "permissions": ["manage_members"],
            "description": "Leads raid nights and manages the roster.",
            "badge": { "icon": "sword", "color": "purple" },
        }))
        .send()
        .await
        .unwrap();
    assert!(
        create_role.status().is_success(),
        "{:?}",
        create_role.status()
    );
    let created: serde_json::Value = create_role.json().await.unwrap();
    assert_eq!(
        created["description"].as_str().unwrap(),
        "Leads raid nights and manages the roster."
    );
    assert_eq!(created["badge"]["icon"].as_str().unwrap(), "sword");
    assert_eq!(created["badge"]["color"].as_str().unwrap(), "purple");

    let roles: serde_json::Value =
        auth(http.get(format!("{base}/guilds/{guild_id}/roles")), &token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let raid_leader = roles
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"].as_str().unwrap() == "Raid Leader")
        .expect("newly created role should be listed");
    assert_eq!(
        raid_leader["description"].as_str().unwrap(),
        "Leads raid nights and manages the roster."
    );
    assert_eq!(raid_leader["badge"]["icon"].as_str().unwrap(), "sword");
    assert_eq!(raid_leader["badge"]["color"].as_str().unwrap(), "purple");
}

/// Issue #152: an unrecognized badge icon is rejected outright, not
/// silently dropped or coerced to a default.
#[tokio::test]
#[ignore]
async fn creating_a_role_with_an_unknown_badge_icon_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, token) = seed_identity_session(&pool).await;

    let create_guild = auth(http.post(format!("{base}/guilds")), &token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    let guild: serde_json::Value = create_guild.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    let create_role = auth(http.post(format!("{base}/guilds/{guild_id}/roles")), &token)
        .json(&serde_json::json!({
            "name": "Bogus",
            "badge": { "icon": "not_a_real_icon", "color": "gold" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(create_role.status(), reqwest::StatusCode::BAD_REQUEST);
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

// --- Membership lifecycle (issue #21) --------------------------------------

#[tokio::test]
#[ignore]
async fn creating_a_guild_makes_the_owner_a_member() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (owner_id, token) = seed_identity_session(&pool).await;

    let create = auth(http.post(format!("{base}/guilds")), &token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap();

    let members: serde_json::Value = auth(
        http.get(format!("{base}/guilds/{guild_id}/members")),
        &token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let members = members.as_array().unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(
        members[0]["identity_id"].as_str().unwrap(),
        owner_id.to_string()
    );
    assert_eq!(members[0]["role_index"].as_i64().unwrap(), 0);
}

#[tokio::test]
#[ignore]
async fn invite_accept_join_and_leave_flow() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (invitee_id, invitee_token) = seed_identity_session(&pool).await;

    let create = auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap();

    let invite = auth(
        http.post(format!("{base}/guilds/{guild_id}/invites")),
        &owner_token,
    )
    .json(&serde_json::json!({ "to": invitee_id }))
    .send()
    .await
    .unwrap();
    assert!(invite.status().is_success(), "{:?}", invite.status());
    let invite_body: serde_json::Value = invite.json().await.unwrap();
    let invite_id = invite_body["id"].as_str().unwrap();

    // Re-inviting while the first invite is still pending is idempotent —
    // same invite id back, not a new row or an error.
    let duplicate = auth(
        http.post(format!("{base}/guilds/{guild_id}/invites")),
        &owner_token,
    )
    .json(&serde_json::json!({ "to": invitee_id }))
    .send()
    .await
    .unwrap();
    assert!(duplicate.status().is_success(), "{:?}", duplicate.status());
    let duplicate_body: serde_json::Value = duplicate.json().await.unwrap();
    assert_eq!(duplicate_body["id"].as_str().unwrap(), invite_id);

    let accept = auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/invites/{invite_id}/accept"
        )),
        &invitee_token,
    )
    .send()
    .await
    .unwrap();
    assert!(accept.status().is_success(), "{:?}", accept.status());

    let members: serde_json::Value = auth(
        http.get(format!("{base}/guilds/{guild_id}/members")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(members.as_array().unwrap().len(), 2);

    let leave = auth(
        http.post(format!("{base}/guilds/{guild_id}/leave")),
        &invitee_token,
    )
    .send()
    .await
    .unwrap();
    assert!(leave.status().is_success(), "{:?}", leave.status());

    let members_after: serde_json::Value = auth(
        http.get(format!("{base}/guilds/{guild_id}/members")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(members_after.as_array().unwrap().len(), 1);
}

#[tokio::test]
#[ignore]
async fn joining_an_invite_only_guild_directly_is_rejected() {
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
    // Guilds default to invite-only (ticket design) — no endpoint here
    // flips it to open, so this always exercises the closed path.
    assert_eq!(body["join_policy"].as_str().unwrap(), "invite_only");

    let join = auth(
        http.post(format!("{base}/guilds/{guild_id}/join")),
        &other_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(join.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn the_owner_cannot_leave_or_be_removed() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;

    let create = auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap();

    let leave = auth(
        http.post(format!("{base}/guilds/{guild_id}/leave")),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(leave.status(), reqwest::StatusCode::FORBIDDEN);

    let remove = auth(
        http.delete(format!("{base}/guilds/{guild_id}/members/{owner_id}")),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(remove.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn an_officer_cannot_remove_another_officer_without_manage_roles() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (officer_a_id, officer_a_token) = seed_identity_session(&pool).await;
    let (officer_b_id, officer_b_token) = seed_identity_session(&pool).await;

    let create = auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap();

    for (identity_id, identity_token) in [
        (officer_a_id, &officer_a_token),
        (officer_b_id, &officer_b_token),
    ] {
        let invite = auth(
            http.post(format!("{base}/guilds/{guild_id}/invites")),
            &owner_token,
        )
        .json(&serde_json::json!({ "to": identity_id }))
        .send()
        .await
        .unwrap();
        let invite_body: serde_json::Value = invite.json().await.unwrap();
        let invite_id = invite_body["id"].as_str().unwrap();
        auth(
            http.post(format!(
                "{base}/guilds/{guild_id}/invites/{invite_id}/accept"
            )),
            identity_token,
        )
        .send()
        .await
        .unwrap();

        // Index 1 is "officer" — see the starter-role ordering asserted in
        // `creating_a_guild_makes_the_creator_the_owner`.
        let promote = auth(
            http.patch(format!("{base}/guilds/{guild_id}/members/{identity_id}")),
            &owner_token,
        )
        .json(&serde_json::json!({ "role_index": 1 }))
        .send()
        .await
        .unwrap();
        assert!(promote.status().is_success(), "{:?}", promote.status());
    }

    // Officer A (manage_members, no manage_roles) tries to remove Officer B.
    let remove = auth(
        http.delete(format!("{base}/guilds/{guild_id}/members/{officer_b_id}")),
        &officer_a_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(remove.status(), reqwest::StatusCode::FORBIDDEN);
}

/// Issue #153: setting motd/banner/links/recruiting via `PATCH
/// /guilds/{id}` round-trips through `GET /guilds/{id}`.
#[tokio::test]
#[ignore]
async fn guild_metadata_round_trips_through_patch_and_get() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, token) = seed_identity_session(&pool).await;

    let create_guild = auth(http.post(format!("{base}/guilds")), &token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    assert!(create_guild.status().is_success());
    let guild: serde_json::Value = create_guild.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();
    // Defaults before any metadata is set.
    assert!(guild["motd"].is_null());
    assert!(guild["banner"].is_null());
    assert_eq!(guild["links"].as_array().unwrap().len(), 0);
    assert!(!guild["recruiting"].as_bool().unwrap());

    let patch = auth(http.patch(format!("{base}/guilds/{guild_id}")), &token)
        .json(&serde_json::json!({
            "motd": "Raid night every Friday!",
            "banner": "https://example.com/banner.png",
            "links": [
                { "label": "Discord", "url": "https://discord.gg/example" },
                { "label": "Website", "url": "https://example.com" },
            ],
            "recruiting": true,
        }))
        .send()
        .await
        .unwrap();
    assert!(patch.status().is_success(), "{:?}", patch.status());
    let patched: serde_json::Value = patch.json().await.unwrap();
    assert_eq!(
        patched["motd"].as_str().unwrap(),
        "Raid night every Friday!"
    );
    assert_eq!(
        patched["banner"].as_str().unwrap(),
        "https://example.com/banner.png"
    );
    assert_eq!(patched["links"].as_array().unwrap().len(), 2);
    assert!(patched["recruiting"].as_bool().unwrap());

    let fetched: serde_json::Value = auth(http.get(format!("{base}/guilds/{guild_id}")), &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        fetched["motd"].as_str().unwrap(),
        "Raid night every Friday!"
    );
    assert_eq!(
        fetched["banner"].as_str().unwrap(),
        "https://example.com/banner.png"
    );
    assert_eq!(fetched["links"][0]["label"].as_str().unwrap(), "Discord");
    assert_eq!(
        fetched["links"][0]["url"].as_str().unwrap(),
        "https://discord.gg/example"
    );
    assert!(fetched["recruiting"].as_bool().unwrap());

    // Clearing motd/banner via empty string, and links via an empty list.
    let clear = auth(http.patch(format!("{base}/guilds/{guild_id}")), &token)
        .json(&serde_json::json!({
            "motd": "",
            "banner": "",
            "links": [],
            "recruiting": false,
        }))
        .send()
        .await
        .unwrap();
    assert!(clear.status().is_success(), "{:?}", clear.status());
    let cleared: serde_json::Value = clear.json().await.unwrap();
    assert!(cleared["motd"].is_null());
    assert!(cleared["banner"].is_null());
    assert_eq!(cleared["links"].as_array().unwrap().len(), 0);
    assert!(!cleared["recruiting"].as_bool().unwrap());
}

/// Issue #153: invalid metadata (overlong motd, non-http(s) banner, too
/// many links) is rejected outright, not silently dropped or truncated.
#[tokio::test]
#[ignore]
async fn invalid_guild_metadata_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, token) = seed_identity_session(&pool).await;

    let create_guild = auth(http.post(format!("{base}/guilds")), &token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    let guild: serde_json::Value = create_guild.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    let overlong_motd = auth(http.patch(format!("{base}/guilds/{guild_id}")), &token)
        .json(&serde_json::json!({ "motd": "a".repeat(501) }))
        .send()
        .await
        .unwrap();
    assert_eq!(overlong_motd.status(), reqwest::StatusCode::BAD_REQUEST);

    let bad_banner = auth(http.patch(format!("{base}/guilds/{guild_id}")), &token)
        .json(&serde_json::json!({ "banner": "javascript:alert(1)" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_banner.status(), reqwest::StatusCode::BAD_REQUEST);

    let too_many_links: Vec<serde_json::Value> = (0..6)
        .map(|i| serde_json::json!({ "label": format!("Link {i}"), "url": "https://example.com" }))
        .collect();
    let bad_links = auth(http.patch(format!("{base}/guilds/{guild_id}")), &token)
        .json(&serde_json::json!({ "links": too_many_links }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_links.status(), reqwest::StatusCode::BAD_REQUEST);
}

// -- Issue #154: `GET /guilds/discover` --

async fn create_guild(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    name_hint: &str,
) -> serde_json::Value {
    let suffix = Uuid::new_v4().simple().to_string();
    let body = serde_json::json!({
        "name": format!("{name_hint} {}", &suffix[..8]),
        "tag": suffix[..4].to_uppercase(),
        "description": format!("a discover-test guild ({name_hint})"),
    });
    let create = auth(http.post(format!("{base}/guilds")), token)
        .json(&body)
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    assert!(create.status().is_success(), "{:?}", create.status());
    create.json().await.unwrap()
}

async fn set_recruiting(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    guild_id: &str,
    recruiting: bool,
) {
    let patch = auth(http.patch(format!("{base}/guilds/{guild_id}")), token)
        .json(&serde_json::json!({ "recruiting": recruiting }))
        .send()
        .await
        .unwrap();
    assert!(patch.status().is_success(), "{:?}", patch.status());
}

/// Ticket #154 acceptance criteria: a recruiting guild is discoverable by a
/// non-member; a non-recruiting guild is excluded from the general browse
/// (`recruiting=` omitted) results for a caller who isn't a member of it,
/// while exact `GET /guilds/{id}` lookup still finds it unchanged.
#[tokio::test]
#[ignore]
async fn recruiting_filter_excludes_non_recruiting_guilds_from_general_browse() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_stranger_id, stranger_token) = seed_identity_session(&pool).await;

    let recruiting_guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let recruiting_id = recruiting_guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, recruiting_id, true).await;

    let closed_guild = create_guild(&http, &base, &owner_token, "Closed Guild").await;
    let closed_id = closed_guild["id"].as_str().unwrap();
    // `recruiting` defaults to `false` — no PATCH needed to leave it closed.

    // A stranger's default browse (no `recruiting=` filter) sees the
    // recruiting guild but not the closed one.
    let browse: serde_json::Value =
        auth(http.get(format!("{base}/guilds/discover")), &stranger_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let ids: Vec<&str> = browse["guilds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&recruiting_id));
    assert!(!ids.contains(&closed_id));

    // Exact id lookup still finds the closed guild, unchanged.
    let direct = auth(
        http.get(format!("{base}/guilds/{closed_id}")),
        &stranger_token,
    )
    .send()
    .await
    .unwrap();
    assert!(direct.status().is_success());

    // `recruiting=false` is an explicit filter, so it surfaces the closed
    // guild (and excludes the recruiting one).
    let closed_browse: serde_json::Value = auth(
        http.get(format!("{base}/guilds/discover?recruiting=false")),
        &stranger_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let closed_ids: Vec<&str> = closed_browse["guilds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert!(closed_ids.contains(&closed_id));
    assert!(!closed_ids.contains(&recruiting_id));
}

/// A member of a non-recruiting guild still sees it in their own default
/// (no `recruiting=`) browse results — only strangers lose visibility.
#[tokio::test]
#[ignore]
async fn a_member_still_sees_their_own_non_recruiting_guild_in_default_browse() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;

    let closed_guild = create_guild(&http, &base, &owner_token, "Own Closed Guild").await;
    let closed_id = closed_guild["id"].as_str().unwrap();

    let browse: serde_json::Value = auth(http.get(format!("{base}/guilds/discover")), &owner_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = browse["guilds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&closed_id));
}

/// Free-text `q=` matches name/tag substrings case-insensitively.
#[tokio::test]
#[ignore]
async fn search_matches_name_and_tag_substrings_case_insensitively() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;

    let suffix = Uuid::new_v4().simple().to_string();
    let unique_word = format!("Zephyrion{}", &suffix[..6]);
    let body = serde_json::json!({
        "name": format!("{unique_word} Vanguard"),
        "tag": suffix[..4].to_uppercase(),
        "description": "a discover-test guild",
    });
    let create = auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(create.status().is_success());
    let guild: serde_json::Value = create.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    let lowercase_query = unique_word.to_lowercase();
    let search: serde_json::Value = auth(
        http.get(format!(
            "{base}/guilds/discover?q={lowercase_query}&recruiting=true"
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let ids: Vec<&str> = search["guilds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&guild_id));
}

/// Cursor pagination doesn't skip or duplicate rows across pages.
#[tokio::test]
#[ignore]
async fn pagination_does_not_skip_or_duplicate_rows_across_pages() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;

    let mut created_ids = Vec::new();
    for _ in 0..5 {
        let guild = create_guild(&http, &base, &owner_token, "Paged Guild").await;
        let id = guild["id"].as_str().unwrap().to_string();
        set_recruiting(&http, &base, &owner_token, &id, true).await;
        created_ids.push(id);
    }

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let url = match &cursor {
            Some(c) => format!("{base}/guilds/discover?recruiting=true&limit=2&cursor={c}"),
            None => format!("{base}/guilds/discover?recruiting=true&limit=2"),
        };
        let page: serde_json::Value = auth(http.get(url), &owner_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let page_ids: Vec<String> = page["guilds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["id"].as_str().unwrap().to_string())
            .collect();
        for id in &page_ids {
            assert!(
                !seen.contains(id),
                "guild {id} appeared on more than one page"
            );
        }
        seen.extend(page_ids);

        cursor = page["next_cursor"].as_str().map(|s| s.to_string());
        if cursor.is_none() {
            break;
        }
    }

    for id in &created_ids {
        assert!(
            seen.contains(id),
            "guild {id} never appeared across any page"
        );
    }
}
