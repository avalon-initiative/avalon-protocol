//! Exercises the device-registration/linked-device grant model
//! against a real, running `avalon-server` and Postgres. Gated
//! `--ignored` since it needs live infra — see `make test-live` / `make
//! start`.
//!
//! Test identities/sessions are seeded directly via SQL, same approach as
//! `crates/server/tests/friends.rs` — a signing key is seeded the same way
//! too, but with a real, known `ed25519_dalek::SigningKey` kept in the test
//! so it can produce a genuine approval signature over HTTP, exercising the
//! real verification path rather than bypassing it.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

fn network_id() -> String {
    std::env::var("AVALON_NETWORK_ID").unwrap_or_else(|_| "avalon-dev-local".to_string())
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

async fn seed_identity_session(pool: &PgPool) -> (avalon_protocol::ids::IdentityId, String) {
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
        .bind(format!("device-grants-test-{identity_id}"))
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

/// Seeds a real signing key row for `identity_id` and returns its id plus
/// the actual private key, so the test can sign a genuine grant approval
/// with it — exercising `devices::approve_device_grant`'s real
/// `verify_event_signature` check, not a bypass.
async fn seed_signing_key(
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
    // The projection the node verifies approvals against holds the same key.
    sqlx::query(
        "INSERT INTO indexer_identity_signing_keys (signing_key_id, identity_id, public_key, added_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(key_id)
    .bind(identity_id)
    .bind(public_key.as_slice())
    .execute(pool)
    .await
    .expect("failed to seed projected signing key");

    (key_id, signing_key)
}

fn auth(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request.bearer_auth(token)
}

/// The 32-byte chain head hash behind a request's lowercase-hex `prev_hash`.
fn head_bytes(prev_hash: Option<&str>) -> Option<[u8; 32]> {
    prev_hash.map(|h| hex::decode(h).unwrap().try_into().unwrap())
}

/// The signed body `POST /me/devices/grants/{id}/approve` requires, at a chain position.
fn approve_body_at(
    grant_id: Uuid,
    identity_id: avalon_protocol::ids::IdentityId,
    approver: (Uuid, &SigningKey),
    requested_signing_public_key: &[u8; 32],
    seq: u64,
    prev_hash: Option<&str>,
) -> serde_json::Value {
    let bytes = avalon_protocol::identity_id::device_grant_approval_signing_bytes(
        &network_id(),
        grant_id,
        &identity_id,
        approver.0,
        requested_signing_public_key,
        seq,
        head_bytes(prev_hash).as_ref(),
    );
    serde_json::json!({
        "approver_signing_key_id": approver.0,
        "seq": seq,
        "prev_hash": prev_hash,
        "signature": BASE64.encode(approver.1.sign(&bytes).to_bytes()),
    })
}

/// The signed body `POST /me/devices/{id}/revoke` requires, at a chain position.
fn revoke_body_at(
    identity_id: avalon_protocol::ids::IdentityId,
    target_key_id: Uuid,
    revoker_key_id: Uuid,
    revoker_key: &SigningKey,
    seq: u64,
    prev_hash: Option<&str>,
) -> serde_json::Value {
    let bytes = avalon_protocol::identity_id::signing_key_revoked_signing_bytes(
        &network_id(),
        &identity_id,
        target_key_id,
        revoker_key_id,
        seq,
        head_bytes(prev_hash).as_ref(),
    );
    serde_json::json!({
        "revoked_by_signing_key_id": revoker_key_id,
        "seq": seq,
        "prev_hash": prev_hash,
        "signature": BASE64.encode(revoker_key.sign(&bytes).to_bytes()),
    })
}

/// A revocation at the first chain position (seeded identities start with an empty chain).
fn revoke_body(
    identity_id: avalon_protocol::ids::IdentityId,
    target_key_id: Uuid,
    revoker_key_id: Uuid,
    revoker_key: &SigningKey,
) -> serde_json::Value {
    revoke_body_at(
        identity_id,
        target_key_id,
        revoker_key_id,
        revoker_key,
        1,
        None,
    )
}

#[tokio::test]
#[ignore]
async fn a_grant_approved_by_a_valid_trusted_key_succeeds_and_the_new_key_is_registered() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, token) = seed_identity_session(&pool).await;
    let (trusted_key_id, trusted_key) = seed_signing_key(&pool, identity_id).await;

    let devices_before: serde_json::Value = auth(http.get(format!("{base}/me/devices")), &token)
        .send()
        .await
        .expect("list devices failed — is `make start` running?")
        .json()
        .await
        .unwrap();
    assert_eq!(devices_before.as_array().unwrap().len(), 1);

    let new_device_signing_key = SigningKey::generate(&mut rand::rng());
    let new_device_public_key = new_device_signing_key.verifying_key().to_bytes();

    let request_body = serde_json::json!({
        "requested_signing_public_key": BASE64.encode(new_device_public_key),
        "device_label": "second device",
    });
    let grant: serde_json::Value = auth(http.post(format!("{base}/me/devices/grants")), &token)
        .json(&request_body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(grant["status"], "pending");
    let grant_id: Uuid = grant["id"].as_str().unwrap().parse().unwrap();

    let approve_body = approve_body_at(
        grant_id,
        identity_id,
        (trusted_key_id, &trusted_key),
        &new_device_public_key,
        1,
        None,
    );
    let approve = auth(
        http.post(format!("{base}/me/devices/grants/{grant_id}/approve")),
        &token,
    )
    .json(&approve_body)
    .send()
    .await
    .unwrap();
    assert!(approve.status().is_success(), "{:?}", approve.status());

    let grant_after: serde_json::Value = auth(
        http.get(format!("{base}/me/devices/grants/{grant_id}")),
        &token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(grant_after["status"], "approved");

    let devices_after: serde_json::Value = auth(http.get(format!("{base}/me/devices")), &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(devices_after.as_array().unwrap().len(), 2);
}

#[tokio::test]
#[ignore]
async fn an_approval_attempt_from_a_revoked_key_is_rejected() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, token) = seed_identity_session(&pool).await;
    let (revoked_key_id, revoked_key) = seed_signing_key(&pool, identity_id).await;
    sqlx::query("UPDATE identity_signing_keys SET revoked_at = now() WHERE id = $1")
        .bind(revoked_key_id)
        .execute(&pool)
        .await
        .unwrap();

    let new_device_signing_key = SigningKey::generate(&mut rand::rng());
    let new_device_public_key = new_device_signing_key.verifying_key().to_bytes();
    let request_body = serde_json::json!({
        "requested_signing_public_key": BASE64.encode(new_device_public_key),
        "device_label": null,
    });
    let grant: serde_json::Value = auth(http.post(format!("{base}/me/devices/grants")), &token)
        .json(&request_body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let grant_id: Uuid = grant["id"].as_str().unwrap().parse().unwrap();

    let approve_body = approve_body_at(
        grant_id,
        identity_id,
        (revoked_key_id, &revoked_key),
        &new_device_public_key,
        1,
        None,
    );
    let approve = auth(
        http.post(format!("{base}/me/devices/grants/{grant_id}/approve")),
        &token,
    )
    .json(&approve_body)
    .send()
    .await
    .unwrap();
    assert_eq!(approve.status(), reqwest::StatusCode::UNAUTHORIZED);

    let grant_after: serde_json::Value = auth(
        http.get(format!("{base}/me/devices/grants/{grant_id}")),
        &token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(grant_after["status"], "pending");
}

#[tokio::test]
#[ignore]
async fn revoking_one_device_does_not_affect_another() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, token) = seed_identity_session(&pool).await;
    let (key_a_id, _key_a) = seed_signing_key(&pool, identity_id).await;
    let (key_b_id, key_b) = seed_signing_key(&pool, identity_id).await;

    let revoke = auth(
        http.post(format!("{base}/me/devices/{key_a_id}/revoke")),
        &token,
    )
    .json(&revoke_body(identity_id, key_a_id, key_b_id, &key_b))
    .send()
    .await
    .unwrap();
    assert!(revoke.status().is_success(), "{:?}", revoke.status());

    let devices: serde_json::Value = auth(http.get(format!("{base}/me/devices")), &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let devices = devices.as_array().unwrap();
    let device_a = devices
        .iter()
        .find(|d| d["id"].as_str().unwrap() == key_a_id.to_string())
        .unwrap();
    let device_b = devices
        .iter()
        .find(|d| d["id"].as_str().unwrap() == key_b_id.to_string())
        .unwrap();
    assert!(!device_a["revoked_at"].is_null());
    assert!(device_b["revoked_at"].is_null());

    // Revoking again should be a no-op failure, not a second success — the
    // row is already revoked.
    let second_revoke = auth(
        http.post(format!("{base}/me/devices/{key_a_id}/revoke")),
        &token,
    )
    .json(&revoke_body(identity_id, key_a_id, key_b_id, &key_b))
    .send()
    .await
    .unwrap();
    assert_eq!(second_revoke.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore]
async fn a_device_can_be_renamed_and_the_rename_is_scoped_to_its_own_identity() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_a, token_a) = seed_identity_session(&pool).await;
    let (key_id, _key) = seed_signing_key(&pool, identity_a).await;
    let (_identity_b, token_b) = seed_identity_session(&pool).await;

    let rename = auth(http.patch(format!("{base}/me/devices/{key_id}")), &token_a)
        .json(&serde_json::json!({ "label": "Renamed device" }))
        .send()
        .await
        .expect("rename request failed — is `make start` running?");
    assert!(rename.status().is_success(), "{:?}", rename.status());
    let renamed: serde_json::Value = rename.json().await.unwrap();
    assert_eq!(renamed["label"], "Renamed device");

    let devices: serde_json::Value = auth(http.get(format!("{base}/me/devices")), &token_a)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let device = devices
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"].as_str().unwrap() == key_id.to_string())
        .unwrap();
    assert_eq!(device["label"], "Renamed device");

    // A different identity's session can't rename someone else's device.
    let cross_identity_rename = auth(http.patch(format!("{base}/me/devices/{key_id}")), &token_b)
        .json(&serde_json::json!({ "label": "Hijacked" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        cross_identity_rename.status(),
        reqwest::StatusCode::NOT_FOUND
    );
}

#[tokio::test]
#[ignore]
async fn a_revocation_signed_by_another_identitys_key_or_a_revoked_key_is_refused() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, token) = seed_identity_session(&pool).await;
    let (key_a_id, key_a) = seed_signing_key(&pool, identity_id).await;
    let (key_b_id, key_b) = seed_signing_key(&pool, identity_id).await;
    let (other_identity, _other_token) = seed_identity_session(&pool).await;
    let (outsider_key_id, outsider_key) = seed_signing_key(&pool, other_identity).await;

    let by_outsider = auth(
        http.post(format!("{base}/me/devices/{key_a_id}/revoke")),
        &token,
    )
    .json(&revoke_body(
        identity_id,
        key_a_id,
        outsider_key_id,
        &outsider_key,
    ))
    .send()
    .await
    .unwrap();
    assert!(
        by_outsider.status().is_client_error(),
        "{:?}",
        by_outsider.status()
    );

    let wrong_signature = auth(
        http.post(format!("{base}/me/devices/{key_a_id}/revoke")),
        &token,
    )
    .json(&revoke_body(identity_id, key_b_id, key_b_id, &key_b))
    .send()
    .await
    .unwrap();
    assert_eq!(
        wrong_signature.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a signature over a different target must not authorize this revocation"
    );

    let own = auth(
        http.post(format!("{base}/me/devices/{key_a_id}/revoke")),
        &token,
    )
    .json(&revoke_body(identity_id, key_a_id, key_a_id, &key_a))
    .send()
    .await
    .unwrap();
    assert!(own.status().is_success(), "{:?}", own.status());
}

#[tokio::test]
#[ignore]
async fn the_last_active_signing_key_cannot_be_revoked() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, token) = seed_identity_session(&pool).await;
    let (key_id, key) = seed_signing_key(&pool, identity_id).await;
    let response = auth(
        http.post(format!("{base}/me/devices/{key_id}/revoke")),
        &token,
    )
    .json(&revoke_body(identity_id, key_id, key_id, &key))
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["code"], "LAST_SIGNING_KEY");
}

#[tokio::test]
#[ignore]
async fn two_keys_revoking_each_other_concurrently_leave_one_active() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, token) = seed_identity_session(&pool).await;
    let (key_a_id, key_a) = seed_signing_key(&pool, identity_id).await;
    let (key_b_id, key_b) = seed_signing_key(&pool, identity_id).await;

    // Hold the identity's key rows locked so both requests are in flight before either proceeds.
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM identity_signing_keys WHERE identity_id = $1 FOR UPDATE")
        .bind(identity_id)
        .fetch_all(&mut *blocker)
        .await
        .unwrap();
    let a_revokes_b = auth(
        http.post(format!("{base}/me/devices/{key_b_id}/revoke")),
        &token,
    )
    .json(&revoke_body(identity_id, key_b_id, key_a_id, &key_a))
    .send();
    let b_revokes_a = auth(
        http.post(format!("{base}/me/devices/{key_a_id}/revoke")),
        &token,
    )
    .json(&revoke_body(identity_id, key_a_id, key_b_id, &key_b))
    .send();
    // Release the lock only once both requests are observed waiting on it.
    let waiting_pool = pool.clone();
    let release = async {
        for _ in 0..200 {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE wait_event_type = 'Lock' AND query ILIKE '%identity_signing_keys%'",
            )
            .fetch_one(&waiting_pool)
            .await
            .unwrap();
            if waiting >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        blocker.commit().await.unwrap();
    };
    let (first, second, ()) = tokio::join!(a_revokes_b, b_revokes_a, release);
    let statuses = [first.unwrap().status(), second.unwrap().status()];
    assert_eq!(
        statuses.iter().filter(|s| s.is_success()).count(),
        1,
        "exactly one revocation may win: {statuses:?}"
    );

    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM identity_signing_keys WHERE identity_id = $1 AND revoked_at IS NULL",
    )
    .bind(identity_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(active, 1);
}

async fn request_grant(http: &reqwest::Client, token: &str, public_key: &[u8; 32]) -> Uuid {
    let grant: serde_json::Value = auth(
        http.post(format!("{}/me/devices/grants", server_url())),
        token,
    )
    .json(&serde_json::json!({
        "requested_signing_public_key": BASE64.encode(public_key),
        "device_label": null,
    }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    grant["id"].as_str().unwrap().parse().unwrap()
}

#[tokio::test]
#[ignore]
async fn a_key_event_signed_at_a_stale_position_is_refused_with_the_head_and_resigns() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (identity_id, token) = seed_identity_session(&pool).await;
    let (trusted_id, trusted) = seed_signing_key(&pool, identity_id).await;
    let first_device = SigningKey::generate(&mut rand::rng())
        .verifying_key()
        .to_bytes();
    let second_device = SigningKey::generate(&mut rand::rng())
        .verifying_key()
        .to_bytes();
    let (first, second) = (
        request_grant(&http, &token, &first_device).await,
        request_grant(&http, &token, &second_device).await,
    );
    let approve = |grant_id: Uuid, key: &[u8; 32], seq: u64, prev: Option<&str>| {
        let body = approve_body_at(
            grant_id,
            identity_id,
            (trusted_id, &trusted),
            key,
            seq,
            prev,
        );
        auth(
            http.post(format!("{base}/me/devices/grants/{grant_id}/approve")),
            &token,
        )
        .json(&body)
        .send()
    };

    assert!(approve(first, &first_device, 1, None)
        .await
        .unwrap()
        .status()
        .is_success());
    // The second grant was signed against the empty chain, which is no longer the head.
    let stale = approve(second, &second_device, 1, None).await.unwrap();
    assert_eq!(stale.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = stale.json().await.unwrap();
    assert_eq!(body["code"], "IDENTITY_CHAIN_POSITION_STALE");
    assert_eq!(body["head_seq"], 1);
    let head = body["head_hash"].as_str().unwrap().to_string();
    // Nothing changed, and signing at the returned head succeeds.
    let devices: serde_json::Value = auth(http.get(format!("{base}/me/devices")), &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(devices.as_array().unwrap().len(), 2);
    let ok = approve(second, &second_device, 2, Some(&head))
        .await
        .unwrap();
    assert!(ok.status().is_success(), "{:?}", ok.status());
    // The new key's id is the grant id.
    let ids: Vec<String> = devices_ids(&http, &token).await;
    assert!(ids.contains(&first.to_string()) && ids.contains(&second.to_string()));
}

async fn devices_ids(http: &reqwest::Client, token: &str) -> Vec<String> {
    let devices: serde_json::Value = auth(http.get(format!("{}/me/devices", server_url())), token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    devices
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
#[ignore]
async fn a_signature_does_not_carry_over_to_another_approver_id_position_or_malformed_head() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (identity_id, token) = seed_identity_session(&pool).await;
    let (trusted_id, trusted) = seed_signing_key(&pool, identity_id).await;
    let (other_id, _other) = seed_signing_key(&pool, identity_id).await;
    let device = SigningKey::generate(&mut rand::rng())
        .verifying_key()
        .to_bytes();
    let grant_id = request_grant(&http, &token, &device).await;
    let post = |body: serde_json::Value| {
        auth(
            http.post(format!("{base}/me/devices/grants/{grant_id}/approve")),
            &token,
        )
        .json(&body)
        .send()
    };
    let signed = approve_body_at(
        grant_id,
        identity_id,
        (trusted_id, &trusted),
        &device,
        1,
        None,
    );

    // The same signature under another approver key id, or claiming another position.
    let mut other_approver = signed.clone();
    other_approver["approver_signing_key_id"] = serde_json::json!(other_id);
    let mut other_seq = signed.clone();
    other_seq["seq"] = serde_json::json!(2);
    let mut other_prev = signed.clone();
    other_prev["prev_hash"] = serde_json::json!("00".repeat(32));
    for replay in [other_approver, other_seq, other_prev] {
        let response = post(replay).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    }
    let mut malformed = signed.clone();
    malformed["prev_hash"] = serde_json::json!("ABCD");
    let response = post(malformed).await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(post(signed).await.unwrap().status().is_success());
}

#[tokio::test]
#[ignore]
async fn a_revocation_signed_at_a_stale_position_is_refused_with_the_head() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (identity_id, token) = seed_identity_session(&pool).await;
    let (a_id, a) = seed_signing_key(&pool, identity_id).await;
    let (b_id, b) = seed_signing_key(&pool, identity_id).await;
    let (c_id, _c) = seed_signing_key(&pool, identity_id).await;
    let revoke = |target: Uuid, body: serde_json::Value| {
        auth(
            http.post(format!("{base}/me/devices/{target}/revoke")),
            &token,
        )
        .json(&body)
        .send()
    };
    let first = revoke(a_id, revoke_body_at(identity_id, a_id, b_id, &b, 1, None))
        .await
        .unwrap();
    assert!(first.status().is_success());
    let stale = revoke(c_id, revoke_body_at(identity_id, c_id, b_id, &b, 1, None))
        .await
        .unwrap();
    assert_eq!(stale.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = stale.json().await.unwrap();
    assert_eq!(body["code"], "IDENTITY_CHAIN_POSITION_STALE");
    assert_eq!(body["head_seq"], 1);
    let head = body["head_hash"].as_str().unwrap();
    let ok = revoke(
        c_id,
        revoke_body_at(identity_id, c_id, b_id, &b, 2, Some(head)),
    )
    .await
    .unwrap();
    assert!(ok.status().is_success(), "{:?}", ok.status());
    let _ = a;
}

/// Splits two concurrent responses into the one success and the 409 stale-position body.
async fn one_wins_one_stale(first: reqwest::Response, second: reqwest::Response) {
    let mut stale = None;
    let mut wins = 0;
    for response in [first, second] {
        if response.status().is_success() {
            wins += 1;
        } else {
            assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
            stale = Some(response.json::<serde_json::Value>().await.unwrap());
        }
    }
    assert_eq!(
        wins, 1,
        "exactly one concurrent event may take the position"
    );
    let body = stale.unwrap();
    assert_eq!(body["code"], "IDENTITY_CHAIN_POSITION_STALE");
    assert_eq!(body["head_seq"], 1);
    assert_eq!(body["head_hash"].as_str().unwrap().len(), 64);
}

#[tokio::test]
#[ignore]
async fn two_concurrent_approvals_at_the_same_head_leave_one_and_a_stale_refusal() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (identity_id, token) = seed_identity_session(&pool).await;
    let (trusted_id, trusted) = seed_signing_key(&pool, identity_id).await;
    let keys = [
        SigningKey::generate(&mut rand::rng())
            .verifying_key()
            .to_bytes(),
        SigningKey::generate(&mut rand::rng())
            .verifying_key()
            .to_bytes(),
    ];
    let grants = [
        request_grant(&http, &token, &keys[0]).await,
        request_grant(&http, &token, &keys[1]).await,
    ];
    let send = |i: usize| {
        let body = approve_body_at(
            grants[i],
            identity_id,
            (trusted_id, &trusted),
            &keys[i],
            1,
            None,
        );
        auth(
            http.post(format!("{base}/me/devices/grants/{}/approve", grants[i])),
            &token,
        )
        .json(&body)
        .send()
    };
    let (a, b) = tokio::join!(send(0), send(1));
    one_wins_one_stale(a.unwrap(), b.unwrap()).await;
}

#[tokio::test]
#[ignore]
async fn two_concurrent_revocations_at_the_same_head_leave_one_and_a_stale_refusal() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (identity_id, token) = seed_identity_session(&pool).await;
    let (a_id, _a) = seed_signing_key(&pool, identity_id).await;
    let (b_id, _b) = seed_signing_key(&pool, identity_id).await;
    let (c_id, c) = seed_signing_key(&pool, identity_id).await;
    let send = |target: Uuid| {
        let body = revoke_body_at(identity_id, target, c_id, &c, 1, None);
        auth(
            http.post(format!("{base}/me/devices/{target}/revoke")),
            &token,
        )
        .json(&body)
        .send()
    };
    let (first, second) = tokio::join!(send(a_id), send(b_id));
    one_wins_one_stale(first.unwrap(), second.unwrap()).await;
}
