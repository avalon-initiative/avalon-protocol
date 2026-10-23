//! Exercises guild creation, uniqueness, ownership transfer, metadata, and session gates.
//! Role tests live in `guild_roles.rs`, membership tests in `guild_membership.rs`,
//! and discovery, integrator breakdown, and pin tests in `guild_discovery.rs`.
//! Requires a running `avalon-server` and Postgres; tests are `#[ignore]`.
//! See `make test-live` / `make start`.

use uuid::Uuid;

mod common;
mod guild_support;

use guild_support::{seed_identity_session, seed_signing_key, unique_guild_body};

#[tokio::test]
#[ignore]
async fn creating_a_guild_makes_the_creator_the_owner() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (owner_id, token) = seed_identity_session(&pool).await;

    let create = common::auth(http.post(format!("{base}/guilds")), &token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    assert!(create.status().is_success(), "{:?}", create.status());
    let body: serde_json::Value = create.json().await.unwrap();
    assert_eq!(body["owner"].as_str().unwrap(), owner_id.to_string());
    assert_eq!(body["member_count"].as_i64().unwrap(), 1);

    let roles: serde_json::Value = common::auth(
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
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_id, token) = seed_identity_session(&pool).await;

    let mut body = unique_guild_body();
    let first = common::auth(http.post(format!("{base}/guilds")), &token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success());

    // Same name, different case, different tag — should still collide on name.
    body["name"] = serde_json::json!(body["name"].as_str().unwrap().to_uppercase());
    body["tag"] = serde_json::json!(Uuid::new_v4().simple().to_string()[..4].to_uppercase());
    let second = common::auth(http.post(format!("{base}/guilds")), &token)
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
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_id, token) = seed_identity_session(&pool).await;

    let mut body = unique_guild_body();
    let first = common::auth(http.post(format!("{base}/guilds")), &token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success());

    // Same tag, different case, different name — should still collide on tag.
    body["tag"] = serde_json::json!(body["tag"].as_str().unwrap().to_lowercase());
    body["name"] = serde_json::json!(format!("Different Name {}", Uuid::new_v4()));
    let second = common::auth(http.post(format!("{base}/guilds")), &token)
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
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (new_owner_id, _new_owner_token) = seed_identity_session(&pool).await;

    let create = common::auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap();

    let (signing_key_id, signing_key) = seed_signing_key(&pool, owner_id).await;
    let signature = common::sign_action(
        &signing_key,
        "guild.transfer_ownership",
        &[guild_id, &owner_id.to_string(), &new_owner_id.to_string()],
    );
    let transfer = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/transfer-ownership")),
        &owner_token,
    )
    .json(&serde_json::json!({
        "to": new_owner_id,
        "signing_key_id": signing_key_id,
        "signature": signature,
    }))
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
        common::auth(http.get(format!("{base}/guilds/{guild_id}")), &owner_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(fetched["owner"].as_str().unwrap(), new_owner_id.to_string());
    assert_ne!(fetched["owner"].as_str().unwrap(), owner_id.to_string());

    // The old owner no longer has owner-only authority.
    let second_transfer = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/transfer-ownership")),
        &owner_token,
    )
    .json(&serde_json::json!({ "to": owner_id }))
    .send()
    .await
    .unwrap();
    assert_eq!(second_transfer.status(), reqwest::StatusCode::FORBIDDEN);
}

/// Issue #704's gap #2: an ambient-session-only transfer (no
/// `signing_key_id`/`signature`, owner has no registered signing key) must
/// be rejected — a stolen bearer token alone must not be able to hand off
/// permanent guild ownership. `guilds.owner` must stay unchanged.
#[tokio::test]
#[ignore]
async fn transferring_ownership_without_a_signature_is_rejected() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (new_owner_id, _new_owner_token) = seed_identity_session(&pool).await;

    let create = common::auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap();

    let transfer = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/transfer-ownership")),
        &owner_token,
    )
    .json(&serde_json::json!({ "to": new_owner_id }))
    .send()
    .await
    .unwrap();
    assert_eq!(transfer.status(), reqwest::StatusCode::CONFLICT);
    let transfer_body: serde_json::Value = transfer.json().await.unwrap();
    assert_eq!(transfer_body["code"], "NO_REGISTERED_SIGNING_KEY");

    let fetched: serde_json::Value =
        common::auth(http.get(format!("{base}/guilds/{guild_id}")), &owner_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(fetched["owner"].as_str().unwrap(), owner_id.to_string());
}

#[tokio::test]
#[ignore]
async fn guild_endpoints_require_a_session_token() {
    let http = reqwest::Client::new();
    let base = common::server_url();

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

/// Issue #153: setting motd/banner/links/recruiting via `PATCH
/// /guilds/{id}` round-trips through `GET /guilds/{id}`.
#[tokio::test]
#[ignore]
async fn guild_metadata_round_trips_through_patch_and_get() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, token) = seed_identity_session(&pool).await;

    let create_guild = common::auth(http.post(format!("{base}/guilds")), &token)
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

    let patch = common::auth(http.patch(format!("{base}/guilds/{guild_id}")), &token)
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

    let fetched: serde_json::Value =
        common::auth(http.get(format!("{base}/guilds/{guild_id}")), &token)
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
    let clear = common::auth(http.patch(format!("{base}/guilds/{guild_id}")), &token)
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

    let overlong_motd = common::auth(http.patch(format!("{base}/guilds/{guild_id}")), &token)
        .json(&serde_json::json!({ "motd": "a".repeat(501) }))
        .send()
        .await
        .unwrap();
    assert_eq!(overlong_motd.status(), reqwest::StatusCode::BAD_REQUEST);

    let bad_banner = common::auth(http.patch(format!("{base}/guilds/{guild_id}")), &token)
        .json(&serde_json::json!({ "banner": "javascript:alert(1)" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_banner.status(), reqwest::StatusCode::BAD_REQUEST);

    let too_many_links: Vec<serde_json::Value> = (0..6)
        .map(|i| serde_json::json!({ "label": format!("Link {i}"), "url": "https://example.com" }))
        .collect();
    let bad_links = common::auth(http.patch(format!("{base}/guilds/{guild_id}")), &token)
        .json(&serde_json::json!({ "links": too_many_links }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_links.status(), reqwest::StatusCode::BAD_REQUEST);
}
