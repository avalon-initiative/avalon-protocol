//! Exercises epic #623 / issue #635's identity locator against a real,
//! running `avalon-server` and Postgres. Gated `--ignored`, same convention
//! as `crates/server/tests/cross_node_login.rs`, whose registration/signing
//! helpers this file borrows the shape of. See
//! `crates/server/tests/interest_dht.rs` for the DHT-layer-only round-trip
//! test (`InterestScope::Identity` put/get, no DB) that this file builds on
//! top of.
//!
//! Set a short `AVALON_IDENTITY_LOCATOR_SCAN_INTERVAL_SECS` (e.g. `2`) on
//! the running server before `make start` for this test to complete
//! quickly — see `crate::identity_locator`'s module doc comment.

use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::CredentialCreationOptions;
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

fn new_virtual_client() -> VirtualClient {
    let authenticator = Authenticator::new(
        Aaguid::new_empty(),
        MemoryStore::new(),
        MockUserValidationMethod::verified_user(1),
    );
    Client::new(authenticator).allows_insecure_localhost(true)
}

/// Registers a brand-new identity — just far enough through the flow
/// (register/start + register/finish) to produce a real
/// `indexer_identity_signing_keys` row, which is all `identity_locator`
/// scans for. Never logs in — this test doesn't need a session.
async fn register_identity(http: &reqwest::Client, base: &str) -> Uuid {
    let identity_id = Uuid::new_v4();
    let display_name = format!("identity-locator-test-{identity_id}");

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

    identity_id
}

/// The headline case: a freshly registered identity's signing key gets
/// picked up by `identity_locator::run_worker`'s next scan, registered as
/// real DHT interest, and shows up in `GET /identities/{id}/locations` —
/// including this node's own base URL, since a single-node run resolves
/// its own just-put record.
#[tokio::test]
#[ignore]
async fn a_freshly_registered_identity_becomes_resolvable_via_locations() {
    let http = reqwest::Client::new();
    let base = server_url();

    let identity_id = register_identity(&http, &base).await;

    // Real wait for identity_locator's next scan tick (see this file's own
    // module doc comment on configuring a short scan interval on the
    // running server) plus DHT propagation.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut locations = Vec::new();
    while std::time::Instant::now() < deadline {
        let response: serde_json::Value = http
            .get(format!("{base}/identities/{identity_id}/locations"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        locations = response["locations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        if !locations.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    assert!(
        !locations.is_empty(),
        "identity locator should have registered and resolved at least one location \
         within the wait budget — is AVALON_IDENTITY_LOCATOR_SCAN_INTERVAL_SECS set short \
         on the running server?"
    );
}

/// An identity nobody ever registered resolves to no locations — the
/// negative case, proving this isn't just always returning *something*.
#[tokio::test]
#[ignore]
async fn an_unknown_identity_resolves_to_no_locations() {
    let http = reqwest::Client::new();
    let base = server_url();

    let response: serde_json::Value = http
        .get(format!("{base}/identities/{}/locations", Uuid::new_v4()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let locations = response["locations"].as_array().unwrap();
    assert!(locations.is_empty());
}
