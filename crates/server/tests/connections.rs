//! Exercises the integrator-connect / capability grant / revoke flow (issues #27
//! and #83) against a real, running `avalon-server` and Postgres. Gated
//! `--ignored` since it needs live infra — see `make test-live` / `make
//! start`. Skipped in this sandbox per `.claude/CLAUDE.md` (no reachable
//! Postgres here); written but not run against a live database.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

/// #697/#698: seeds a real signing key for `identity_id` so a test can
/// produce a genuine fresh-signature over HTTP, same pattern
/// `crates/server/tests/device_grants.rs` already established.
async fn seed_signing_key(pool: &PgPool, identity_id: Uuid) -> (Uuid, SigningKey) {
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

/// Mirrors `crate::signature_gate::canonical_message` byte-for-byte.
fn sign_action(signing_key: &SigningKey, action_tag: &str, fields: &[&str]) -> String {
    let mut message = format!("avalon:{action_tag}:v1");
    for field in fields {
        message.push(':');
        message.push_str(field);
    }
    BASE64.encode(signing_key.sign(message.as_bytes()).to_bytes())
}

/// Signs a `POST /integrations/{slug}/connect` request for `identity_id`
/// requesting exactly `capabilities`, seeding a fresh signing key each
/// call — every `connect` in this file needs one now (#697/#698).
async fn connect_body(
    pool: &PgPool,
    identity_id: Uuid,
    slug: &str,
    capabilities: &[&str],
) -> serde_json::Value {
    let (signing_key_id, signing_key) = seed_signing_key(pool, identity_id).await;
    let signature = sign_action(
        &signing_key,
        "integration.connect",
        &[slug, &capabilities.join(",")],
    );
    serde_json::json!({
        "capabilities": capabilities,
        "signing_key_id": signing_key_id,
        "signature": signature,
    })
}

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

/// Same pattern `crates/server/tests/guilds.rs` established: seed a bare
/// identity + session directly via SQL rather than a real WebAuthn
/// ceremony — these endpoints only care that the bearer token is valid.
async fn seed_identity_session(pool: &PgPool) -> (Uuid, String) {
    let identity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(identity_id)
        .execute(pool)
        .await
        .expect("failed to seed identity");
    sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(format!("connections-test-{identity_id}"))
        .execute(pool)
        .await
        .expect("failed to seed profile");

    let token = format!("test-token-{}", Uuid::new_v4());
    let expires_at = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    sqlx::query("INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)")
        .bind(&token)
        .bind(identity_id)
        .bind(expires_at)
        .execute(pool)
        .await
        .expect("failed to seed session");

    (identity_id, token)
}

/// Registers a fresh integrator via the real `POST /integrations` endpoint (same
/// pattern `crates/server/tests/integrations.rs::unique_integrator` uses) declaring the
/// two capabilities these tests approve/reject against.
async fn register_unique_integrator(http: &reqwest::Client, base: &str) -> serde_json::Value {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let body = serde_json::json!({
        "slug": format!("test-conn-{}", &suffix[..12]),
        "name": format!("Connections Test Integrator {}", &suffix[..8]),
        "owner_name": "Test Studio",
        "requested_capabilities": ["presence.read", "friends.read"],
        "initial_key": {
            "algorithm": "ed25519",
            "public_key": BASE64.encode(signing_key.verifying_key().as_bytes()),
        },
    });

    let response = http
        .post(format!("{base}/integrations"))
        .json(&body)
        .send()
        .await
        .expect("register integrator failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());
    response.json().await.unwrap()
}

#[tokio::test]
#[ignore]
async fn connecting_creates_a_binding_and_grants_approved_capabilities() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (identity_id, token) = seed_identity_session(&pool).await;
    let integrator = register_unique_integrator(&http, &base).await;
    let slug = integrator["slug"].as_str().unwrap();

    let response = http
        .post(format!("{base}/integrations/{slug}/connect"))
        .bearer_auth(&token)
        .json(&connect_body(&pool, identity_id, slug, &["presence.read"]).await)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        body["granted_capabilities"].as_array().unwrap(),
        &[serde_json::json!("presence.read")]
    );

    let connections: serde_json::Value = http
        .get(format!("{base}/me/connections"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let connections = connections.as_array().unwrap();
    assert_eq!(connections.len(), 1);
    assert_eq!(connections[0]["slug"].as_str().unwrap(), slug);
    assert_eq!(connections[0]["grants"].as_array().unwrap().len(), 1);
}

#[tokio::test]
#[ignore]
async fn connecting_with_an_undeclared_capability_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (identity_id, token) = seed_identity_session(&pool).await;
    let integrator = register_unique_integrator(&http, &base).await;
    let slug = integrator["slug"].as_str().unwrap();

    let response = http
        .post(format!("{base}/integrations/{slug}/connect"))
        .bearer_auth(&token)
        .json(&connect_body(&pool, identity_id, slug, &["wallet.write"]).await)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore]
async fn reconnecting_does_not_duplicate_the_binding() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (identity_id, token) = seed_identity_session(&pool).await;
    let integrator = register_unique_integrator(&http, &base).await;
    let slug = integrator["slug"].as_str().unwrap();

    let first = http
        .post(format!("{base}/integrations/{slug}/connect"))
        .bearer_auth(&token)
        .json(&connect_body(&pool, identity_id, slug, &["presence.read"]).await)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();

    let second = http
        .post(format!("{base}/integrations/{slug}/connect"))
        .bearer_auth(&token)
        .json(&connect_body(&pool, identity_id, slug, &["friends.read"]).await)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();

    assert_eq!(
        first["binding_id"].as_str().unwrap(),
        second["binding_id"].as_str().unwrap()
    );
    assert_eq!(first["established_at"], second["established_at"]);

    let connections: serde_json::Value = http
        .get(format!("{base}/me/connections"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let connections = connections.as_array().unwrap();
    assert_eq!(
        connections.len(),
        1,
        "reconnecting must not duplicate the binding"
    );
    assert_eq!(connections[0]["grants"].as_array().unwrap().len(), 2);
}

/// The end-to-end proof called out in #27's own acceptance criteria:
/// connect → a capability-gated SDK method that requires that capability
/// succeeds → revoke → it fails with `CapabilityNotGranted`.
#[tokio::test]
#[ignore]
async fn revoking_a_grant_removes_sdk_access_to_the_gated_method() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (identity_id, token) = seed_identity_session(&pool).await;
    let integrator = register_unique_integrator(&http, &base).await;
    let slug = integrator["slug"].as_str().unwrap();
    let key_id = integrator["credential"]["key_id"]
        .as_str()
        .unwrap()
        .to_string();

    let connect = http
        .post(format!("{base}/integrations/{slug}/connect"))
        .bearer_auth(&token)
        .json(&connect_body(&pool, identity_id, slug, &["friends.read"]).await)
        .send()
        .await
        .unwrap();
    assert!(connect.status().is_success(), "{:?}", connect.status());

    let client = avalon_sdk::AvalonClient::new(avalon_sdk::AvalonConfig {
        server_url: base.clone(),
        integrator_credential_key_id: key_id,
        integrator_slug: None,
        signing_key: None,
        retry: Default::default(),
    });
    let session = client
        .authenticate(&token)
        .await
        .expect("authenticate should succeed");

    let friends_before = session.friends().await;
    assert!(
        friends_before.is_ok(),
        "friends.read was just granted, should be callable: {friends_before:?}"
    );

    let revoke = http
        .delete(format!("{base}/integrations/{slug}/grants/friends.read"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert!(revoke.status().is_success());

    let session_after = client
        .authenticate(&token)
        .await
        .expect("authenticate should succeed");
    let friends_after = session_after.friends().await;
    assert!(matches!(
        friends_after,
        Err(avalon_sdk::SdkError::CapabilityNotGranted(_))
    ));
}

#[tokio::test]
#[ignore]
async fn disconnecting_revokes_every_active_grant() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (identity_id, token) = seed_identity_session(&pool).await;
    let integrator = register_unique_integrator(&http, &base).await;
    let slug = integrator["slug"].as_str().unwrap();

    let connect = http
        .post(format!("{base}/integrations/{slug}/connect"))
        .bearer_auth(&token)
        .json(&connect_body(&pool, identity_id, slug, &["presence.read", "friends.read"]).await)
        .send()
        .await
        .unwrap();
    assert!(connect.status().is_success(), "{:?}", connect.status());

    let disconnect = http
        .delete(format!("{base}/integrations/{slug}/connect"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert!(disconnect.status().is_success());

    let connections: serde_json::Value = http
        .get(format!("{base}/me/connections"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        connections.as_array().unwrap().len(),
        0,
        "an ended binding must not appear in the active connections list"
    );

    // Reconnecting after ending must succeed (a partial-unique index, not a
    // plain unique constraint, backs `bindings`).
    let reconnect = http
        .post(format!("{base}/integrations/{slug}/connect"))
        .bearer_auth(&token)
        .json(&connect_body(&pool, identity_id, slug, &["presence.read"]).await)
        .send()
        .await
        .unwrap();
    assert!(reconnect.status().is_success());
}

/// #697/#698: `POST /integrations/{slug}/connect` is signature-required —
/// an ambient-session-only connect (caller has no registered signing key)
/// must be rejected, and no binding should be created.
#[tokio::test]
#[ignore]
async fn connecting_without_a_signature_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_identity_id, token) = seed_identity_session(&pool).await;
    let integrator = register_unique_integrator(&http, &base).await;
    let slug = integrator["slug"].as_str().unwrap();

    let response = http
        .post(format!("{base}/integrations/{slug}/connect"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "capabilities": ["presence.read"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["code"], "NO_REGISTERED_SIGNING_KEY");

    let connections: serde_json::Value = http
        .get(format!("{base}/me/connections"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(connections.as_array().unwrap().len(), 0);
}
