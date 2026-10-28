//! Shared server URL, Postgres pool, and identity-session fixtures for live integration tests.
#![allow(dead_code)]

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::Client;
use passkey_types::ctap2::Aaguid;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

pub async fn test_pool() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

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
        .bind(format!("revoke-test-{identity_id}"))
        .execute(pool)
        .await
        .expect("failed to seed profile");
    let token = format!("test-token-{}", Uuid::new_v4());
    let expires_at = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    sqlx::query("INSERT INTO sessions (token_hash, identity_id, expires_at) VALUES (sha256(convert_to($1::text, 'UTF8')), $2, $3)")
        .bind(&token)
        .bind(identity_id)
        .bind(expires_at)
        .execute(pool)
        .await
        .expect("failed to seed session");
    (identity_id, token)
}

type VirtualClient = Client<
    MemoryStore,
    MockUserValidationMethod,
    passkey_crypto::AvailableBackend,
    public_suffix::PublicSuffixList,
    (),
>;

pub fn auth(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request.bearer_auth(token)
}

pub fn rp_origin() -> url::Url {
    let origin = std::env::var("AVALON_WEBAUTHN_ORIGIN")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    url::Url::parse(&origin).expect("AVALON_WEBAUTHN_ORIGIN must be a valid URL")
}

/// Mirrors `crate::signature_gate::canonical_message` byte-for-byte.
pub fn sign_connect(signing_key: &SigningKey, slug: &str, capabilities: &[&str]) -> String {
    let message = format!(
        "avalon:integration.connect:v1:{slug}:{}",
        capabilities.join(",")
    );
    BASE64.encode(signing_key.sign(message.as_bytes()).to_bytes())
}

/// Mirrors `crate::signature_gate::canonical_message` byte-for-byte.
pub fn sign_action(signing_key: &SigningKey, action_tag: &str, fields: &[&str]) -> String {
    let mut message = format!("avalon:{action_tag}:v1");
    for field in fields {
        message.push(':');
        message.push_str(field);
    }
    BASE64.encode(signing_key.sign(message.as_bytes()).to_bytes())
}

/// #697/#698: `POST /integrations/{slug}/connect` is signature-required
/// -- seeds a real signing key for `identity_id` so a connect call can
/// produce a genuine fresh signature over HTTP.
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
    (sqlx::Row::try_get(&row, "id").unwrap(), signing_key)
}

pub fn new_virtual_client() -> VirtualClient {
    let authenticator = Authenticator::new(
        Aaguid::new_empty(),
        MemoryStore::new(),
        MockUserValidationMethod::verified_user(2),
        passkey_crypto::AvailableBackend,
    );
    Client::new(authenticator).allows_insecure_localhost(true)
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub fn base_database_url() -> String {
    std::env::var("DATABASE_URL").expect("DATABASE_URL must be set")
}

pub fn migrations_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("db/migrations")
}
