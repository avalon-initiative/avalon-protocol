//! Issue #510: `display_name` itself is the globally-unique, case-insensitive
//! handle — no discriminator suffix, no auto-suggested variant, a taken
//! name is a hard rejection. Gated `--ignored` since it needs live infra —
//! see `make test-live` / `make start`.
//!
//! Drives real identities through the actual WebAuthn ceremony (same
//! approach `crates/server/tests/passkeys.rs`/`rebuild_from_events.rs`
//! already take) rather than the SQL-seeding shortcut most other feature
//! tests use — this file's whole point is exercising the real
//! registration/rename write paths, not just reading back seeded state.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::{CredentialCreationOptions, CredentialRequestOptions};
use uuid::Uuid;

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

/// `expected_checks`: registration's `make_credential` and login's
/// `get_assertion` each check user presence once — a client only ever
/// used for registration (never logged back in with) needs `1`; one
/// reused for both needs `2`. `MockUserValidationMethod`'s `Drop` impl
/// asserts the exact count, so this must match how the returned client is
/// actually used by the caller.
fn new_virtual_client(expected_checks: usize) -> VirtualClient {
    let authenticator = Authenticator::new(
        Aaguid::new_empty(),
        MemoryStore::new(),
        MockUserValidationMethod::verified_user(expected_checks),
    );
    Client::new(authenticator).allows_insecure_localhost(true)
}

/// Just the `register/start` step, returning the raw response so a caller
/// can assert on its status directly (a taken-name test needs to see the
/// rejection here, before any WebAuthn ceremony happens).
async fn register_start(
    http: &reqwest::Client,
    base: &str,
    identity_id: Uuid,
    display_name: &str,
) -> reqwest::Response {
    http.post(format!("{base}/identities/register/start"))
        .json(&serde_json::json!({ "identity_id": identity_id, "display_name": display_name }))
        .send()
        .await
        .expect("register/start failed — is `make start` running?")
}

/// Full registration through `register/finish`, for a caller that already
/// knows `register/start` will succeed (a genuinely available name).
/// Returns the virtual client used for the ceremony too (`None` if
/// `register/start` itself failed, since the mock authenticator is never
/// touched in that case), so a caller that needs to log back in afterward
/// can reuse the same credential — a fresh client has an empty store and
/// can't authenticate as this identity. The client is only constructed
/// once `register/start` is known to have succeeded — `MockUserValidationMethod`
/// asserts its exact call count on drop, so one must never be created and
/// then left unused by an early return.
async fn register(
    http: &reqwest::Client,
    base: &str,
    display_name: &str,
    expected_checks: usize,
) -> (Uuid, Option<VirtualClient>, StatusResult) {
    let identity_id = Uuid::new_v4();
    let start_response = register_start(http, base, identity_id, display_name).await;
    if !start_response.status().is_success() {
        return (
            identity_id,
            None,
            StatusResult::StartFailed(start_response.status()),
        );
    }
    let start: serde_json::Value = start_response.json().await.unwrap();
    let ticket_id = start["ticket_id"].as_str().unwrap().to_string();

    let mut client = new_virtual_client(expected_checks);
    let origin = rp_origin();
    let creation_options: CredentialCreationOptions =
        serde_json::from_value(start["challenge"].clone()).unwrap();
    let credential = client
        .register(Origin::from(&origin), creation_options, DefaultClientData)
        .await
        .expect("virtual authenticator registration should succeed");

    let signing_key = SigningKey::generate(&mut rand::rng());
    let signing_bytes =
        format!("avalon:identity.created:v1:{identity_id}:{display_name}").into_bytes();
    let signature = signing_key.sign(&signing_bytes);

    let finish_response = http
        .post(format!("{base}/identities/register/finish"))
        .json(&serde_json::json!({
            "ticket_id": ticket_id,
            "webauthn_credential": credential,
            "event_signing_public_key": BASE64.encode(signing_key.verifying_key().to_bytes()),
            "event_signature": BASE64.encode(signature.to_bytes()),
            "device_label": null,
        }))
        .send()
        .await
        .unwrap();

    if finish_response.status().is_success() {
        (identity_id, Some(client), StatusResult::Finished)
    } else {
        (
            identity_id,
            Some(client),
            StatusResult::FinishFailed(finish_response.status()),
        )
    }
}

#[derive(Debug)]
enum StatusResult {
    Finished,
    StartFailed(reqwest::StatusCode),
    FinishFailed(reqwest::StatusCode),
}

async fn login(
    http: &reqwest::Client,
    base: &str,
    identity_id: Uuid,
    client: &mut VirtualClient,
) -> String {
    let start: serde_json::Value = http
        .post(format!("{base}/sessions/start"))
        .json(&serde_json::json!({ "identity_id": identity_id }))
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

    let finish: serde_json::Value = http
        .post(format!("{base}/sessions/finish"))
        .json(&serde_json::json!({ "ticket_id": ticket_id, "credential": assertion }))
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

#[tokio::test]
#[ignore]
async fn registering_a_taken_display_name_is_rejected_at_register_start() {
    let http = reqwest::Client::new();
    let base = server_url();
    let name = format!("taken-name-{}", Uuid::new_v4());

    let (_first_id, _client, result) = register(&http, &base, &name, 1).await;
    assert!(
        matches!(result, StatusResult::Finished),
        "the first registration for a fresh name must succeed: {result:?}"
    );

    // Same name, different case — case-insensitive uniqueness.
    let second_id = Uuid::new_v4();
    let response = register_start(&http, &base, second_id, &name.to_uppercase()).await;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["code"], "DISPLAY_NAME_TAKEN");
}

#[tokio::test]
#[ignore]
async fn renaming_to_a_taken_display_name_is_rejected_cleanly() {
    let http = reqwest::Client::new();
    let base = server_url();
    let name_a = format!("owner-name-{}", Uuid::new_v4());
    let name_b = format!("other-name-{}", Uuid::new_v4());

    // name_a's client is reused for login below (2 checks); name_b's
    // never logs in (1 check, registration only).
    let (id_a, client_a, result_a) = register(&http, &base, &name_a, 2).await;
    assert!(matches!(result_a, StatusResult::Finished), "{result_a:?}");
    let (_id_b, _client_b, result_b) = register(&http, &base, &name_b, 1).await;
    assert!(matches!(result_b, StatusResult::Finished), "{result_b:?}");

    let mut client_a = client_a.expect("registration succeeded, client must exist");
    let token_a = login(&http, &base, id_a, &mut client_a).await;

    let update = http
        .patch(format!("{base}/me"))
        .bearer_auth(&token_a)
        .json(&serde_json::json!({ "display_name": name_b }))
        .send()
        .await
        .expect("PATCH /me failed");
    assert_eq!(update.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = update.json().await.unwrap();
    assert_eq!(body["code"], "DISPLAY_NAME_TAKEN");

    // A's own display_name must be unchanged.
    let me: serde_json::Value = http
        .get(format!("{base}/me"))
        .bearer_auth(&token_a)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["display_name"], name_a);
}

#[tokio::test]
#[ignore]
async fn two_concurrent_registrations_for_the_same_name_one_wins_one_is_rejected() {
    let http_a = reqwest::Client::new();
    let http_b = reqwest::Client::new();
    let base = server_url();
    let name = format!("racing-name-{}", Uuid::new_v4());

    let ((_, _, result_a), (_, _, result_b)) = tokio::join!(
        register(&http_a, &base, &name, 1),
        register(&http_b, &base, &name, 1)
    );

    let finished = [&result_a, &result_b]
        .iter()
        .filter(|r| matches!(r, StatusResult::Finished))
        .count();
    let rejected = [&result_a, &result_b]
        .iter()
        .filter(|r| {
            matches!(
                r,
                StatusResult::StartFailed(reqwest::StatusCode::CONFLICT)
                    | StatusResult::FinishFailed(reqwest::StatusCode::CONFLICT)
            )
        })
        .count();

    assert_eq!(
        finished, 1,
        "exactly one concurrent registration for the same name must succeed: {result_a:?} {result_b:?}"
    );
    assert_eq!(
        rejected, 1,
        "the other must be cleanly rejected, not corrupt state or 500: {result_a:?} {result_b:?}"
    );
}
