//! Exercises issue #523 (Part 1 of #521's decision) against a real, running
//! `avalon-server` and Postgres: registering or revoking a passkey must
//! populate `indexer_identity_passkeys` — the projection a mirror-only node
//! reconstructs the same way from replayed history — not just
//! `identity_keys`, this (authoring) node's own local source of truth.
//! Gated `--ignored`, same convention as `crates/server/tests/passkeys.rs`,
//! which this file borrows its WebAuthn-ceremony helpers' shape from.

use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::CredentialCreationOptions;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
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

type VirtualClient =
    Client<MemoryStore, MockUserValidationMethod, public_suffix::PublicSuffixList, ()>;

/// `expected_ceremonies` must equal exactly how many WebAuthn ceremonies
/// (registration and/or authentication) this client will go through over
/// its lifetime — `MockUserValidationMethod::verified_user`'s count is a
/// hard expectation, not an upper bound, and panics on drop if unmet.
fn new_virtual_client(expected_ceremonies: usize) -> VirtualClient {
    let authenticator = Authenticator::new(
        Aaguid::new_empty(),
        MemoryStore::new(),
        MockUserValidationMethod::verified_user(expected_ceremonies),
    );
    Client::new(authenticator).allows_insecure_localhost(true)
}

/// Registers a brand-new identity with exactly one passkey, entirely via
/// real HTTP — same ceremony `crates/server/tests/passkeys.rs`'s own
/// `create_identity_with_one_passkey` drives. `expected_ceremonies` is
/// passed straight through to [`new_virtual_client`] — pass `2` if the
/// caller will also log in with the returned client afterward, `1` if
/// registration is the only ceremony it will ever perform.
async fn create_identity_with_one_passkey(
    http: &reqwest::Client,
    base: &str,
    expected_ceremonies: usize,
) -> (Uuid, VirtualClient) {
    let identity_id = Uuid::new_v4();
    let display_name = format!("passkey-durability-test-{identity_id}");

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

    let mut client = new_virtual_client(expected_ceremonies);
    let origin = rp_origin();
    let creation_options: CredentialCreationOptions =
        serde_json::from_value(start["challenge"].clone()).unwrap();
    let credential = client
        .register(Origin::from(&origin), creation_options, DefaultClientData)
        .await
        .expect("virtual authenticator registration should succeed");

    use ed25519_dalek::{Signer, SigningKey};
    let signing_key = SigningKey::generate(&mut rand::rng());
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

async fn active_mirrored_passkey_count(pool: &PgPool, identity_id: Uuid) -> i64 {
    sqlx::query(
        "SELECT COUNT(*) AS count FROM indexer_identity_passkeys \
         WHERE identity_id = $1 AND revoked_at IS NULL",
    )
    .bind(identity_id)
    .fetch_one(pool)
    .await
    .unwrap()
    .try_get("count")
    .unwrap()
}

/// The very first passkey — written by `handlers::register_finish`, not
/// `passkeys::register_finish` — must be just as durable/mirrored as any
/// later one, so a freshly-created identity is portable from the start.
#[tokio::test]
#[ignore]
async fn the_first_passkey_at_registration_populates_the_mirror_projection() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, _client) = create_identity_with_one_passkey(&http, &base, 1).await;

    let live_row =
        sqlx::query("SELECT credential_id, passkey_data FROM identity_keys WHERE identity_id = $1")
            .bind(identity_id)
            .fetch_one(&pool)
            .await
            .expect("identity_keys row should exist");
    let live_credential_id: Vec<u8> = live_row.try_get("credential_id").unwrap();
    let live_passkey_data: serde_json::Value = live_row.try_get("passkey_data").unwrap();

    let mirrored_row = sqlx::query(
        "SELECT credential_id, passkey_data, revoked_at FROM indexer_identity_passkeys \
         WHERE identity_id = $1",
    )
    .bind(identity_id)
    .fetch_one(&pool)
    .await
    .expect("indexer_identity_passkeys row should exist for the very first passkey");
    let mirrored_credential_id: Vec<u8> = mirrored_row.try_get("credential_id").unwrap();
    let mirrored_passkey_data: serde_json::Value = mirrored_row.try_get("passkey_data").unwrap();
    let revoked_at: Option<time::OffsetDateTime> = mirrored_row.try_get("revoked_at").unwrap();

    assert_eq!(
        mirrored_credential_id, live_credential_id,
        "the mirror projection must carry the exact same credential_id as identity_keys"
    );
    assert_eq!(
        mirrored_passkey_data, live_passkey_data,
        "the mirror projection must carry the exact same public credential material"
    );
    assert!(revoked_at.is_none(), "a fresh passkey must not be revoked");
}

/// A second, authenticated-flow passkey registration and its later
/// revocation must both reach `indexer_identity_passkeys` — not just the
/// first-passkey path exercised above.
#[tokio::test]
#[ignore]
async fn registering_and_revoking_a_second_passkey_updates_the_mirror_projection() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (identity_id, mut client_a) = create_identity_with_one_passkey(&http, &base, 2).await;
    assert_eq!(active_mirrored_passkey_count(&pool, identity_id).await, 1);

    // Log in with the first passkey to get a session, then register a
    // second — same authenticated flow `passkeys.rs`'s own test drives.
    use passkey_types::webauthn::CredentialRequestOptions;

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
    let assertion = client_a
        .authenticate(Origin::from(&origin), request_options, DefaultClientData)
        .await
        .expect("virtual authenticator authentication should succeed");
    let finish_body = serde_json::json!({ "ticket_id": ticket_id, "credential": assertion });
    let finish: serde_json::Value = http
        .post(format!("{base}/sessions/finish"))
        .json(&finish_body)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = finish["token"].as_str().unwrap().to_string();

    let start: serde_json::Value = http
        .post(format!("{base}/me/passkeys/register/start"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ticket_id = start["ticket_id"].as_str().unwrap().to_string();
    let mut client_b = new_virtual_client(1);
    let creation_options: CredentialCreationOptions =
        serde_json::from_value(start["challenge"].clone()).unwrap();
    let credential = client_b
        .register(Origin::from(&origin), creation_options, DefaultClientData)
        .await
        .expect("virtual authenticator registration should succeed");
    let finish_body = serde_json::json!({
        "ticket_id": ticket_id,
        "webauthn_credential": credential,
        "label": "mirrored second device",
    });
    let added: serde_json::Value = http
        .post(format!("{base}/me/passkeys/register/finish"))
        .bearer_auth(&token)
        .json(&finish_body)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let second_passkey_id = added["id"].as_str().unwrap().to_string();

    assert_eq!(
        active_mirrored_passkey_count(&pool, identity_id).await,
        2,
        "the mirror projection must reflect the second passkey too"
    );

    let revoke_status = http
        .post(format!("{base}/me/passkeys/{second_passkey_id}/revoke"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .status();
    assert!(revoke_status.is_success());

    assert_eq!(
        active_mirrored_passkey_count(&pool, identity_id).await,
        1,
        "revoking must mark the mirror projection's row revoked, not just delete identity_keys'"
    );

    let revoked_row =
        sqlx::query("SELECT revoked_at FROM indexer_identity_passkeys WHERE passkey_id = $1::uuid")
            .bind(&second_passkey_id)
            .fetch_one(&pool)
            .await
            .expect("the revoked passkey's mirror row must still exist, soft-revoked");
    let revoked_at: Option<time::OffsetDateTime> = revoked_row.try_get("revoked_at").unwrap();
    assert!(revoked_at.is_some(), "revoked_at must be set once revoked");
}
