//! Exercises epic #623 / issue #634's cross-node login lifecycle against a
//! real, running `avalon-server` and Postgres. Gated `--ignored`, same
//! convention as `crates/server/tests/session_continuation.rs`, whose
//! WebAuthn-ceremony/signing-key helpers this file borrows the shape of.
//!
//! This sandbox only ever runs one real `avalon-server` process against
//! this database (same limitation `crates/server/tests/mirror_watcher.rs`'s
//! own module doc already notes for a different feature), so every grant
//! here is verified against that one node's own `AVALON_NODE_URL` — the
//! genuinely-cross-node case (grant destination-bound to a *different*
//! node) is covered by the mismatched-destination rejection test instead of
//! an actual second live node.

use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::{CredentialCreationOptions, CredentialRequestOptions};
use uuid::Uuid;

use avalon_protocol::cross_node_login::{signing_bytes, CrossNodeLoginGrant, DEFAULT_TTL_SECONDS};

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn rp_origin() -> url::Url {
    let origin = std::env::var("AVALON_WEBAUTHN_ORIGIN")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    url::Url::parse(&origin).expect("AVALON_WEBAUTHN_ORIGIN must be a valid URL")
}

type VirtualClient =
    Client<MemoryStore, MockUserValidationMethod, public_suffix::PublicSuffixList, ()>;

fn new_virtual_client() -> VirtualClient {
    let authenticator = Authenticator::new(
        Aaguid::new_empty(),
        MemoryStore::new(),
        MockUserValidationMethod::verified_user(2),
    );
    Client::new(authenticator).allows_insecure_localhost(true)
}

async fn create_identity_and_log_in(
    http: &reqwest::Client,
    base: &str,
) -> (Uuid, String, SigningKey) {
    let identity_id = Uuid::new_v4();
    let display_name = format!("cross-node-login-test-{identity_id}");

    let start_body = serde_json::json!({
        "identity_id": identity_id,
        "display_name": display_name,
    });
    let start: serde_json::Value = http
        .post(format!("{base}/identities/register/start"))
        .json(&start_body)
        .send()
        .await
        .expect("register/start failed — is `make start` running?")
        .json()
        .await
        .unwrap();
    let ticket_id = start["ticket_id"].as_str().unwrap().to_string();

    let mut client = new_virtual_client();
    let origin = rp_origin();
    let creation_options: CredentialCreationOptions =
        serde_json::from_value(start["challenge"].clone()).unwrap();
    let credential = client
        .register(Origin::from(&origin), creation_options, DefaultClientData)
        .await
        .expect("virtual authenticator registration should succeed");

    let signing_key = SigningKey::generate(&mut rand::rng());
    let signing_bytes_for_creation =
        format!("avalon:identity.created:v1:{identity_id}:{display_name}").into_bytes();
    let signature = signing_key.sign(&signing_bytes_for_creation);

    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine;

    let finish_body = serde_json::json!({
        "ticket_id": ticket_id,
        "webauthn_credential": credential,
        "event_signing_public_key": BASE64.encode(signing_key.verifying_key().to_bytes()),
        "event_signature": BASE64.encode(signature.to_bytes()),
        "device_label": null,
    });
    http.post(format!("{base}/identities/register/finish"))
        .json(&finish_body)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .expect("register/finish should succeed");

    let session_start: serde_json::Value = http
        .post(format!("{base}/sessions/start"))
        .json(&serde_json::json!({ "identity_id": identity_id }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_ticket_id = session_start["ticket_id"].as_str().unwrap().to_string();
    let request_options: CredentialRequestOptions =
        serde_json::from_value(session_start["challenge"].clone()).unwrap();
    let assertion = client
        .authenticate(Origin::from(&origin), request_options, DefaultClientData)
        .await
        .expect("virtual authenticator authentication should succeed");
    let session_finish: serde_json::Value = http
        .post(format!("{base}/sessions/finish"))
        .json(&serde_json::json!({ "ticket_id": session_ticket_id, "credential": assertion }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = session_finish["token"].as_str().unwrap().to_string();

    (identity_id, token, signing_key)
}

async fn first_signing_key_id(http: &reqwest::Client, base: &str, session_token: &str) -> Uuid {
    let devices: serde_json::Value = http
        .get(format!("{base}/me/devices"))
        .bearer_auth(session_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let devices = devices.as_array().unwrap();
    assert_eq!(
        devices.len(),
        1,
        "a freshly registered identity has exactly one signing key"
    );
    devices[0]["id"].as_str().unwrap().parse().unwrap()
}

#[allow(clippy::too_many_arguments)]
fn mint_grant(
    identity_id: Uuid,
    signing_key_id: Uuid,
    destination_base_url: &str,
    requesting_context: &str,
    signing_key: &SigningKey,
    issued_at: time::OffsetDateTime,
    expires_at: time::OffsetDateTime,
) -> CrossNodeLoginGrant {
    let nonce = Uuid::new_v4();
    let bytes = signing_bytes(
        identity_id,
        signing_key_id,
        destination_base_url,
        requesting_context,
        nonce,
        issued_at,
        expires_at,
    );
    let signature = signing_key.sign(&bytes);
    CrossNodeLoginGrant {
        identity_id,
        signing_key_id,
        destination_base_url: destination_base_url.to_string(),
        requesting_context: requesting_context.to_string(),
        nonce,
        issued_at,
        expires_at,
        signature: hex::encode(signature.to_bytes()),
    }
}

/// The same-device fast path: submit a freshly-minted grant with no
/// `user_code` at all, and get a real session back directly.
#[tokio::test]
#[ignore]
async fn same_device_submit_returns_a_real_session_directly() {
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, session_token, signing_key) = create_identity_and_log_in(&http, &base).await;
    let signing_key_id = first_signing_key_id(&http, &base, &session_token).await;

    let start: serde_json::Value = http
        .post(format!("{base}/auth/cross-node/start"))
        .send()
        .await
        .expect("cross-node/start failed — is `make start` running?")
        .json()
        .await
        .unwrap();
    let destination = start["requesting_context"].as_str().unwrap().to_string();

    let now = time::OffsetDateTime::now_utc();
    let grant = mint_grant(
        identity_id,
        signing_key_id,
        &destination,
        &destination,
        &signing_key,
        now,
        now + time::Duration::seconds(30),
    );

    let submit: serde_json::Value = http
        .post(format!("{base}/auth/cross-node/submit"))
        .json(&serde_json::json!({ "grant": grant }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .expect("submit should succeed")
        .json()
        .await
        .unwrap();
    let token = submit["token"]
        .as_str()
        .expect("same-device submit should return a token directly");

    let me: serde_json::Value = http
        .get(format!("{base}/me"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .expect("minted session should authenticate")
        .json()
        .await
        .unwrap();
    assert_eq!(me["identity_id"], identity_id.to_string());
}

/// The cross-device path: start on one "client", submit the grant (as if
/// relayed from wherever the signing key lives), poll to pick up the
/// resulting session — exactly the flow a mobile-hub QR scan drives.
#[tokio::test]
#[ignore]
async fn cross_device_start_submit_poll_round_trip_delivers_a_session_exactly_once() {
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, session_token, signing_key) = create_identity_and_log_in(&http, &base).await;
    let signing_key_id = first_signing_key_id(&http, &base, &session_token).await;

    let start: serde_json::Value = http
        .post(format!("{base}/auth/cross-node/start"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let request_code = start["request_code"].as_str().unwrap().to_string();
    let user_code = start["user_code"].as_str().unwrap().to_string();
    let destination = start["requesting_context"].as_str().unwrap().to_string();
    assert!(start["expires_in"].as_i64().unwrap() > 0);

    let pending: serde_json::Value = http
        .post(format!("{base}/auth/cross-node/poll"))
        .bearer_auth(&request_code)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pending["status"], "pending");

    let now = time::OffsetDateTime::now_utc();
    let grant = mint_grant(
        identity_id,
        signing_key_id,
        &destination,
        &destination,
        &signing_key,
        now,
        now + time::Duration::seconds(30),
    );
    let submit = http
        .post(format!("{base}/auth/cross-node/submit"))
        .json(&serde_json::json!({ "user_code": user_code, "grant": grant }))
        .send()
        .await
        .unwrap();
    assert!(submit.status().is_success(), "{:?}", submit.status());

    let approved: serde_json::Value = http
        .post(format!("{base}/auth/cross-node/poll"))
        .bearer_auth(&request_code)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(approved["status"], "approved");
    let token = approved["token"]
        .as_str()
        .expect("approved poll should carry a token");

    let me: serde_json::Value = http
        .get(format!("{base}/me"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["identity_id"], identity_id.to_string());

    // Single-use: a second poll of the same request_code never returns the
    // token again.
    let second_poll: serde_json::Value = http
        .post(format!("{base}/auth/cross-node/poll"))
        .bearer_auth(&request_code)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(second_poll["status"], "expired");
    assert!(second_poll["token"].is_null());
}

/// The destination-binding check (#610's lesson, applied here): a grant
/// signed for a different destination than this node's own `base_url`
/// must be rejected outright, even with an otherwise-perfect signature.
#[tokio::test]
#[ignore]
async fn a_grant_bound_to_a_different_destination_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, session_token, signing_key) = create_identity_and_log_in(&http, &base).await;
    let signing_key_id = first_signing_key_id(&http, &base, &session_token).await;

    let now = time::OffsetDateTime::now_utc();
    let grant = mint_grant(
        identity_id,
        signing_key_id,
        "https://a-different-node.example",
        "SomeIntegrator (a-different-node.example)",
        &signing_key,
        now,
        now + time::Duration::seconds(30),
    );

    let submit = http
        .post(format!("{base}/auth/cross-node/submit"))
        .json(&serde_json::json!({ "grant": grant }))
        .send()
        .await
        .unwrap();
    assert_eq!(submit.status().as_u16(), 401);
}

/// A grant can never be submitted twice — same nonce anti-replay gate
/// `session_continuation.rs` already proves for `ContinuationToken`.
#[tokio::test]
#[ignore]
async fn a_grant_cannot_be_replayed() {
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, session_token, signing_key) = create_identity_and_log_in(&http, &base).await;
    let signing_key_id = first_signing_key_id(&http, &base, &session_token).await;

    let start: serde_json::Value = http
        .post(format!("{base}/auth/cross-node/start"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let destination = start["requesting_context"].as_str().unwrap().to_string();

    let now = time::OffsetDateTime::now_utc();
    let grant = mint_grant(
        identity_id,
        signing_key_id,
        &destination,
        &destination,
        &signing_key,
        now,
        now + time::Duration::seconds(30),
    );

    let first = http
        .post(format!("{base}/auth/cross-node/submit"))
        .json(&serde_json::json!({ "grant": grant }))
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success());

    let second = http
        .post(format!("{base}/auth/cross-node/submit"))
        .json(&serde_json::json!({ "grant": grant }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status().as_u16(), 401);
}

/// An expired grant is rejected even with an otherwise-perfect signature.
#[tokio::test]
#[ignore]
async fn an_expired_grant_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, session_token, signing_key) = create_identity_and_log_in(&http, &base).await;
    let signing_key_id = first_signing_key_id(&http, &base, &session_token).await;

    let start: serde_json::Value = http
        .post(format!("{base}/auth/cross-node/start"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let destination = start["requesting_context"].as_str().unwrap().to_string();

    let past = time::OffsetDateTime::now_utc() - time::Duration::minutes(5);
    let grant = mint_grant(
        identity_id,
        signing_key_id,
        &destination,
        &destination,
        &signing_key,
        past,
        past + time::Duration::seconds(30),
    );

    let response = http
        .post(format!("{base}/auth/cross-node/submit"))
        .json(&serde_json::json!({ "grant": grant }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 401);
}

/// A grant whose claimed validity window exceeds the protocol's own
/// documented cap is rejected, regardless of a valid signature.
#[tokio::test]
#[ignore]
async fn a_grant_claiming_too_long_a_ttl_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, session_token, signing_key) = create_identity_and_log_in(&http, &base).await;
    let signing_key_id = first_signing_key_id(&http, &base, &session_token).await;

    let start: serde_json::Value = http
        .post(format!("{base}/auth/cross-node/start"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let destination = start["requesting_context"].as_str().unwrap().to_string();

    let now = time::OffsetDateTime::now_utc();
    let grant = mint_grant(
        identity_id,
        signing_key_id,
        &destination,
        &destination,
        &signing_key,
        now,
        now + time::Duration::seconds(DEFAULT_TTL_SECONDS * 10),
    );

    let response = http
        .post(format!("{base}/auth/cross-node/submit"))
        .json(&serde_json::json!({ "grant": grant }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 401);
}

/// A grant signed with the wrong key (not the identity's real one) is
/// rejected — proves the signature is actually checked, not just parsed.
#[tokio::test]
#[ignore]
async fn a_grant_signed_by_the_wrong_key_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, session_token, _real_signing_key) =
        create_identity_and_log_in(&http, &base).await;
    let signing_key_id = first_signing_key_id(&http, &base, &session_token).await;

    let start: serde_json::Value = http
        .post(format!("{base}/auth/cross-node/start"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let destination = start["requesting_context"].as_str().unwrap().to_string();

    let impostor_key = SigningKey::generate(&mut rand::rng());
    let now = time::OffsetDateTime::now_utc();
    let grant = mint_grant(
        identity_id,
        signing_key_id,
        &destination,
        &destination,
        &impostor_key,
        now,
        now + time::Duration::seconds(30),
    );

    let response = http
        .post(format!("{base}/auth/cross-node/submit"))
        .json(&serde_json::json!({ "grant": grant }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 401);
}

/// Denial is unauthenticated on purpose (see `cross_node_login::deny`'s own
/// doc comment) — it still resolves the pending request and blocks a later
/// submit against the same `user_code`.
#[tokio::test]
#[ignore]
async fn a_denied_request_can_never_be_approved_afterward() {
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, session_token, signing_key) = create_identity_and_log_in(&http, &base).await;
    let signing_key_id = first_signing_key_id(&http, &base, &session_token).await;

    let start: serde_json::Value = http
        .post(format!("{base}/auth/cross-node/start"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let request_code = start["request_code"].as_str().unwrap().to_string();
    let user_code = start["user_code"].as_str().unwrap().to_string();
    let destination = start["requesting_context"].as_str().unwrap().to_string();

    let deny = http
        .post(format!("{base}/auth/cross-node/deny"))
        .json(&serde_json::json!({ "user_code": user_code }))
        .send()
        .await
        .unwrap();
    assert!(deny.status().is_success(), "{:?}", deny.status());

    let polled: serde_json::Value = http
        .post(format!("{base}/auth/cross-node/poll"))
        .bearer_auth(&request_code)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(polled["status"], "denied");

    let now = time::OffsetDateTime::now_utc();
    let grant = mint_grant(
        identity_id,
        signing_key_id,
        &destination,
        &destination,
        &signing_key,
        now,
        now + time::Duration::seconds(30),
    );
    let submit_after_deny = http
        .post(format!("{base}/auth/cross-node/submit"))
        .json(&serde_json::json!({ "user_code": user_code, "grant": grant }))
        .send()
        .await
        .unwrap();
    assert_eq!(submit_after_deny.status().as_u16(), 404);
}

#[tokio::test]
#[ignore]
async fn polling_an_unknown_request_code_is_unauthorized() {
    let http = reqwest::Client::new();
    let base = server_url();

    let response = http
        .post(format!("{base}/auth/cross-node/poll"))
        .bearer_auth("not-a-real-request-code")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}
