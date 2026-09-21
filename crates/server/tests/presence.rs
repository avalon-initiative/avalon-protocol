//! Exercises the presence publish/read flow (issue #16) against a real,
//! running `avalon-server`. Gated `--ignored` since it needs live infra —
//! see `make test-live` / `make start`. Skipped in this sandbox per
//! `.claude/CLAUDE.md` (no reachable Postgres here); written but not run
//! against a live database, same posture as every other `--ignored` file
//! in this directory.
//!
//! Expiry is exercised by starting the server with a short
//! `AVALON_PRESENCE_TTL_SECS` rather than sleeping the real 120s default —
//! set it before `make start` when running this file specifically, e.g.
//! `AVALON_PRESENCE_TTL_SECS=1 make start`. Without it, the expiry test
//! below is skipped rather than sleeping two real minutes.
//!
//! Test identities are seeded directly via SQL rather than through a real
//! WebAuthn ceremony — same approach as `crates/server/tests/friends.rs`,
//! since presence doesn't care how a session was established. The
//! integrator-side tests below reuse `crates/server/tests/connections.rs` and
//! `crates/server/tests/integrations.rs`'s own patterns for registering an integrator
//! and completing its challenge-response auth over real HTTP, since
//! `PUT /presence/:identity_id` is authenticated the same way
//! `crate::authz::authenticate_caller` authenticates any `Caller::Integrator`.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
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
        .bind(format!("presence-test-{identity_id}"))
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

/// #697/#698: `POST /integrations/{slug}/connect` is signature-required —
/// seeds a real signing key for `identity_id` so a connect call can
/// produce a genuine fresh signature over HTTP.
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
    (sqlx::Row::try_get(&row, "id").unwrap(), signing_key)
}

/// Mirrors `crate::signature_gate::canonical_message` byte-for-byte.
fn sign_connect(signing_key: &SigningKey, slug: &str, capabilities: &[&str]) -> String {
    let message = format!(
        "avalon:integration.connect:v1:{slug}:{}",
        capabilities.join(",")
    );
    BASE64.encode(signing_key.sign(message.as_bytes()).to_bytes())
}

fn auth(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request.bearer_auth(token)
}

#[tokio::test]
#[ignore]
async fn publishing_presence_is_visible_via_get() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (alice_id, alice_token) = seed_identity_session(&pool).await;

    let publish = auth(http.put(format!("{base}/me/presence")), &alice_token)
        .json(&serde_json::json!({ "status": "Online" }))
        .send()
        .await
        .expect("publish presence failed — is `make start` running?");
    assert!(publish.status().is_success(), "{:?}", publish.status());

    let read: serde_json::Value = auth(
        http.get(format!("{base}/presence?ids={alice_id}")),
        &alice_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let entries = read.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["status"], "Online");
    assert_eq!(entries[0]["active_in"], serde_json::Value::Null);
}

#[tokio::test]
#[ignore]
async fn a_user_cannot_claim_to_be_playing_a_integrator() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (alice_id, alice_token) = seed_identity_session(&pool).await;

    // The request body has no `active_in` field at all in the wire format —
    // this is enforced by `UpdatePresenceRequest` only ever deserializing
    // `status`, not by rejecting an extra field, so this just confirms the
    // response never echoes an `active_in` value regardless of what's sent.
    auth(http.put(format!("{base}/me/presence")), &alice_token)
        .json(&serde_json::json!({ "status": "Online", "active_in": Uuid::new_v4() }))
        .send()
        .await
        .unwrap();

    let read: serde_json::Value = auth(
        http.get(format!("{base}/presence?ids={alice_id}")),
        &alice_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(read[0]["active_in"], serde_json::Value::Null);
}

#[tokio::test]
#[ignore]
async fn missing_presence_reads_as_offline() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (_alice_id, alice_token) = seed_identity_session(&pool).await;
    let never_published = Uuid::new_v4();

    let read: serde_json::Value = auth(
        http.get(format!("{base}/presence?ids={never_published}")),
        &alice_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(read[0]["status"], "Offline");
}

#[tokio::test]
#[ignore]
async fn stale_presence_expires_to_offline() {
    let Ok(ttl) = std::env::var("AVALON_PRESENCE_TTL_SECS") else {
        eprintln!(
            "skipping: set AVALON_PRESENCE_TTL_SECS (e.g. 1) before `make start` to run this test"
        );
        return;
    };
    let ttl_secs: u64 = ttl
        .parse()
        .expect("AVALON_PRESENCE_TTL_SECS must be a number");

    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (alice_id, alice_token) = seed_identity_session(&pool).await;

    auth(http.put(format!("{base}/me/presence")), &alice_token)
        .json(&serde_json::json!({ "status": "Online" }))
        .send()
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_secs(ttl_secs + 1)).await;

    let read: serde_json::Value = auth(
        http.get(format!("{base}/presence?ids={alice_id}")),
        &alice_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(read[0]["status"], "Offline");
}

/// Sticky manual overrides (`Away`/`DoNotDisturb`/`Offline`) must survive
/// past the TTL that would otherwise expire a plain `Online` publish to
/// `Offline` — see `crates/server/src/presence.rs::PresenceStore::get`.
/// Needs a short `AVALON_PRESENCE_TTL_SECS` for the same reason
/// `stale_presence_expires_to_offline` does.
async fn assert_status_sticks_past_ttl(status: &str) {
    let Ok(ttl) = std::env::var("AVALON_PRESENCE_TTL_SECS") else {
        eprintln!(
            "skipping: set AVALON_PRESENCE_TTL_SECS (e.g. 1) before `make start` to run this test"
        );
        return;
    };
    let ttl_secs: u64 = ttl
        .parse()
        .expect("AVALON_PRESENCE_TTL_SECS must be a number");

    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (alice_id, alice_token) = seed_identity_session(&pool).await;

    auth(http.put(format!("{base}/me/presence")), &alice_token)
        .json(&serde_json::json!({ "status": status }))
        .send()
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_secs(ttl_secs + 1)).await;

    let read: serde_json::Value = auth(
        http.get(format!("{base}/presence?ids={alice_id}")),
        &alice_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(read[0]["status"], status);
}

#[tokio::test]
#[ignore]
async fn sticky_away_survives_past_the_ttl() {
    assert_status_sticks_past_ttl("Away").await;
}

#[tokio::test]
#[ignore]
async fn sticky_do_not_disturb_survives_past_the_ttl() {
    assert_status_sticks_past_ttl("DoNotDisturb").await;
}

#[tokio::test]
#[ignore]
async fn sticky_offline_survives_past_the_ttl() {
    assert_status_sticks_past_ttl("Offline").await;
}

#[tokio::test]
#[ignore]
async fn explicitly_setting_online_clears_a_sticky_override_and_resumes_ttl_tracking() {
    let Ok(ttl) = std::env::var("AVALON_PRESENCE_TTL_SECS") else {
        eprintln!(
            "skipping: set AVALON_PRESENCE_TTL_SECS (e.g. 1) before `make start` to run this test"
        );
        return;
    };
    let ttl_secs: u64 = ttl
        .parse()
        .expect("AVALON_PRESENCE_TTL_SECS must be a number");

    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (alice_id, alice_token) = seed_identity_session(&pool).await;

    auth(http.put(format!("{base}/me/presence")), &alice_token)
        .json(&serde_json::json!({ "status": "DoNotDisturb" }))
        .send()
        .await
        .unwrap();

    // Clearing the override: explicitly back to Online.
    auth(http.put(format!("{base}/me/presence")), &alice_token)
        .json(&serde_json::json!({ "status": "Online" }))
        .send()
        .await
        .unwrap();

    let read: serde_json::Value = auth(
        http.get(format!("{base}/presence?ids={alice_id}")),
        &alice_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(read[0]["status"], "Online");

    // Now automatic TTL tracking governs it again.
    tokio::time::sleep(std::time::Duration::from_secs(ttl_secs + 1)).await;
    let read: serde_json::Value = auth(
        http.get(format!("{base}/presence?ids={alice_id}")),
        &alice_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(read[0]["status"], "Offline");
}

/// Registers a fresh integrator via the real `POST /integrations` endpoint declaring
/// `presence.publish` — same pattern
/// `crates/server/tests/connections.rs::register_unique_integrator` uses.
async fn register_unique_integrator(
    http: &reqwest::Client,
    base: &str,
) -> (serde_json::Value, SigningKey) {
    let suffix = Uuid::new_v4().simple().to_string();
    let signing_key = SigningKey::generate(&mut rand::rng());
    let body = serde_json::json!({
        "slug": format!("presence-test-{}", &suffix[..12]),
        "name": format!("Presence Test Integrator {}", &suffix[..8]),
        "owner_name": "Test Studio",
        "requested_capabilities": ["presence.publish"],
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
    (response.json().await.unwrap(), signing_key)
}

/// Completes one round of the challenge-response integrator-auth handshake
/// (`crate::integrators::authenticate_integrator`) and returns a request builder
/// carrying every header `crate::authz::authenticate_caller` needs to
/// resolve a `Caller::Integrator { integrator_id, identity_id }` — the three
/// `x-avalon-integrator-*` headers plus `x-avalon-identity-id`, same shape
/// `crates/server/tests/integrations.rs`'s own round-trip test builds by hand.
async fn integrator_auth_request(
    http: &reqwest::Client,
    base: &str,
    slug: &str,
    key_id: &str,
    signing_key: &SigningKey,
    identity_id: Uuid,
    request: reqwest::RequestBuilder,
) -> reqwest::RequestBuilder {
    let challenge: serde_json::Value = http
        .post(format!("{base}/integrations/{slug}/challenge"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap().to_string();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let signature = signing_key.sign(&nonce);

    request
        .header("x-avalon-integrator-key-id", key_id)
        .header("x-avalon-integrator-challenge-id", challenge_id)
        .header(
            "x-avalon-integrator-signature",
            BASE64.encode(signature.to_bytes()),
        )
        .header("x-avalon-identity-id", identity_id.to_string())
}

#[tokio::test]
#[ignore]
async fn a_bound_integrator_can_publish_its_own_playing_claim() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (alice_id, alice_token) = seed_identity_session(&pool).await;
    let (alice_signing_key_id, alice_signing_key) = seed_signing_key(&pool, alice_id).await;
    let (integrator, signing_key) = register_unique_integrator(&http, &base).await;
    let slug = integrator["slug"].as_str().unwrap();
    let integrator_id = integrator["id"].as_str().unwrap();
    let key_id = integrator["credential"]["key_id"].as_str().unwrap();

    let connect_capabilities = ["presence.publish"];
    auth(
        http.post(format!("{base}/integrations/{slug}/connect")),
        &alice_token,
    )
    .json(&serde_json::json!({
        "capabilities": connect_capabilities,
        "signing_key_id": alice_signing_key_id,
        "signature": sign_connect(&alice_signing_key, slug, &connect_capabilities),
    }))
    .send()
    .await
    .unwrap();

    let request = integrator_auth_request(
        &http,
        &base,
        slug,
        key_id,
        &signing_key,
        alice_id,
        http.put(format!("{base}/presence/{alice_id}")),
    )
    .await;
    let response = request
        .json(&serde_json::json!({ "status": "Online", "active_in": integrator_id }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());

    let read: serde_json::Value = auth(
        http.get(format!("{base}/presence?ids={alice_id}")),
        &alice_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(read[0]["status"], "Online");
    assert_eq!(read[0]["active_in"].as_str().unwrap(), integrator_id);
}

/// The ticket's own explicit ask: an integrator publishing `active_in` for an integrator
/// id that isn't its own — even one it's otherwise fully bound and
/// granted against — is rejected.
#[tokio::test]
#[ignore]
async fn a_integrator_cannot_claim_to_be_playing_a_different_integrator() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (alice_id, alice_token) = seed_identity_session(&pool).await;
    let (alice_signing_key_id, alice_signing_key) = seed_signing_key(&pool, alice_id).await;
    let (integrator, signing_key) = register_unique_integrator(&http, &base).await;
    let slug = integrator["slug"].as_str().unwrap();
    let key_id = integrator["credential"]["key_id"].as_str().unwrap();

    let connect_capabilities = ["presence.publish"];
    auth(
        http.post(format!("{base}/integrations/{slug}/connect")),
        &alice_token,
    )
    .json(&serde_json::json!({
        "capabilities": connect_capabilities,
        "signing_key_id": alice_signing_key_id,
        "signature": sign_connect(&alice_signing_key, slug, &connect_capabilities),
    }))
    .send()
    .await
    .unwrap();

    let some_other_integrator_id = Uuid::new_v4();
    let request = integrator_auth_request(
        &http,
        &base,
        slug,
        key_id,
        &signing_key,
        alice_id,
        http.put(format!("{base}/presence/{alice_id}")),
    )
    .await;
    let response = request
        .json(&serde_json::json!({ "status": "Online", "active_in": some_other_integrator_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
}

/// The ticket's other explicit ask: an integrator with no active binding to the
/// target identity at all — never connected, or connected without
/// `presence.publish` — cannot publish presence for them.
#[tokio::test]
#[ignore]
async fn a_integrator_cannot_publish_presence_for_an_unbound_identity() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (alice_id, _alice_token) = seed_identity_session(&pool).await;
    let (integrator, signing_key) = register_unique_integrator(&http, &base).await;
    let slug = integrator["slug"].as_str().unwrap();
    let key_id = integrator["credential"]["key_id"].as_str().unwrap();

    // Deliberately never calls `POST /integrations/{slug}/connect` — no binding,
    // no grant, at all.
    let request = integrator_auth_request(
        &http,
        &base,
        slug,
        key_id,
        &signing_key,
        alice_id,
        http.put(format!("{base}/presence/{alice_id}")),
    )
    .await;
    let response = request
        .json(&serde_json::json!({ "status": "Online" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
}

/// Acceptance criteria's own named scenario: identity A publishes Online,
/// friend B sees it, a non-friend C sees nothing (reads as `Offline`,
/// indistinguishable from a missing entry — same posture issue #97's
/// blocking already established).
#[tokio::test]
#[ignore]
async fn presence_visible_to_friends_only() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (alice_id, alice_token) = seed_identity_session(&pool).await;
    let (bob_id, bob_token) = seed_identity_session(&pool).await;
    let (_carol_id, carol_token) = seed_identity_session(&pool).await;

    // Alice and Bob become friends; Carol never does.
    let friend_request: serde_json::Value =
        auth(http.post(format!("{base}/friends/requests")), &alice_token)
            .json(&serde_json::json!({ "to": bob_id }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let request_id = friend_request["id"].as_str().unwrap();
    auth(
        http.post(format!("{base}/friends/requests/{request_id}/accept")),
        &bob_token,
    )
    .send()
    .await
    .unwrap();

    auth(http.put(format!("{base}/me/presence")), &alice_token)
        .json(&serde_json::json!({ "status": "Online" }))
        .send()
        .await
        .unwrap();

    let bob_read: serde_json::Value = auth(
        http.get(format!("{base}/presence?ids={alice_id}")),
        &bob_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(bob_read[0]["status"], "Online");

    let carol_read: serde_json::Value = auth(
        http.get(format!("{base}/presence?ids={alice_id}")),
        &carol_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(carol_read[0]["status"], "Offline");
}
