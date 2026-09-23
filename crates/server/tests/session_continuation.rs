//! Exercises issue #525 (Part 2 of #521's decision) — session-continuation
//! tokens — against a real, running `avalon-server` and Postgres. Gated
//! `--ignored`, same convention as `crates/server/tests/passkeys.rs`, whose
//! WebAuthn-ceremony helper this file borrows.
//!
//! A continuation token is minted client-side with the identity's own
//! Ed25519 event-signing key (the same one produced during registration)
//! and presented as a normal `Authorization: Bearer` header — this file
//! never talks to a second real `avalon-server` process (same sandbox
//! limitation `crates/server/tests/mirror_watcher.rs`'s own module doc
//! already notes), but verification itself doesn't care which node ran the
//! original ceremony: it only ever reads
//! `avalon_indexer::projections::identity_signing_keys`, exactly what a
//! mirror-only node would also read from replayed history. See
//! `docs/projects/backend-server/architecture/identity.md`'s session-continuation section.

use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::{CredentialCreationOptions, CredentialRequestOptions};
use uuid::Uuid;

use avalon_protocol::continuation::{signing_bytes, ContinuationToken, DEFAULT_TTL_SECONDS};

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

/// Registers a brand-new identity, returning its id, a logged-in session
/// token, and the real `SigningKey` used for `identity.created` — the
/// exact private key backing the identity's very first, now-durable
/// `identity_signing_keys` row.
async fn create_identity_and_log_in(
    http: &reqwest::Client,
    base: &str,
) -> (Uuid, String, SigningKey) {
    let identity_id = Uuid::new_v4();
    let display_name = format!("continuation-test-{identity_id}");

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

fn mint_token(
    identity_id: Uuid,
    signing_key_id: Uuid,
    signing_key: &SigningKey,
    issued_at: time::OffsetDateTime,
    expires_at: time::OffsetDateTime,
) -> ContinuationToken {
    let nonce = Uuid::new_v4();
    let bytes = signing_bytes(identity_id, signing_key_id, nonce, issued_at, expires_at);
    let signature = signing_key.sign(&bytes);
    ContinuationToken {
        identity_id,
        signing_key_id,
        nonce,
        issued_at,
        expires_at,
        signature: hex::encode(signature.to_bytes()),
    }
}

/// The headline case: a continuation token, minted with the identity's own
/// signing key and never involving the opaque `sessions` table at all,
/// authenticates a normal request exactly like a real session token would.
#[tokio::test]
#[ignore]
async fn a_valid_continuation_token_authenticates_like_a_real_session() {
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, session_token, signing_key) = create_identity_and_log_in(&http, &base).await;
    let signing_key_id = first_signing_key_id(&http, &base, &session_token).await;

    let now = time::OffsetDateTime::now_utc();
    let token = mint_token(
        identity_id,
        signing_key_id,
        &signing_key,
        now,
        now + time::Duration::seconds(30),
    );

    let me: serde_json::Value = http
        .get(format!("{base}/me"))
        .bearer_auth(token.to_wire())
        .send()
        .await
        .unwrap()
        .error_for_status()
        .expect("continuation token should authenticate")
        .json()
        .await
        .unwrap();
    assert_eq!(me["identity_id"], identity_id.to_string());
}

/// The nonce anti-replay gate: the exact same token string can never be
/// accepted twice.
#[tokio::test]
#[ignore]
async fn a_continuation_token_cannot_be_replayed() {
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, session_token, signing_key) = create_identity_and_log_in(&http, &base).await;
    let signing_key_id = first_signing_key_id(&http, &base, &session_token).await;

    let now = time::OffsetDateTime::now_utc();
    let token = mint_token(
        identity_id,
        signing_key_id,
        &signing_key,
        now,
        now + time::Duration::seconds(30),
    );
    let wire = token.to_wire();

    let first = http
        .get(format!("{base}/me"))
        .bearer_auth(&wire)
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success());

    let second = http
        .get(format!("{base}/me"))
        .bearer_auth(&wire)
        .send()
        .await
        .unwrap();
    assert_eq!(
        second.status().as_u16(),
        401,
        "replaying the same token must be rejected"
    );
}

/// An expired token is rejected even with an otherwise-perfect signature.
#[tokio::test]
#[ignore]
async fn an_expired_continuation_token_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, session_token, signing_key) = create_identity_and_log_in(&http, &base).await;
    let signing_key_id = first_signing_key_id(&http, &base, &session_token).await;

    let past = time::OffsetDateTime::now_utc() - time::Duration::minutes(5);
    let token = mint_token(
        identity_id,
        signing_key_id,
        &signing_key,
        past,
        past + time::Duration::seconds(30),
    );

    let response = http
        .get(format!("{base}/me"))
        .bearer_auth(token.to_wire())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 401);
}

/// A token whose claimed validity window exceeds the protocol's own
/// documented cap is rejected, regardless of a valid signature — a
/// verifier must never just trust `expires_at` at face value.
#[tokio::test]
#[ignore]
async fn a_continuation_token_claiming_too_long_a_ttl_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, session_token, signing_key) = create_identity_and_log_in(&http, &base).await;
    let signing_key_id = first_signing_key_id(&http, &base, &session_token).await;

    let now = time::OffsetDateTime::now_utc();
    let token = mint_token(
        identity_id,
        signing_key_id,
        &signing_key,
        now,
        now + time::Duration::seconds(DEFAULT_TTL_SECONDS * 10),
    );

    let response = http
        .get(format!("{base}/me"))
        .bearer_auth(token.to_wire())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 401);
}

/// A token signed with the wrong key (not the identity's real one) is
/// rejected — proves the signature is actually checked, not just parsed.
#[tokio::test]
#[ignore]
async fn a_continuation_token_signed_by_the_wrong_key_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, session_token, _real_signing_key) =
        create_identity_and_log_in(&http, &base).await;
    let signing_key_id = first_signing_key_id(&http, &base, &session_token).await;

    let impostor_key = SigningKey::generate(&mut rand::rng());
    let now = time::OffsetDateTime::now_utc();
    let token = mint_token(
        identity_id,
        signing_key_id,
        &impostor_key,
        now,
        now + time::Duration::seconds(30),
    );

    let response = http
        .get(format!("{base}/me"))
        .bearer_auth(token.to_wire())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 401);
}

/// Once the signing key is revoked, a continuation token minted with it —
/// even one that was never used before — must be rejected. This is the
/// invariant the whole ticket calls out explicitly: revocation must be
/// respected regardless of which node (or which projection path) is doing
/// the checking.
#[tokio::test]
#[ignore]
async fn a_continuation_token_signed_by_a_revoked_key_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, session_token, signing_key) = create_identity_and_log_in(&http, &base).await;
    let signing_key_id = first_signing_key_id(&http, &base, &session_token).await;

    // Revoking your own only signing key is allowed (unilateral, per
    // `devices::revoke_device`'s own doc comment) — the identity still has
    // its passkey for normal login, just no active signing key.
    let revoke_status = http
        .post(format!("{base}/me/devices/{signing_key_id}/revoke"))
        .bearer_auth(&session_token)
        .send()
        .await
        .unwrap()
        .status();
    assert!(revoke_status.is_success());

    let now = time::OffsetDateTime::now_utc();
    let token = mint_token(
        identity_id,
        signing_key_id,
        &signing_key,
        now,
        now + time::Duration::seconds(30),
    );

    let response = http
        .get(format!("{base}/me"))
        .bearer_auth(token.to_wire())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 401);
}

/// A continuation token never substitutes for the WebAuthn ceremony that
/// establishes a brand-new session — it can only extend one via an
/// already-existing signing key. There is no separate "reject this as a
/// login" check to exercise (continuation tokens are never accepted by
/// `/sessions/*`, which never call `authenticate`/`authenticate_token` at
/// all), so this test documents that boundary by confirming the token only
/// ever works against an *authenticated* route, never as a substitute for
/// the registration/login endpoints themselves.
#[tokio::test]
#[ignore]
async fn a_continuation_token_is_never_accepted_by_the_registration_endpoints() {
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, session_token, signing_key) = create_identity_and_log_in(&http, &base).await;
    let signing_key_id = first_signing_key_id(&http, &base, &session_token).await;

    let now = time::OffsetDateTime::now_utc();
    let token = mint_token(
        identity_id,
        signing_key_id,
        &signing_key,
        now,
        now + time::Duration::seconds(30),
    );

    // `/sessions/start` takes an `identity_id` body, not a bearer token at
    // all — confirming a continuation token has no code path into it.
    let response = http
        .post(format!("{base}/sessions/start"))
        .bearer_auth(token.to_wire())
        .json(&serde_json::json!({ "identity_id": identity_id }))
        .send()
        .await
        .unwrap();
    // Succeeds or fails on its own webauthn-ceremony merits, entirely
    // independent of the bearer header present here — asserting only that
    // this didn't somehow short-circuit into an authenticated response
    // shaped like `/me`'s.
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body.get("identity_id").is_none());
}
