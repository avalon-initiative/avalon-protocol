//! Post-compromise rollback against a real, running `avalon-server` and
//! Postgres. Gated `--ignored` — see `make test-live`.
//!
//! Identities, sessions and signing keys are seeded via SQL; the actions to
//! be reversed (friend accept/remove, guild join/leave) are driven through
//! the real HTTP handlers so the ledger rows are genuine. The completed
//! recovery is seeded directly since only its `completed_at` matters here.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use std::time::Duration;
use time::format_description::well_known::Rfc3339;
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
        .expect("failed to connect to Postgres")
}

struct Actor {
    id: Uuid,
    token: String,
    key_id: Uuid,
    key: SigningKey,
}

async fn seed_actor(pool: &PgPool) -> Actor {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
        .bind(id)
        .bind(format!("rollback-test-{id}"))
        .execute(pool)
        .await
        .unwrap();
    let token = format!("test-token-{}", Uuid::new_v4());
    sqlx::query("INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)")
        .bind(&token)
        .bind(id)
        .bind(OffsetDateTime::now_utc() + time::Duration::hours(1))
        .execute(pool)
        .await
        .unwrap();
    let key = SigningKey::generate(&mut rand::rng());
    let row = sqlx::query(
        "INSERT INTO identity_signing_keys (identity_id, public_key) VALUES ($1, $2) RETURNING id",
    )
    .bind(id)
    .bind(key.verifying_key().to_bytes().as_slice())
    .fetch_one(pool)
    .await
    .unwrap();
    Actor {
        id,
        token,
        key_id: row.try_get("id").unwrap(),
        key,
    }
}

fn sign_reverse(actor: &Actor, event_id: &str, since: &str) -> String {
    let message = format!("avalon:rollback.reverse:v1:{event_id}:{}:{since}", actor.id);
    BASE64.encode(actor.key.sign(message.as_bytes()).to_bytes())
}

async fn wait_for_outbox_drain(pool: &PgPool) {
    for _ in 0..150 {
        let status = avalon_server::outbox::status(pool).await.unwrap();
        if status.pending_count == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("outbox did not drain — is the outbox worker running?");
}

async fn create_open_guild(http: &reqwest::Client, base: &str, owner: &Actor) -> Uuid {
    let suffix = Uuid::new_v4().simple().to_string();
    let body: serde_json::Value = http
        .post(format!("{base}/guilds"))
        .bearer_auth(&owner.token)
        .json(&serde_json::json!({
            "name": format!("Rollback Guild {}", &suffix[..8]),
            "tag": suffix[..5].to_uppercase(),
            "description": "rollback test",
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let guild_id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    set_join_policy(http, base, owner, guild_id, "open").await;
    guild_id
}

async fn set_join_policy(
    http: &reqwest::Client,
    base: &str,
    owner: &Actor,
    guild_id: Uuid,
    policy: &str,
) {
    http.patch(format!("{base}/guilds/{guild_id}"))
        .bearer_auth(&owner.token)
        .json(&serde_json::json!({ "join_policy": policy }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
}

async fn befriend(http: &reqwest::Client, base: &str, requester: &Actor, acceptor: &Actor) {
    let request: serde_json::Value = http
        .post(format!("{base}/friends/requests"))
        .bearer_auth(&requester.token)
        .json(&serde_json::json!({ "to": acceptor.id }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let request_id = request["id"].as_str().unwrap();
    http.post(format!("{base}/friends/requests/{request_id}/accept"))
        .bearer_auth(&acceptor.token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
}

async fn candidates(
    http: &reqwest::Client,
    base: &str,
    owner: &Actor,
    since: &str,
) -> (u16, serde_json::Value) {
    let response = http
        .get(format!("{base}/me/rollback/candidates"))
        .query(&[("since", since)])
        .bearer_auth(&owner.token)
        .send()
        .await
        .unwrap();
    (
        response.status().as_u16(),
        response.json().await.unwrap_or_default(),
    )
}

async fn reverse(
    http: &reqwest::Client,
    base: &str,
    owner: &Actor,
    event_id: &str,
    since: &str,
    signed: bool,
) -> (u16, serde_json::Value) {
    let mut body = serde_json::json!({ "since": since });
    if signed {
        body["signing_key_id"] = serde_json::json!(owner.key_id);
        body["signature"] = serde_json::json!(sign_reverse(owner, event_id, since));
    }
    let response = http
        .post(format!("{base}/me/rollback/{event_id}/reverse"))
        .bearer_auth(&owner.token)
        .json(&body)
        .send()
        .await
        .unwrap();
    (
        response.status().as_u16(),
        response.json().await.unwrap_or_default(),
    )
}

fn of_kind<'a>(listing: &'a serde_json::Value, kind: &str) -> Vec<&'a serde_json::Value> {
    listing["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["kind"] == kind)
        .collect()
}

async fn insert_completed_recovery(pool: &PgPool, identity_id: Uuid) -> OffsetDateTime {
    let completed_at = OffsetDateTime::now_utc();
    sqlx::query(
        "INSERT INTO recovery_requests \
         (identity_id, pending_passkey_data, pending_credential_id, threshold_at_request, status, completed_at) \
         VALUES ($1, '{}'::jsonb, $2, 1, 'completed', $3)",
    )
    .bind(identity_id)
    .bind(Uuid::new_v4().as_bytes().as_slice())
    .bind(completed_at)
    .execute(pool)
    .await
    .unwrap();
    completed_at
}

async fn is_friend(pool: &PgPool, x: Uuid, y: Uuid) -> bool {
    let (a, b) = if x < y { (x, y) } else { (y, x) };
    sqlx::query("SELECT 1 FROM indexer_friendships WHERE a = $1 AND b = $2")
        .bind(a)
        .bind(b)
        .fetch_optional(pool)
        .await
        .unwrap()
        .is_some()
}

async fn role_index(pool: &PgPool, guild: Uuid, identity: Uuid) -> Option<i32> {
    sqlx::query(
        "SELECT role_index FROM indexer_guild_members WHERE guild_id = $1 AND identity_id = $2",
    )
    .bind(guild)
    .bind(identity)
    .fetch_optional(pool)
    .await
    .unwrap()
    .map(|r| r.get("role_index"))
}

#[tokio::test]
#[ignore]
async fn owner_reverses_attacker_additions_and_voluntary_leaves() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let owner = seed_actor(&pool).await;
    let other = seed_actor(&pool).await;
    let other_two = seed_actor(&pool).await;

    let before_attack = (OffsetDateTime::now_utc() - time::Duration::seconds(5))
        .format(&Rfc3339)
        .unwrap();

    befriend(&http, &base, &other, &owner).await;
    befriend(&http, &base, &other_two, &owner).await;
    http.delete(format!("{base}/friends/{}", other_two.id))
        .bearer_auth(&owner.token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let joined_guild = create_open_guild(&http, &base, &other).await;
    let left_guild = create_open_guild(&http, &base, &other).await;
    let invite_only_guild = create_open_guild(&http, &base, &other).await;
    for guild in [joined_guild, left_guild, invite_only_guild] {
        http.post(format!("{base}/guilds/{guild}/join"))
            .bearer_auth(&owner.token)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    for guild in [left_guild, invite_only_guild] {
        http.post(format!("{base}/guilds/{guild}/leave"))
            .bearer_auth(&owner.token)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    set_join_policy(&http, &base, &other, invite_only_guild, "invite_only").await;

    wait_for_outbox_drain(&pool).await;

    // No completed recovery yet: nothing is eligible.
    let (status, body) = candidates(&http, &base, &owner, &before_attack).await;
    assert_eq!(status, 409);
    assert_eq!(body["code"], "ROLLBACK_NO_COMPLETED_RECOVERY");

    let completed_at = insert_completed_recovery(&pool, owner.id).await;

    // An inverted window is a validation error, not an empty list.
    let after_recovery = (completed_at + time::Duration::seconds(1))
        .format(&Rfc3339)
        .unwrap();
    let (status, body) = candidates(&http, &base, &owner, &after_recovery).await;
    assert_eq!(status, 400);
    assert_eq!(body["code"], "INVALID_ROLLBACK_WINDOW");

    // A window that starts right before completion holds none of the events.
    let just_before = (completed_at - time::Duration::milliseconds(1))
        .format(&Rfc3339)
        .unwrap();
    let (status, body) = candidates(&http, &base, &owner, &just_before).await;
    assert_eq!(status, 200);
    assert!(body["candidates"].as_array().unwrap().is_empty());

    let (status, listing) = candidates(&http, &base, &owner, &before_attack).await;
    assert_eq!(status, 200, "{listing}");
    let accepted = of_kind(&listing, "friend.accepted");
    // Both friend.accepted events were authored by the owner as acceptor.
    assert_eq!(accepted.len(), 2);
    let accepted = accepted
        .into_iter()
        .find(|c| {
            c["summary"]
                .as_str()
                .unwrap()
                .contains(&other.id.to_string())
        })
        .unwrap();
    let removed_friend = of_kind(&listing, "friend.removed");
    assert_eq!(removed_friend.len(), 1);
    let removed_friend = removed_friend[0];
    assert_eq!(accepted["reversible"], true);
    assert_eq!(removed_friend["reversible"], false);
    assert!(removed_friend["reason"]
        .as_str()
        .unwrap()
        .contains("friend request"));

    let member_added = of_kind(&listing, "guild.member_added");
    assert_eq!(member_added.len(), 3);
    let member_removed = of_kind(&listing, "guild.member_removed");
    assert_eq!(member_removed.len(), 2);
    // One leave is restorable (open guild), one is not (now invite-only).
    assert_eq!(
        member_removed
            .iter()
            .filter(|c| c["reversible"] == true)
            .count(),
        1
    );
    assert!(member_removed.iter().any(|c| c["reason"]
        .as_str()
        .is_some_and(|r| r.contains("invite-only"))));

    // Signature is required.
    let accepted_id = accepted["event_id"].as_str().unwrap();
    let (status, body) = reverse(&http, &base, &owner, accepted_id, &before_attack, false).await;
    assert_eq!(status, 401);
    assert_eq!(body["code"], "FRESH_SIGNATURE_REQUIRED");

    // An event outside the window is not eligible.
    let (status, body) = reverse(&http, &base, &owner, accepted_id, &just_before, true).await;
    assert_eq!(status, 404);
    assert_eq!(body["code"], "ROLLBACK_EVENT_NOT_ELIGIBLE");

    // Unknown event id.
    let (status, _) = reverse(
        &http,
        &base,
        &owner,
        &Uuid::new_v4().to_string(),
        &before_attack,
        true,
    )
    .await;
    assert_eq!(status, 404);

    // friend.removed is never restorable.
    let (status, body) = reverse(
        &http,
        &base,
        &owner,
        removed_friend["event_id"].as_str().unwrap(),
        &before_attack,
        true,
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(body["code"], "ROLLBACK_NOT_REVERSIBLE");

    // Undo the friendship.
    assert!(is_friend(&pool, owner.id, other.id).await);
    let (status, body) = reverse(&http, &base, &owner, accepted_id, &before_attack, true).await;
    assert_eq!(status, 200, "{body}");
    assert!(body["reversal_event_id"].is_string());
    assert!(!is_friend(&pool, owner.id, other.id).await);

    // A second reversal of the same event is refused.
    let (status, body) = reverse(&http, &base, &owner, accepted_id, &before_attack, true).await;
    assert_eq!(status, 409);
    assert_eq!(body["code"], "ROLLBACK_ALREADY_REVERSED");

    // Undo a join.
    let joined_event = member_added
        .iter()
        .find(|c| {
            c["summary"]
                .as_str()
                .unwrap()
                .contains(&joined_guild.to_string())
        })
        .unwrap();
    assert_eq!(role_index(&pool, joined_guild, owner.id).await, Some(2));
    let (status, body) = reverse(
        &http,
        &base,
        &owner,
        joined_event["event_id"].as_str().unwrap(),
        &before_attack,
        true,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(role_index(&pool, joined_guild, owner.id).await, None);

    // Restore a voluntary leave into an open guild, at the default role.
    let restorable = member_removed
        .iter()
        .find(|c| c["reversible"] == true)
        .unwrap();
    assert!(restorable["summary"]
        .as_str()
        .unwrap()
        .contains(&left_guild.to_string()));
    assert_eq!(role_index(&pool, left_guild, owner.id).await, None);
    let (status, body) = reverse(
        &http,
        &base,
        &owner,
        restorable["event_id"].as_str().unwrap(),
        &before_attack,
        true,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(role_index(&pool, left_guild, owner.id).await, Some(2));

    // Invite-only leave stays refused.
    let invite_only_leave = member_removed
        .iter()
        .find(|c| c["reversible"] == false)
        .unwrap();
    let (status, body) = reverse(
        &http,
        &base,
        &owner,
        invite_only_leave["event_id"].as_str().unwrap(),
        &before_attack,
        true,
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(body["code"], "ROLLBACK_NOT_REVERSIBLE");

    // The reversed events now report as already reversed, and each
    // reversal is a durable ledger entry.
    wait_for_outbox_drain(&pool).await;
    let (_, relisted) = candidates(&http, &base, &owner, &before_attack).await;
    let reversed_ids: Vec<&str> = relisted["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["already_reversed"] == true)
        .map(|c| c["event_id"].as_str().unwrap())
        .collect();
    assert_eq!(reversed_ids.len(), 3);
    let reversal_entries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ledger_entries \
         WHERE kind IN ('guild.membership_reversed', 'friend.relationship_reversed') \
           AND payload->>'identity_id' = $1",
    )
    .bind(owner.id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(reversal_entries, 3);
}
