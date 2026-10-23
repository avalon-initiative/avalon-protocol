//! Exercises guild role creation, updates, deletion, and authorization.
//! Requires a running `avalon-server` and Postgres; tests are `#[ignore]`.
//! See `make test-live` / `make start`.

mod common;
mod guild_support;

use guild_support::{seed_identity_session, seed_signing_key, unique_guild_body};

/// Issue #152: creating a role with a description/badge round-trips
/// through `GET /guilds/{id}/roles`.
#[tokio::test]
#[ignore]
async fn creating_a_role_with_description_and_badge_round_trips() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (owner_id, token) = seed_identity_session(&pool).await;

    let create_guild = common::auth(http.post(format!("{base}/guilds")), &token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    assert!(create_guild.status().is_success());
    let guild: serde_json::Value = create_guild.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    let (signing_key_id, signing_key) = seed_signing_key(&pool, owner_id).await;
    let signature = common::sign_action(
        &signing_key,
        "guild.role.create",
        &[guild_id, "Raid Leader", "manage_members"],
    );
    let create_role = common::auth(http.post(format!("{base}/guilds/{guild_id}/roles")), &token)
        .json(&serde_json::json!({
            "name": "Raid Leader",
            "permissions": ["manage_members"],
            "description": "Leads raid nights and manages the roster.",
            "badge": { "icon": "sword", "color": "purple" },
            "signing_key_id": signing_key_id,
            "signature": signature,
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
        common::auth(http.get(format!("{base}/guilds/{guild_id}/roles")), &token)
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
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (owner_id, token) = seed_identity_session(&pool).await;

    let create_guild = common::auth(http.post(format!("{base}/guilds")), &token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    let guild: serde_json::Value = create_guild.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    let (signing_key_id, signing_key) = seed_signing_key(&pool, owner_id).await;
    let signature =
        common::sign_action(&signing_key, "guild.role.create", &[guild_id, "Bogus", ""]);
    let create_role = common::auth(http.post(format!("{base}/guilds/{guild_id}/roles")), &token)
        .json(&serde_json::json!({
            "name": "Bogus",
            "badge": { "icon": "not_a_real_icon", "color": "gold" },
            "signing_key_id": signing_key_id,
            "signature": signature,
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
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (owner_id, token) = seed_identity_session(&pool).await;

    let create_guild = common::auth(http.post(format!("{base}/guilds")), &token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    let guild: serde_json::Value = create_guild.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    let (signing_key_id, signing_key) = seed_signing_key(&pool, owner_id).await;
    let first_signature = common::sign_action(
        &signing_key,
        "guild.role.create",
        &[guild_id, "Raid Leader", ""],
    );
    let first = common::auth(http.post(format!("{base}/guilds/{guild_id}/roles")), &token)
        .json(&serde_json::json!({
            "name": "Raid Leader",
            "signing_key_id": signing_key_id,
            "signature": first_signature,
        }))
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success());

    let duplicate_signature = common::sign_action(
        &signing_key,
        "guild.role.create",
        &[guild_id, "raid leader", ""],
    );
    let duplicate = common::auth(http.post(format!("{base}/guilds/{guild_id}/roles")), &token)
        .json(&serde_json::json!({
            "name": "raid leader",
            "signing_key_id": signing_key_id,
            "signature": duplicate_signature,
        }))
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
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (owner_id, token) = seed_identity_session(&pool).await;

    let create_guild = common::auth(http.post(format!("{base}/guilds")), &token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    let guild: serde_json::Value = create_guild.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    let (signing_key_id, signing_key) = seed_signing_key(&pool, owner_id).await;
    let signature = common::sign_action(&signing_key, "guild.role.update", &[guild_id, "0"]);
    let rename = common::auth(
        http.patch(format!("{base}/guilds/{guild_id}/roles/0")),
        &token,
    )
    .json(&serde_json::json!({
        "name": "Guild Master",
        "signing_key_id": signing_key_id,
        "signature": signature,
    }))
    .send()
    .await
    .unwrap();
    assert!(rename.status().is_success(), "{:?}", rename.status());
    let renamed: serde_json::Value = rename.json().await.unwrap();
    assert_eq!(renamed["name"].as_str().unwrap(), "Guild Master");

    let signature = common::sign_action(&signing_key, "guild.role.update", &[guild_id, "0"]);
    let change_permissions = common::auth(
        http.patch(format!("{base}/guilds/{guild_id}/roles/0")),
        &token,
    )
    .json(&serde_json::json!({
        "permissions": ["manage_guild"],
        "signing_key_id": signing_key_id,
        "signature": signature,
    }))
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
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (owner_id, token) = seed_identity_session(&pool).await;

    let create_guild = common::auth(http.post(format!("{base}/guilds")), &token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    let guild: serde_json::Value = create_guild.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    let (signing_key_id, signing_key) = seed_signing_key(&pool, owner_id).await;

    // Base roles: owner (0) and member (2) can never be deleted.
    let signature = common::sign_action(&signing_key, "guild.role.delete", &[guild_id, "0"]);
    let delete_owner = common::auth(
        http.delete(format!("{base}/guilds/{guild_id}/roles/0")),
        &token,
    )
    .json(&serde_json::json!({ "signing_key_id": signing_key_id, "signature": signature }))
    .send()
    .await
    .unwrap();
    assert_eq!(delete_owner.status(), reqwest::StatusCode::FORBIDDEN);

    let signature = common::sign_action(&signing_key, "guild.role.delete", &[guild_id, "2"]);
    let delete_member = common::auth(
        http.delete(format!("{base}/guilds/{guild_id}/roles/2")),
        &token,
    )
    .json(&serde_json::json!({ "signing_key_id": signing_key_id, "signature": signature }))
    .send()
    .await
    .unwrap();
    assert_eq!(delete_member.status(), reqwest::StatusCode::FORBIDDEN);

    // A freshly created, unassigned custom role deletes cleanly.
    let create_signature = common::sign_action(
        &signing_key,
        "guild.role.create",
        &[guild_id, "Temp Role", ""],
    );
    let create_role = common::auth(http.post(format!("{base}/guilds/{guild_id}/roles")), &token)
        .json(&serde_json::json!({
            "name": "Temp Role",
            "signing_key_id": signing_key_id,
            "signature": create_signature,
        }))
        .send()
        .await
        .unwrap();
    let role: serde_json::Value = create_role.json().await.unwrap();
    let name_index = role["name_index"].as_i64().unwrap();

    let delete_signature = common::sign_action(
        &signing_key,
        "guild.role.delete",
        &[guild_id, &name_index.to_string()],
    );
    let delete = common::auth(
        http.delete(format!("{base}/guilds/{guild_id}/roles/{name_index}")),
        &token,
    )
    .json(&serde_json::json!({
        "signing_key_id": signing_key_id,
        "signature": delete_signature,
    }))
    .send()
    .await
    .unwrap();
    assert!(delete.status().is_success(), "{:?}", delete.status());

    let roles: serde_json::Value =
        common::auth(http.get(format!("{base}/guilds/{guild_id}/roles")), &token)
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
/// own explicit application check (the old `guild_members.role_index -> guild_roles.name_index` foreign key no longer applies)
/// catches this, mapped to a clean 409 rather than a raw DB error.
#[tokio::test]
#[ignore]
async fn deleting_a_role_still_held_by_a_member_is_rejected() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (other_id, other_token) = seed_identity_session(&pool).await;

    let create_guild = common::auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    let guild: serde_json::Value = create_guild.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    let (signing_key_id, signing_key) = seed_signing_key(&pool, owner_id).await;
    let create_signature = common::sign_action(
        &signing_key,
        "guild.role.create",
        &[guild_id, "Quartermaster", ""],
    );
    let create_role = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/roles")),
        &owner_token,
    )
    .json(&serde_json::json!({
        "name": "Quartermaster",
        "signing_key_id": signing_key_id,
        "signature": create_signature,
    }))
    .send()
    .await
    .unwrap();
    let role: serde_json::Value = create_role.json().await.unwrap();
    let name_index = role["name_index"].as_i64().unwrap();

    let open = common::auth(
        http.patch(format!("{base}/guilds/{guild_id}")),
        &owner_token,
    )
    .json(&serde_json::json!({ "join_policy": "open" }))
    .send()
    .await
    .unwrap();
    assert!(open.status().is_success());

    let join = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/join")),
        &other_token,
    )
    .send()
    .await
    .unwrap();
    assert!(join.status().is_success());

    let assign = common::auth(
        http.patch(format!("{base}/guilds/{guild_id}/members/{other_id}")),
        &owner_token,
    )
    .json(&serde_json::json!({ "role_index": name_index }))
    .send()
    .await
    .unwrap();
    assert!(assign.status().is_success(), "{:?}", assign.status());

    let delete_signature = common::sign_action(
        &signing_key,
        "guild.role.delete",
        &[guild_id, &name_index.to_string()],
    );
    let delete = common::auth(
        http.delete(format!("{base}/guilds/{guild_id}/roles/{name_index}")),
        &owner_token,
    )
    .json(&serde_json::json!({
        "signing_key_id": signing_key_id,
        "signature": delete_signature,
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(delete.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test]
#[ignore]
async fn a_role_change_by_a_non_manager_is_rejected() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_other_id, other_token) = seed_identity_session(&pool).await;

    let create = common::auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap();

    // Index 1 is "officer" — see the starter-role ordering asserted in
    // `creating_a_guild_makes_the_creator_the_owner`.
    let update = common::auth(
        http.patch(format!("{base}/guilds/{guild_id}/roles/1")),
        &other_token,
    )
    .json(&serde_json::json!({ "name": "hijacked" }))
    .send()
    .await
    .unwrap();
    assert_eq!(update.status(), reqwest::StatusCode::FORBIDDEN);
}

/// #697/#698: `POST /guilds/{id}/roles` is signature-required — an
/// ambient-session-only create (owner has no registered signing key) must
/// be rejected, and no role should be created.
#[tokio::test]
#[ignore]
async fn creating_a_role_without_a_signature_is_rejected() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, token) = seed_identity_session(&pool).await;

    let create_guild = common::auth(http.post(format!("{base}/guilds")), &token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    let guild: serde_json::Value = create_guild.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    let create_role = common::auth(http.post(format!("{base}/guilds/{guild_id}/roles")), &token)
        .json(&serde_json::json!({ "name": "Unsigned Role" }))
        .send()
        .await
        .unwrap();
    assert_eq!(create_role.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = create_role.json().await.unwrap();
    assert_eq!(body["code"], "NO_REGISTERED_SIGNING_KEY");

    let roles: serde_json::Value =
        common::auth(http.get(format!("{base}/guilds/{guild_id}/roles")), &token)
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
        .all(|r| r["name"].as_str().unwrap() != "Unsigned Role"));
}
