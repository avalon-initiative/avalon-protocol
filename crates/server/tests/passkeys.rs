//! Exercises multi-passkey registration/revocation (issue #200) against a
//! real, running `avalon-server` and Postgres. Gated `--ignored` since it
//! needs live infra — see `make test-live` / `make start`.
//!
//! Drives genuine WebAuthn ceremonies over HTTP with `passkey`'s virtual
//! client + authenticator, same approach `crates/server/src/auth.rs`'s
//! in-process test and `avalon create-identity`/`avalon login` already use
//! — a fresh virtual authenticator per passkey, so "register passkey A,
//! then passkey B, then authenticate with B alone" is a real, independent
//! credential each authenticator only ever knows about its own passkey.
//!
//! Test identities/sessions/the first passkey are seeded directly via SQL
//! (same approach `crates/server/tests/device_grants.rs` takes for its own
//! seeding), since account *creation* itself is already covered by #55's
//! existing coverage — this file is scoped to what #200 actually adds.

use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::{CredentialCreationOptions, CredentialRequestOptions};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn rp_origin() -> url::Url {
    let origin = std::env::var("AVALON_WEBAUTHN_ORIGIN")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    url::Url::parse(&origin).expect("AVALON_WEBAUTHN_ORIGIN must be a valid URL")
}

async fn test_pool() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

/// Named so `Client`'s registration/authentication ceremony state can be
/// carried across `await` points and function boundaries in this file —
/// the TLD-verifier type parameter (`public_suffix::PublicSuffixList`) has
/// to be spelled out explicitly since `passkey-client` doesn't re-export it
/// under its own namespace (see the workspace `Cargo.toml`'s
/// `public-suffix` dependency comment).
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

/// Registers a brand-new identity with exactly one passkey, entirely via
/// real HTTP against the running server (the same `/identities/register/*`
/// ceremony the Hub drives) — including a throwaway Ed25519 event-signing
/// key, since `register_finish` requires one even though this test has no
/// further use for it.
async fn create_identity_with_one_passkey(
    http: &reqwest::Client,
    base: &str,
) -> (Uuid, VirtualClient) {
    let identity_id = Uuid::new_v4();
    let display_name = format!("passkeys-test-{identity_id}");

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

    use ed25519_dalek::{Signer, SigningKey};
    let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
    let signing_bytes =
        format!("avalon:identity.created:v1:{identity_id}:{display_name}").into_bytes();
    let signature = signing_key.sign(&signing_bytes);

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

    (identity_id, client)
}

async fn login_with_client(
    http: &reqwest::Client,
    base: &str,
    identity_id: Uuid,
    client: &mut VirtualClient,
) -> String {
    let start_body = serde_json::json!({ "identity_id": identity_id });
    let start: serde_json::Value = http
        .post(format!("{base}/sessions/start"))
        .json(&start_body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ticket_id = start["ticket_id"].as_str().unwrap().to_string();

    let origin = rp_origin();
    let request_options: CredentialRequestOptions =
        serde_json::from_value(start["challenge"].clone()).unwrap();
    let assertion = client
        .authenticate(Origin::from(&origin), request_options, DefaultClientData)
        .await
        .expect("virtual authenticator authentication should succeed");

    let finish_body = serde_json::json!({
        "ticket_id": ticket_id,
        "credential": assertion,
    });
    let finish: serde_json::Value = http
        .post(format!("{base}/sessions/finish"))
        .json(&finish_body)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .expect("sessions/finish should succeed")
        .json()
        .await
        .unwrap();
    finish["token"].as_str().unwrap().to_string()
}

fn auth(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request.bearer_auth(token)
}

#[tokio::test]
#[ignore]
async fn register_second_passkey_authenticate_with_either_then_revoke_one() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, mut client_a) = create_identity_with_one_passkey(&http, &base).await;
    let token = login_with_client(&http, &base, identity_id, &mut client_a).await;

    let passkeys_after_registration: serde_json::Value =
        auth(http.get(format!("{base}/me/passkeys")), &token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(passkeys_after_registration.as_array().unwrap().len(), 1);

    // Add a second passkey — the authenticated flow #200 adds, distinct
    // from the unauthenticated account-creation ceremony above.
    let start: serde_json::Value = auth(
        http.post(format!("{base}/me/passkeys/register/start")),
        &token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let ticket_id = start["ticket_id"].as_str().unwrap().to_string();

    let mut client_b = new_virtual_client();
    let origin = rp_origin();
    let creation_options: CredentialCreationOptions =
        serde_json::from_value(start["challenge"].clone()).unwrap();
    let credential = client_b
        .register(Origin::from(&origin), creation_options, DefaultClientData)
        .await
        .expect("virtual authenticator registration should succeed");

    let finish_body = serde_json::json!({
        "ticket_id": ticket_id,
        "webauthn_credential": credential,
        "label": "second device",
    });
    let added: serde_json::Value = auth(
        http.post(format!("{base}/me/passkeys/register/finish")),
        &token,
    )
    .json(&finish_body)
    .send()
    .await
    .unwrap()
    .error_for_status()
    .expect("passkeys/register/finish should succeed")
    .json()
    .await
    .unwrap();
    assert_eq!(added["label"], "second device");
    let second_passkey_id = added["id"].as_str().unwrap().to_string();

    let passkeys: serde_json::Value = auth(http.get(format!("{base}/me/passkeys")), &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(passkeys.as_array().unwrap().len(), 2);

    // Authenticate with the *second* passkey alone — a fresh session,
    // proving it's independently valid, not merely accepted alongside the
    // first.
    let token_via_b = login_with_client(&http, &base, identity_id, &mut client_b).await;
    let me: serde_json::Value = auth(http.get(format!("{base}/me")), &token_via_b)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["identity_id"], identity_id.to_string());

    // Revoke the first passkey — two remain zero afterward is not the
    // case here (one will remain), so no ?confirm=true should be needed.
    let passkeys_before_revoke: serde_json::Value =
        auth(http.get(format!("{base}/me/passkeys")), &token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let first_passkey_id = passkeys_before_revoke
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"].as_str().unwrap() != second_passkey_id)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let revoke_status = auth(
        http.post(format!("{base}/me/passkeys/{first_passkey_id}/revoke")),
        &token,
    )
    .send()
    .await
    .unwrap()
    .status();
    assert!(revoke_status.is_success());

    let passkeys_after_revoke: serde_json::Value =
        auth(http.get(format!("{base}/me/passkeys")), &token_via_b)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(passkeys_after_revoke.as_array().unwrap().len(), 1);

    // Still logged in via the surviving passkey's session — revoking the
    // *other* passkey never touched this session.
    let me_still: serde_json::Value = auth(http.get(format!("{base}/me")), &token_via_b)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me_still["identity_id"], identity_id.to_string());

    // Revoking the last remaining passkey without ?confirm=true is
    // rejected — the ticket's explicit-confirmation invariant.
    let unconfirmed_status = auth(
        http.post(format!("{base}/me/passkeys/{second_passkey_id}/revoke")),
        &token_via_b,
    )
    .send()
    .await
    .unwrap()
    .status();
    assert_eq!(unconfirmed_status.as_u16(), 409);

    let confirmed_status = auth(
        http.post(format!(
            "{base}/me/passkeys/{second_passkey_id}/revoke?confirm=true"
        )),
        &token_via_b,
    )
    .send()
    .await
    .unwrap()
    .status();
    assert!(confirmed_status.is_success());

    let passkeys_after_final_revoke =
        sqlx::query("SELECT COUNT(*) AS count FROM identity_keys WHERE identity_id = $1")
            .bind(identity_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let remaining: i64 = sqlx::Row::try_get(&passkeys_after_final_revoke, "count").unwrap();
    assert_eq!(remaining, 0);
}
