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

/// Two roles in the same guild can't share a name (case-insensitively) —
/// nothing previously stopped a guild from having several roles all named
/// e.g. "Master", which makes role names useless for telling roles apart.
#[tokio::test]
#[ignore]
async fn creating_a_role_with_a_name_already_taken_in_the_guild_is_rejected() {
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

    let first = auth(http.post(format!("{base}/guilds/{guild_id}/roles")), &token)
        .json(&serde_json::json!({ "name": "Raid Leader" }))
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success());

    let duplicate = auth(http.post(format!("{base}/guilds/{guild_id}/roles")), &token)
        .json(&serde_json::json!({ "name": "raid leader" }))
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), reqwest::StatusCode::CONFLICT);
}

/// The owner role's name/description/badge are cosmetic-only and safe to
/// change; its permissions are structurally meaningless (owner authority
/// comes from guilds.owner, not this row) and stay rejected.
#[tokio::test]
#[ignore]
async fn the_owner_role_can_be_renamed_but_not_have_its_permissions_changed() {
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

    let rename = auth(
        http.patch(format!("{base}/guilds/{guild_id}/roles/0")),
        &token,
    )
    .json(&serde_json::json!({ "name": "Guild Master" }))
    .send()
    .await
    .unwrap();
    assert!(rename.status().is_success(), "{:?}", rename.status());
    let renamed: serde_json::Value = rename.json().await.unwrap();
    assert_eq!(renamed["name"].as_str().unwrap(), "Guild Master");

    let change_permissions = auth(
        http.patch(format!("{base}/guilds/{guild_id}/roles/0")),
        &token,
    )
    .json(&serde_json::json!({ "permissions": ["manage_guild"] }))
    .send()
    .await
    .unwrap();
    assert_eq!(change_permissions.status(), reqwest::StatusCode::FORBIDDEN);
}

/// The owner and member roles can never be deleted (structural — owner
/// authority lives on guilds.owner, member is the hardcoded default join
/// role); any other role can be, as long as no member still holds it.
#[tokio::test]
#[ignore]
async fn deleting_a_role() {
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

    // Base roles: owner (0) and member (2) can never be deleted.
    let delete_owner = auth(
        http.delete(format!("{base}/guilds/{guild_id}/roles/0")),
        &token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(delete_owner.status(), reqwest::StatusCode::FORBIDDEN);

    let delete_member = auth(
        http.delete(format!("{base}/guilds/{guild_id}/roles/2")),
        &token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(delete_member.status(), reqwest::StatusCode::FORBIDDEN);

    // A freshly created, unassigned custom role deletes cleanly.
    let create_role = auth(http.post(format!("{base}/guilds/{guild_id}/roles")), &token)
        .json(&serde_json::json!({ "name": "Temp Role" }))
        .send()
        .await
        .unwrap();
    let role: serde_json::Value = create_role.json().await.unwrap();
    let name_index = role["name_index"].as_i64().unwrap();

    let delete = auth(
        http.delete(format!("{base}/guilds/{guild_id}/roles/{name_index}")),
        &token,
    )
    .send()
    .await
    .unwrap();
    assert!(delete.status().is_success(), "{:?}", delete.status());

    let roles: serde_json::Value =
        auth(http.get(format!("{base}/guilds/{guild_id}/roles")), &token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert!(roles
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["name_index"].as_i64().unwrap() != name_index));
}

/// A custom role still held by a member can't be deleted — the database's
/// own foreign key (`guild_members.role_index` -> `guild_roles.name_index`)
/// catches this, mapped to a clean 409 rather than a raw DB error.
#[tokio::test]
#[ignore]
async fn deleting_a_role_still_held_by_a_member_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (other_id, other_token) = seed_identity_session(&pool).await;

    let create_guild = auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    let guild: serde_json::Value = create_guild.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    let create_role = auth(
        http.post(format!("{base}/guilds/{guild_id}/roles")),
        &owner_token,
    )
    .json(&serde_json::json!({ "name": "Quartermaster" }))
    .send()
    .await
    .unwrap();
    let role: serde_json::Value = create_role.json().await.unwrap();
    let name_index = role["name_index"].as_i64().unwrap();

    let open = auth(
        http.patch(format!("{base}/guilds/{guild_id}")),
        &owner_token,
    )
    .json(&serde_json::json!({ "join_policy": "open" }))
    .send()
    .await
    .unwrap();
    assert!(open.status().is_success());

    let join = auth(
        http.post(format!("{base}/guilds/{guild_id}/join")),
        &other_token,
    )
    .send()
    .await
    .unwrap();
    assert!(join.status().is_success());

    let assign = auth(
        http.patch(format!("{base}/guilds/{guild_id}/members/{other_id}")),
        &owner_token,
    )
    .json(&serde_json::json!({ "role_index": name_index }))
    .send()
    .await
    .unwrap();
    assert!(assign.status().is_success(), "{:?}", assign.status());

    let delete = auth(
        http.delete(format!("{base}/guilds/{guild_id}/roles/{name_index}")),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(delete.status(), reqwest::StatusCode::CONFLICT);
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
    // Guilds default to invite-only (ticket design) — this test never
    // touches join_policy, so it always exercises the closed path.
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
async fn opening_a_guild_lets_a_stranger_join_directly_bypassing_requests_and_invites() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (other_id, other_token) = seed_identity_session(&pool).await;

    let create = auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap();

    let patch = auth(
        http.patch(format!("{base}/guilds/{guild_id}")),
        &owner_token,
    )
    .json(&serde_json::json!({ "join_policy": "open" }))
    .send()
    .await
    .unwrap();
    assert!(patch.status().is_success(), "{:?}", patch.status());
    let patched: serde_json::Value = patch.json().await.unwrap();
    assert_eq!(patched["join_policy"].as_str().unwrap(), "open");

    // No invite, no join-request/approval round trip — straight to member.
    let join = auth(
        http.post(format!("{base}/guilds/{guild_id}/join")),
        &other_token,
    )
    .send()
    .await
    .unwrap();
    assert!(join.status().is_success(), "{:?}", join.status());
    let member: serde_json::Value = join.json().await.unwrap();
    assert_eq!(
        member["identity_id"].as_str().unwrap(),
        other_id.to_string()
    );

    let members = auth(
        http.get(format!("{base}/guilds/{guild_id}/members")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json::<Vec<serde_json::Value>>()
    .await
    .unwrap();
    assert!(members
        .iter()
        .any(|m| m["identity_id"].as_str().unwrap() == other_id.to_string()));
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
/// non-member; a non-recruiting guild is excluded from browse results (both
/// the default `recruiting=` omitted case and an explicit
/// `recruiting=false`) for a caller who isn't a member of it, while exact
/// `GET /guilds/{id}` lookup still finds it unchanged, and a member always
/// sees their own non-recruiting guild under `recruiting=false`.
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

    // `recruiting=false` from a stranger must NOT bulk-leak the closed
    // guild (or any other non-recruiting guild the stranger isn't in) —
    // it stays membership-gated, so a non-member sees nothing here either.
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
    assert!(!closed_ids.contains(&closed_id));
    assert!(!closed_ids.contains(&recruiting_id));

    // The owner, who *is* a member of the closed guild, sees it under an
    // explicit `recruiting=false` query.
    let owner_closed_browse: serde_json::Value = auth(
        http.get(format!("{base}/guilds/discover?recruiting=false")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let owner_closed_ids: Vec<&str> = owner_closed_browse["guilds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert!(owner_closed_ids.contains(&closed_id));
    assert!(!owner_closed_ids.contains(&recruiting_id));
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

/// Issue #258: Discover cards carry `banner`/`icon`, round-tripping the
/// values set via `PATCH /guilds/{id}` (#153/#246), and reporting `null`
/// for a guild that never set them — same shape `GET /guilds/{id}` already
/// returns, just also reachable from the browse listing.
#[tokio::test]
#[ignore]
async fn discover_card_includes_banner_and_icon() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;

    let with_media = create_guild(&http, &base, &owner_token, "Media Guild").await;
    let with_media_id = with_media["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, with_media_id, true).await;
    let patch = auth(http.patch(format!("{base}/guilds/{with_media_id}")), &owner_token)
        .json(&serde_json::json!({
            "banner": "https://example.com/banner.png",
            "icon": "https://example.com/icon.png",
        }))
        .send()
        .await
        .unwrap();
    assert!(patch.status().is_success());

    let without_media = create_guild(&http, &base, &owner_token, "Plain Guild").await;
    let without_media_id = without_media["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, without_media_id, true).await;

    let browse: serde_json::Value = auth(http.get(format!("{base}/guilds/discover")), &owner_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let guilds = browse["guilds"].as_array().unwrap();

    let with_media_card = guilds
        .iter()
        .find(|g| g["id"].as_str().unwrap() == with_media_id)
        .expect("media guild missing from discover results");
    assert_eq!(
        with_media_card["banner"].as_str().unwrap(),
        "https://example.com/banner.png"
    );
    assert_eq!(
        with_media_card["icon"].as_str().unwrap(),
        "https://example.com/icon.png"
    );

    let without_media_card = guilds
        .iter()
        .find(|g| g["id"].as_str().unwrap() == without_media_id)
        .expect("plain guild missing from discover results");
    assert!(without_media_card["banner"].is_null());
    assert!(without_media_card["icon"].is_null());
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

// -- Issue #206 (implementing decision #160): game affinity breakdown --

/// Seeds a `games` row directly (bypassing the game registration ceremony,
/// same "seed via SQL, endpoint behavior doesn't depend on how the row got
/// there" reasoning `seed_identity_session` above already documents) and
/// returns its id.
async fn seed_game(pool: &PgPool, name: &str) -> Uuid {
    let game_id = Uuid::new_v4();
    let slug = format!(
        "{}-{}",
        name.to_lowercase().replace(' ', "-"),
        Uuid::new_v4().simple()
    );
    sqlx::query(
        "INSERT INTO games (id, slug, name, developer, registered_at, status) \
         VALUES ($1, $2, $3, 'test developer', now(), 'active')",
    )
    .bind(game_id)
    .bind(&slug[..slug.len().min(64)])
    .bind(name)
    .execute(pool)
    .await
    .expect("failed to seed game");
    game_id
}

/// Seeds an active `bindings` row directly — see `seed_game`'s own note.
async fn seed_binding(pool: &PgPool, identity_id: Uuid, game_id: Uuid) -> Uuid {
    let binding_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO bindings (id, identity_id, game_id, established_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(binding_id)
    .bind(identity_id)
    .bind(game_id)
    .execute(pool)
    .await
    .expect("failed to seed binding");
    binding_id
}

async fn end_binding(pool: &PgPool, binding_id: Uuid) {
    sqlx::query("UPDATE bindings SET ended_at = now() WHERE id = $1")
        .bind(binding_id)
        .execute(pool)
        .await
        .expect("failed to end binding");
}

/// Invites `to_identity_id` into `guild_id` and accepts on their behalf,
/// landing them as a plain member (role index 2, no permissions) — the
/// same flow `invite_accept_join_and_leave_flow` above exercises, reused
/// here just to get a second real guild member without hand-writing a
/// `guild_members` row.
async fn invite_and_accept(
    http: &reqwest::Client,
    base: &str,
    owner_token: &str,
    to_token: &str,
    guild_id: &str,
    to_identity_id: Uuid,
) {
    let invite = auth(
        http.post(format!("{base}/guilds/{guild_id}/invites")),
        owner_token,
    )
    .json(&serde_json::json!({ "to": to_identity_id }))
    .send()
    .await
    .unwrap();
    assert!(invite.status().is_success(), "{:?}", invite.status());
    let invite_body: serde_json::Value = invite.json().await.unwrap();
    let invite_id = invite_body["id"].as_str().unwrap();

    let accept = auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/invites/{invite_id}/accept"
        )),
        to_token,
    )
    .send()
    .await
    .unwrap();
    assert!(accept.status().is_success(), "{:?}", accept.status());
}

/// #206's core acceptance criteria: the breakdown is computed from real
/// `GameBinding` (#83) data only, updates as bindings change, and is never
/// something a manager can add for a game with zero bound members (there's
/// no add action at all — this test never calls one).
#[tokio::test]
#[ignore]
async fn game_breakdown_reflects_active_bindings_and_updates_as_they_change() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;

    let create = auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let guild: serde_json::Value = create.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    invite_and_accept(
        &http,
        &base,
        &owner_token,
        &member_token,
        guild_id,
        member_id,
    )
    .await;

    let ashen = seed_game(&pool, "Ashen Realms").await;
    let ocean = seed_game(&pool, "Ocean World").await;

    // Owner and member both play Ashen Realms; only the member plays Ocean
    // World. No game association is ever declared — the breakdown must
    // fall entirely out of these bindings.
    let owner_ashen_binding = seed_binding(&pool, owner_id, ashen).await;
    seed_binding(&pool, member_id, ashen).await;
    seed_binding(&pool, member_id, ocean).await;

    let fetch_breakdown = || {
        let http = http.clone();
        let base = base.clone();
        let guild_id = guild_id.to_string();
        let token = owner_token.clone();
        async move {
            auth(
                http.get(format!("{base}/guilds/{guild_id}/game-breakdown")),
                &token,
            )
            .send()
            .await
            .unwrap()
        }
    };

    let resp = fetch_breakdown().await;
    assert!(resp.status().is_success(), "{:?}", resp.status());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["total_members"].as_i64().unwrap(), 2);
    let by_game: std::collections::HashMap<String, i64> = body["breakdown"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["game_name"].as_str().unwrap().to_string(),
                e["member_count"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(by_game.get("Ashen Realms"), Some(&2));
    assert_eq!(by_game.get("Ocean World"), Some(&1));

    // Ending the owner's Ashen Realms binding drops that count to 1 — the
    // breakdown is live-computed, not a stale/cached association.
    end_binding(&pool, owner_ashen_binding).await;

    let resp = fetch_breakdown().await;
    assert!(resp.status().is_success(), "{:?}", resp.status());
    let body: serde_json::Value = resp.json().await.unwrap();
    let by_game: std::collections::HashMap<String, i64> = body["breakdown"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["game_name"].as_str().unwrap().to_string(),
                e["member_count"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(by_game.get("Ashen Realms"), Some(&1));
    assert_eq!(by_game.get("Ocean World"), Some(&1));

    // A plain member (no manage_guild) cannot fetch the breakdown while
    // the guild hasn't opted into public exposure.
    let member_view = auth(
        http.get(format!("{base}/guilds/{guild_id}/game-breakdown")),
        &member_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(member_view.status(), reqwest::StatusCode::FORBIDDEN);
}

/// #206/#160's show/hide toggle: a non-member gets 403 while
/// `game_breakdown_public` is false, and can fetch the same breakdown once
/// a `manage_guild` holder flips it on via `PATCH /guilds/{id}` — the
/// owner's own view is unaffected by the toggle either way, since they can
/// always see it internally.
#[tokio::test]
#[ignore]
async fn game_breakdown_public_toggle_gates_exposure_to_non_members() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_stranger_id, stranger_token) = seed_identity_session(&pool).await;

    let create = auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let guild: serde_json::Value = create.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();
    assert!(!guild["game_breakdown_public"].as_bool().unwrap());

    let ashen = seed_game(&pool, "Ashen Realms").await;
    seed_binding(&pool, owner_id, ashen).await;

    // Not public yet: a non-member (and non-manager) is rejected.
    let before = auth(
        http.get(format!("{base}/guilds/{guild_id}/game-breakdown")),
        &stranger_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(before.status(), reqwest::StatusCode::FORBIDDEN);

    // The owner can always see it regardless of the toggle.
    let owner_view = auth(
        http.get(format!("{base}/guilds/{guild_id}/game-breakdown")),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(
        owner_view.status().is_success(),
        "{:?}",
        owner_view.status()
    );

    let patch = auth(
        http.patch(format!("{base}/guilds/{guild_id}")),
        &owner_token,
    )
    .json(&serde_json::json!({ "game_breakdown_public": true }))
    .send()
    .await
    .unwrap();
    assert!(patch.status().is_success(), "{:?}", patch.status());
    let patched: serde_json::Value = patch.json().await.unwrap();
    assert!(patched["game_breakdown_public"].as_bool().unwrap());

    // Now public: the same stranger can fetch it.
    let after = auth(
        http.get(format!("{base}/guilds/{guild_id}/game-breakdown")),
        &stranger_token,
    )
    .send()
    .await
    .unwrap();
    assert!(after.status().is_success(), "{:?}", after.status());
    let body: serde_json::Value = after.json().await.unwrap();
    assert_eq!(
        body["breakdown"][0]["game_name"].as_str().unwrap(),
        "Ashen Realms"
    );
}

// --- Issue #207: favorite games pin list ------------------------------

/// #207's core round-trip: a `manage_guild` holder (the owner here) pins,
/// reorders, and unpins games, always drawing from real #206 affinity data.
/// Also covers the cap and the zero-bound-members rejection at the HTTP
/// layer (unit tests in `crates/server/src/guilds.rs` cover the same
/// invariants against the pure validator directly).
#[tokio::test]
#[ignore]
async fn pin_reorder_and_unpin_round_trip() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;

    let create = auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let guild: serde_json::Value = create.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    let ashen = seed_game(&pool, "Ashen Realms").await;
    let ocean = seed_game(&pool, "Ocean World").await;
    let unbound = seed_game(&pool, "Unbound Game").await;

    // The owner is bound to both Ashen Realms and Ocean World — both have
    // real affinity — but never binds to `unbound`.
    seed_binding(&pool, owner_id, ashen).await;
    seed_binding(&pool, owner_id, ocean).await;

    let put_favorites = |game_ids: Vec<Uuid>| {
        let http = http.clone();
        let base = base.clone();
        let guild_id = guild_id.to_string();
        let token = owner_token.clone();
        async move {
            auth(
                http.put(format!("{base}/guilds/{guild_id}/favorite-games")),
                &token,
            )
            .json(&serde_json::json!({ "game_ids": game_ids }))
            .send()
            .await
            .unwrap()
        }
    };

    // Pinning a game with zero bound members is rejected outright.
    let rejected = put_favorites(vec![unbound]).await;
    assert_eq!(rejected.status(), reqwest::StatusCode::FORBIDDEN);

    // Pinning both real-affinity games in order succeeds.
    let pinned = put_favorites(vec![ashen, ocean]).await;
    assert!(pinned.status().is_success(), "{:?}", pinned.status());
    let body: serde_json::Value = pinned.json().await.unwrap();
    let names: Vec<&str> = body["favorites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["game_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Ashen Realms", "Ocean World"]);
    assert!(!body["favorites"][0]["stale"].as_bool().unwrap());

    // Reordering (Ocean World first) round-trips through both the PUT
    // response and a subsequent GET.
    let reordered = put_favorites(vec![ocean, ashen]).await;
    assert!(reordered.status().is_success(), "{:?}", reordered.status());
    let body: serde_json::Value = reordered.json().await.unwrap();
    let names: Vec<&str> = body["favorites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["game_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Ocean World", "Ashen Realms"]);

    let get = auth(
        http.get(format!("{base}/guilds/{guild_id}/favorite-games")),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(get.status().is_success(), "{:?}", get.status());
    let get_body: serde_json::Value = get.json().await.unwrap();
    let names: Vec<&str> = get_body["favorites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["game_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Ocean World", "Ashen Realms"]);

    // The guild's public profile carries the same ordered favorites.
    let profile = http
        .get(format!("{base}/guilds/{guild_id}"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    let profile_body: serde_json::Value = profile.json().await.unwrap();
    let names: Vec<&str> = profile_body["favorite_games"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["game_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Ocean World", "Ashen Realms"]);

    // Unpinning: an empty list clears everything.
    let cleared = put_favorites(vec![]).await;
    assert!(cleared.status().is_success(), "{:?}", cleared.status());
    let body: serde_json::Value = cleared.json().await.unwrap();
    assert!(body["favorites"].as_array().unwrap().is_empty());
}

/// A 6th pin is rejected server-side even if the caller has real affinity
/// for all six games.
#[tokio::test]
#[ignore]
async fn a_sixth_pin_is_rejected_over_http() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;

    let create = auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let guild: serde_json::Value = create.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    let mut game_ids = Vec::new();
    for i in 0..6 {
        let game_id = seed_game(&pool, &format!("Game {i}")).await;
        seed_binding(&pool, owner_id, game_id).await;
        game_ids.push(game_id);
    }

    let resp = auth(
        http.put(format!("{base}/guilds/{guild_id}/favorite-games")),
        &owner_token,
    )
    .json(&serde_json::json!({ "game_ids": game_ids }))
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

/// #207's staleness invariant: when a pinned game's last bound member
/// unbinds, the pin is NOT auto-removed, but the read response flags it
/// `stale: true` so a `manage_guild` holder can choose to unpin it.
#[tokio::test]
#[ignore]
async fn a_stale_pin_is_flagged_but_not_auto_removed() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;

    let create = auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let guild: serde_json::Value = create.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    let ashen = seed_game(&pool, "Ashen Realms").await;
    let binding_id = seed_binding(&pool, owner_id, ashen).await;

    let pin = auth(
        http.put(format!("{base}/guilds/{guild_id}/favorite-games")),
        &owner_token,
    )
    .json(&serde_json::json!({ "game_ids": [ashen] }))
    .send()
    .await
    .unwrap();
    assert!(pin.status().is_success(), "{:?}", pin.status());

    // The owner's last (only) binding to Ashen Realms ends — the pin must
    // survive this untouched, just flagged stale.
    end_binding(&pool, binding_id).await;

    let get = auth(
        http.get(format!("{base}/guilds/{guild_id}/favorite-games")),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(get.status().is_success(), "{:?}", get.status());
    let body: serde_json::Value = get.json().await.unwrap();
    let favorites = body["favorites"].as_array().unwrap();
    assert_eq!(favorites.len(), 1, "the stale pin must not be auto-removed");
    assert!(favorites[0]["stale"].as_bool().unwrap());
}

// -- Issue #242: guild join requests -----------------------------------

async fn member_count(http: &reqwest::Client, base: &str, token: &str, guild_id: &str) -> usize {
    let members: serde_json::Value =
        auth(http.get(format!("{base}/guilds/{guild_id}/members")), token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    members.as_array().unwrap().len()
}

/// Applying to a `recruiting` guild succeeds and shows up for a manager's
/// `GET /guilds/{id}/join-requests` (pending by default).
#[tokio::test]
#[ignore]
async fn applying_to_a_recruiting_guild_succeeds_and_is_listed_pending() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_applicant_id, applicant_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    let apply = auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({ "message": "would love to join" }))
    .send()
    .await
    .unwrap();
    assert!(apply.status().is_success(), "{:?}", apply.status());
    let apply_body: serde_json::Value = apply.json().await.unwrap();
    assert_eq!(apply_body["status"].as_str().unwrap(), "pending");

    let pending: serde_json::Value = auth(
        http.get(format!("{base}/guilds/{guild_id}/join-requests")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let pending = pending.as_array().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0]["id"].as_str().unwrap(),
        apply_body["id"].as_str().unwrap()
    );
}

/// A non-recruiting guild rejects an apply outright — the ticket's own
/// gating, mirroring #154's discovery visibility rule.
#[tokio::test]
#[ignore]
async fn applying_to_a_non_recruiting_guild_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_applicant_id, applicant_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Closed Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    assert!(!guild["recruiting"].as_bool().unwrap());

    let apply = auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();
    assert_eq!(apply.status(), reqwest::StatusCode::FORBIDDEN);
}

/// A second apply while the first is still pending is idempotent — same
/// request id back, not a new row or an error, same posture as
/// `invite_accept_join_and_leave_flow`'s duplicate-invite check.
#[tokio::test]
#[ignore]
async fn duplicate_apply_while_pending_is_idempotent() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_applicant_id, applicant_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    let first = auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();
    assert!(first.status().is_success(), "{:?}", first.status());
    let first_body: serde_json::Value = first.json().await.unwrap();

    let second = auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();
    assert!(second.status().is_success(), "{:?}", second.status());
    let second_body: serde_json::Value = second.json().await.unwrap();
    assert_eq!(
        second_body["id"].as_str().unwrap(),
        first_body["id"].as_str().unwrap()
    );
}

/// Approving a pending request adds the applicant as a member through the
/// same path an accepted invite uses, and marks the request approved.
#[tokio::test]
#[ignore]
async fn approving_a_join_request_adds_membership() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (applicant_id, applicant_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    let apply = auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();
    let apply_body: serde_json::Value = apply.json().await.unwrap();
    let request_id = apply_body["id"].as_str().unwrap();

    assert_eq!(member_count(&http, &base, &owner_token, guild_id).await, 1);

    let approve = auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}/approve"
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(approve.status().is_success(), "{:?}", approve.status());
    let approve_body: serde_json::Value = approve.json().await.unwrap();
    assert_eq!(
        approve_body["identity_id"].as_str().unwrap(),
        applicant_id.to_string()
    );

    assert_eq!(member_count(&http, &base, &owner_token, guild_id).await, 2);

    let all: serde_json::Value = auth(
        http.get(format!("{base}/guilds/{guild_id}/join-requests?status=all")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let all = all.as_array().unwrap();
    assert_eq!(all[0]["status"].as_str().unwrap(), "approved");
}

/// Rejecting a pending request leaves membership unchanged.
#[tokio::test]
#[ignore]
async fn rejecting_a_join_request_leaves_membership_unchanged() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_applicant_id, applicant_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    let apply = auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();
    let apply_body: serde_json::Value = apply.json().await.unwrap();
    let request_id = apply_body["id"].as_str().unwrap();

    let reject = auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}/reject"
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(reject.status().is_success(), "{:?}", reject.status());

    assert_eq!(member_count(&http, &base, &owner_token, guild_id).await, 1);
}

/// The applicant withdrawing their own pending request leaves membership
/// unchanged, and a manager can no longer approve the withdrawn request.
#[tokio::test]
#[ignore]
async fn withdrawing_a_join_request_leaves_membership_unchanged() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_applicant_id, applicant_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    let apply = auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();
    let apply_body: serde_json::Value = apply.json().await.unwrap();
    let request_id = apply_body["id"].as_str().unwrap();

    let withdraw = auth(
        http.delete(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}"
        )),
        &applicant_token,
    )
    .send()
    .await
    .unwrap();
    assert!(withdraw.status().is_success(), "{:?}", withdraw.status());

    assert_eq!(member_count(&http, &base, &owner_token, guild_id).await, 1);

    let approve = auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}/approve"
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(approve.status(), reqwest::StatusCode::NOT_FOUND);
}

/// A non-manager cannot approve or reject someone else's join request.
#[tokio::test]
#[ignore]
async fn non_manager_cannot_approve_or_reject_a_join_request() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_applicant_id, applicant_token) = seed_identity_session(&pool).await;
    let (_bystander_id, bystander_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    let apply = auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();
    let apply_body: serde_json::Value = apply.json().await.unwrap();
    let request_id = apply_body["id"].as_str().unwrap();

    let approve = auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}/approve"
        )),
        &bystander_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(approve.status(), reqwest::StatusCode::FORBIDDEN);

    let reject = auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}/reject"
        )),
        &bystander_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(reject.status(), reqwest::StatusCode::FORBIDDEN);
}

/// Only the applicant themself can withdraw their own pending request —
/// not another identity, including the guild owner.
#[tokio::test]
#[ignore]
async fn only_the_applicant_can_withdraw_their_own_join_request() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_applicant_id, applicant_token) = seed_identity_session(&pool).await;
    let (_bystander_id, bystander_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    let apply = auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();
    let apply_body: serde_json::Value = apply.json().await.unwrap();
    let request_id = apply_body["id"].as_str().unwrap();

    let withdraw_by_bystander = auth(
        http.delete(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}"
        )),
        &bystander_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(
        withdraw_by_bystander.status(),
        reqwest::StatusCode::NOT_FOUND
    );

    let withdraw_by_owner = auth(
        http.delete(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}"
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(withdraw_by_owner.status(), reqwest::StatusCode::NOT_FOUND);

    // The request is still pending and can be withdrawn by the actual
    // applicant.
    let withdraw = auth(
        http.delete(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}"
        )),
        &applicant_token,
    )
    .send()
    .await
    .unwrap();
    assert!(withdraw.status().is_success(), "{:?}", withdraw.status());
}

/// `GET .../join-requests/mine` (issue #256) returns the applicant's own
/// pending request once one exists, and `null` beforehand — no
/// `manage_members` grant required either way, since it's the caller's own
/// data.
#[tokio::test]
#[ignore]
async fn my_join_request_returns_own_pending_request_or_none() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (applicant_id, applicant_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    // Before applying: no pending request of the applicant's own.
    let before = auth(
        http.get(format!("{base}/guilds/{guild_id}/join-requests/mine")),
        &applicant_token,
    )
    .send()
    .await
    .unwrap();
    assert!(before.status().is_success(), "{:?}", before.status());
    let before_body: serde_json::Value = before.json().await.unwrap();
    assert!(before_body.is_null());

    let apply = auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({ "message": "would love to join!" }))
    .send()
    .await
    .unwrap();
    let apply_body: serde_json::Value = apply.json().await.unwrap();
    let request_id = apply_body["id"].as_str().unwrap();

    // After applying: the applicant's own pending request comes back,
    // matching the same shape POST already returned.
    let after = auth(
        http.get(format!("{base}/guilds/{guild_id}/join-requests/mine")),
        &applicant_token,
    )
    .send()
    .await
    .unwrap();
    assert!(after.status().is_success(), "{:?}", after.status());
    let after_body: serde_json::Value = after.json().await.unwrap();
    assert_eq!(after_body["id"].as_str().unwrap(), request_id);
    assert_eq!(
        after_body["applicant"].as_str().unwrap(),
        applicant_id.to_string()
    );
    assert_eq!(after_body["status"].as_str().unwrap(), "pending");
    assert_eq!(after_body["message"].as_str().unwrap(), "would love to join!");
}

/// `GET .../join-requests/mine` never leaks another applicant's pending
/// request for the same guild — each caller only ever sees their own.
#[tokio::test]
#[ignore]
async fn my_join_request_never_returns_another_identitys_request() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_applicant_id, applicant_token) = seed_identity_session(&pool).await;
    let (_bystander_id, bystander_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();

    // The bystander never applied — `mine` reports none for them even
    // though the applicant has a pending request in this same guild.
    let bystander_mine = auth(
        http.get(format!("{base}/guilds/{guild_id}/join-requests/mine")),
        &bystander_token,
    )
    .send()
    .await
    .unwrap();
    assert!(
        bystander_mine.status().is_success(),
        "{:?}",
        bystander_mine.status()
    );
    let bystander_body: serde_json::Value = bystander_mine.json().await.unwrap();
    assert!(bystander_body.is_null());

    // The owner also has no pending request of their own here.
    let owner_mine = auth(
        http.get(format!("{base}/guilds/{guild_id}/join-requests/mine")),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(owner_mine.status().is_success(), "{:?}", owner_mine.status());
    let owner_body: serde_json::Value = owner_mine.json().await.unwrap();
    assert!(owner_body.is_null());
}

/// End-to-end: an applicant can discover their own pending request via
/// `mine`, then withdraw it via the existing `DELETE` route (issue #256's
/// motivation — the Hub previously had no way to reach this at all), and
/// `mine` reflects the withdrawal afterward.
#[tokio::test]
#[ignore]
async fn my_join_request_then_withdraw_is_reachable_end_to_end() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_applicant_id, applicant_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();

    let mine = auth(
        http.get(format!("{base}/guilds/{guild_id}/join-requests/mine")),
        &applicant_token,
    )
    .send()
    .await
    .unwrap();
    let mine_body: serde_json::Value = mine.json().await.unwrap();
    let request_id = mine_body["id"].as_str().unwrap().to_string();

    let withdraw = auth(
        http.delete(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}"
        )),
        &applicant_token,
    )
    .send()
    .await
    .unwrap();
    assert!(withdraw.status().is_success(), "{:?}", withdraw.status());

    let mine_after = auth(
        http.get(format!("{base}/guilds/{guild_id}/join-requests/mine")),
        &applicant_token,
    )
    .send()
    .await
    .unwrap();
    let mine_after_body: serde_json::Value = mine_after.json().await.unwrap();
    assert!(mine_after_body.is_null());
}
