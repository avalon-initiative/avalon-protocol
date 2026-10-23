//! Shared identity, signing, and guild setup helpers for the guild integration tests.
#![allow(dead_code)]

use crate::common;
use ed25519_dalek::SigningKey;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

/// #697/#698: seeds a real signing key for `identity_id` so a test can
/// produce a genuine fresh-signature over HTTP, same pattern
/// `crates/server/tests/device_grants.rs` already established.
pub async fn seed_signing_key(
    pool: &PgPool,
    identity_id: avalon_protocol::ids::IdentityId,
) -> (Uuid, SigningKey) {
    let signing_key = SigningKey::generate(&mut rand::rng());
    let public_key = signing_key.verifying_key().to_bytes();
    let row = sqlx::query(
        "INSERT INTO identity_signing_keys (identity_id, public_key) VALUES ($1, $2) RETURNING id",
    )
    .bind(identity_id)
    .bind(public_key.as_slice())
    .fetch_one(pool)
    .await
    .expect("failed to seed signing key");
    let key_id: Uuid = sqlx::Row::try_get(&row, "id").unwrap();
    (key_id, signing_key)
}

/// Seeds a bare identity + session, bypassing WebAuthn entirely, and
/// returns the session's bearer token.
pub async fn seed_identity_session(pool: &PgPool) -> (avalon_protocol::ids::IdentityId, String) {
    let who = avalon_protocol::identity_id::TestIdentity::new();
    let identity_id = who.id;
    sqlx::query("INSERT INTO identities (id, inception_public_key) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(who.public_key().to_vec())
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
    sqlx::query("INSERT INTO sessions (token_hash, identity_id, expires_at) VALUES (sha256(convert_to($1::text, 'UTF8')), $2, $3)")
        .bind(&token)
        .bind(identity_id)
        .bind(expires_at)
        .execute(pool)
        .await
        .expect("failed to seed session");

    (identity_id, token)
}

/// A short random tag/name pair so concurrent test runs never collide on
/// the case-insensitive uniqueness constraints.
pub fn unique_guild_body() -> serde_json::Value {
    let suffix = Uuid::new_v4().simple().to_string();
    serde_json::json!({
        "name": format!("Test Guild {}", &suffix[..8]),
        "tag": suffix[..5].to_uppercase(),
        "description": "a guild created by an integration test",
    })
}

pub async fn create_guild(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    name_hint: &str,
) -> serde_json::Value {
    let suffix = Uuid::new_v4().simple().to_string();
    let body = serde_json::json!({
        "name": format!("{name_hint} {}", &suffix[..8]),
        "tag": suffix[..5].to_uppercase(),
        "description": format!("a discover-test guild ({name_hint})"),
    });
    let create = common::auth(http.post(format!("{base}/guilds")), token)
        .json(&body)
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    assert!(create.status().is_success(), "{:?}", create.status());
    create.json().await.unwrap()
}

pub async fn set_recruiting(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    guild_id: &str,
    recruiting: bool,
) {
    let patch = common::auth(http.patch(format!("{base}/guilds/{guild_id}")), token)
        .json(&serde_json::json!({ "recruiting": recruiting }))
        .send()
        .await
        .unwrap();
    assert!(patch.status().is_success(), "{:?}", patch.status());
}
