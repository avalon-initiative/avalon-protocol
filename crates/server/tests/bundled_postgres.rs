//! Live proof for issue #990's `avalon-server-bundled` binary
//! (`bundled-postgres` feature): boots with no `DATABASE_URL` set, serves a
//! real registration request, and its data survives a restart. Gated
//! `--ignored` since it needs a real, running `avalon-server-bundled`
//! process — deliberately not `make start` (this binary's whole point is
//! needing no operator-provided Postgres at all).
//!
//! This file does not spawn or kill the process itself — same convention
//! `crates/server/tests/gateway_only_deployment.rs` already established for
//! a manually-run pair of processes.
//!
//! ```text
//! # Terminal 1 — first boot, no DATABASE_URL, fresh data directory:
//! rm -rf /tmp/avalon-bundled-990-data
//! AVALON_DATA_DIR=/tmp/avalon-bundled-990-data \
//! AVALON_SERVER_ADDR=127.0.0.1:18190 AVALON_NODE_URL=http://127.0.0.1:18190 \
//! AVALON_NETWORK_ID=avalon-dev-local \
//! AVALON_WEBAUTHN_RP_ID=localhost AVALON_WEBAUTHN_ORIGIN=http://localhost:18190 \
//! AVALON_BOOTSTRAP_PEERS= AVALON_DHT_ENABLED=false \
//! cargo run -p avalon-server --bin avalon-server-bundled --features bundled-postgres
//!
//! # Terminal 2 — register a real identity end to end, over the bundled
//! # instance's own embedded Postgres:
//! AVALON_BUNDLED_SERVER_URL=http://127.0.0.1:18190 AVALON_WEBAUTHN_ORIGIN=http://localhost:18190 \
//! cargo test -p avalon-server --test bundled_postgres --features bundled-postgres -- --ignored before_restart
//!
//! # Terminal 1 — Ctrl-C (SIGINT). The log shows the embedded Postgres
//! # stopping cleanly, then start the same binary again, same
//! # AVALON_DATA_DIR, still no DATABASE_URL:
//! AVALON_DATA_DIR=/tmp/avalon-bundled-990-data \
//! AVALON_SERVER_ADDR=127.0.0.1:18190 AVALON_NODE_URL=http://127.0.0.1:18190 \
//! AVALON_NETWORK_ID=avalon-dev-local \
//! AVALON_WEBAUTHN_RP_ID=localhost AVALON_WEBAUTHN_ORIGIN=http://localhost:18190 \
//! AVALON_BOOTSTRAP_PEERS= AVALON_DHT_ENABLED=false \
//! cargo run -p avalon-server --bin avalon-server-bundled --features bundled-postgres
//!
//! # Terminal 2 — confirm the identity registered before the restart is
//! # still there (a second registration with the same display name must
//! # be rejected as taken, not accepted as if the database were empty):
//! AVALON_BUNDLED_SERVER_URL=http://127.0.0.1:18190 AVALON_WEBAUTHN_ORIGIN=http://localhost:18190 \
//! cargo test -p avalon-server --test bundled_postgres --features bundled-postgres -- --ignored after_restart
//! ```
//!
//! Live-verified in this environment against the real `avalon-server-bundled`
//! binary and its own embedded Postgres (see the PR body for the exact
//! session transcript).

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::CredentialCreationOptions;

/// A fixed display name (not randomized) — `before_restart` and
/// `after_restart` are separate `cargo test` invocations, run against a
/// process that gets restarted in between, so they can't share Rust-level
/// state; the identity uniqueness constraint enforced across the restart
/// is the shared state this test actually observes.
const DISPLAY_NAME: &str = "bundled-postgres-990-restart-proof";

fn server_url() -> String {
    std::env::var("AVALON_BUNDLED_SERVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:18190".to_string())
}

fn rp_origin() -> url::Url {
    let origin = std::env::var("AVALON_WEBAUTHN_ORIGIN")
        .unwrap_or_else(|_| "http://localhost:18190".to_string());
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

/// Registers a fresh identity under `DISPLAY_NAME` end to end
/// (`register/start` + a real virtual-authenticator WebAuthn ceremony +
/// `register/finish`), asserting every step succeeds — proof the bundled
/// instance's embedded Postgres serves a real write path, not just an
/// open connection.
#[tokio::test]
#[ignore]
async fn before_restart() {
    let http = reqwest::Client::new();
    let base = server_url();
    let signing_key = SigningKey::generate(&mut rand::rng());
    let identity_id =
        avalon_protocol::identity_id::derive_identity_id_for_key(&signing_key.verifying_key());

    let start_response = http
        .post(format!("{base}/identities/register/start"))
        .json(&serde_json::json!({ "identity_id": identity_id, "event_signing_public_key": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, signing_key.verifying_key().to_bytes()), "display_name": DISPLAY_NAME }))
        .send()
        .await
        .expect("register/start failed — is avalon-server-bundled running?");
    assert!(
        start_response.status().is_success(),
        "register/start should succeed against a fresh bundled instance: {:?}",
        start_response.status()
    );
    let start: serde_json::Value = start_response.json().await.unwrap();
    let ticket_id = start["ticket_id"].as_str().unwrap().to_string();

    let mut client = new_virtual_client();
    let creation_options: CredentialCreationOptions =
        serde_json::from_value(start["challenge"].clone()).unwrap();
    let credential = client
        .register(
            Origin::from(&rp_origin()),
            creation_options,
            DefaultClientData,
        )
        .await
        .expect("virtual authenticator registration should succeed");

    let signing_bytes = avalon_protocol::identity_id::identity_created_signing_bytes_v2(
        start["network_id"].as_str().unwrap(),
        ticket_id.parse().unwrap(),
        &identity_id,
        &signing_key.verifying_key().to_bytes(),
        DISPLAY_NAME,
    );
    let signature = signing_key.sign(&signing_bytes);

    let finish_response = http
        .post(format!("{base}/identities/register/finish"))
        .json(&serde_json::json!({
            "ticket_id": ticket_id,
            "webauthn_credential": credential,
            "event_signature": BASE64.encode(signature.to_bytes()),
            "device_label": null,
        }))
        .send()
        .await
        .unwrap();
    assert!(
        finish_response.status().is_success(),
        "register/finish should succeed: {:?}",
        finish_response.status()
    );
}

/// Run after restarting the process (same `AVALON_DATA_DIR`, still no
/// `DATABASE_URL`): a second registration attempt under the same
/// `DISPLAY_NAME` must be rejected as taken, proving the identity
/// `before_restart` created is still in the database, not a fresh empty one.
#[tokio::test]
#[ignore]
async fn after_restart() {
    let http = reqwest::Client::new();
    let base = server_url();
    let signing_key = SigningKey::generate(&mut rand::rng());
    let identity_id =
        avalon_protocol::identity_id::derive_identity_id_for_key(&signing_key.verifying_key());

    let start_response = http
        .post(format!("{base}/identities/register/start"))
        .json(&serde_json::json!({ "identity_id": identity_id, "event_signing_public_key": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, signing_key.verifying_key().to_bytes()), "display_name": DISPLAY_NAME }))
        .send()
        .await
        .expect("register/start failed — is avalon-server-bundled running again?");
    assert_eq!(
        start_response.status(),
        reqwest::StatusCode::CONFLICT,
        "the display name from before_restart should still be taken after a restart with the \
         same AVALON_DATA_DIR — the embedded Postgres's data directory did not survive"
    );
}
