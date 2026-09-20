//! Exercises epic #623 / issue #656's real gap: `POST /auth/cross-node/submit`
//! completing for an identity whose signing-key projection isn't already
//! local to the node being logged into, via #635's locator (real DHT) plus
//! #636's cross-shard fetch-and-verify (a real inclusion proof against a
//! signed STH). Gated `--ignored` — needs **two** real, separately-running
//! `avalon-server` processes with genuinely separate database state (not
//! the usual "two local processes sharing one Postgres" pattern this
//! crate's other live tests use — sharing one DB would mean both
//! processes see the same `identity_signing_keys` row directly, which
//! defeats the entire point of this test: proving the *local lookup
//! genuinely misses* and the cross-shard fallback is what actually
//! resolves it).
//!
//! Setup — two isolated Postgres schemas under the same real database,
//! bootstrapped to each other over a real libp2p DHT, both on this
//! sandbox's own `avalon-dev-local` network (so both nodes' STHs verify
//! against `docs/trusted-networks.json`'s one pinned `verify_key`, which
//! means both must sign with the *same* `AVALON_SETTLEMENT_SIGNING_KEY`
//! this sandbox's `.env` already has — an independent ledger with an
//! unrelated key would never verify against that pin):
//!
//! ```text
//! # one-time schema setup (see crates/server/examples/create_test_schemas.rs,
//! # a throwaway helper not meant to ship — delete after use)
//! DATABASE_URL='...?options=-c%20search_path%3Dtest_cross_shard_node_a' make migrate
//! DATABASE_URL='...?options=-c%20search_path%3Dtest_cross_shard_node_b' make migrate
//!
//! # node A (the identity's real owner)
//! DATABASE_URL='...?options=-c%20search_path%3Dtest_cross_shard_node_a' \
//! AVALON_SERVER_ADDR=127.0.0.1:8091 AVALON_NODE_URL=http://127.0.0.1:8091 \
//! AVALON_BOOTSTRAP_PEERS= AVALON_SETTLEMENT_REMOTE_URLS= AVALON_KNOWN_SHARDS= \
//! AVALON_SETTLEMENT_SUBMIT_KEY= AVALON_IDENTITY_LOCATOR_SCAN_INTERVAL_SECS=2 \
//! AVALON_DHT_ENABLED=true cargo run -p avalon-server
//!
//! # node B (the verifier — never sees node A's identity locally)
//! DATABASE_URL='...?options=-c%20search_path%3Dtest_cross_shard_node_b' \
//! AVALON_SERVER_ADDR=127.0.0.1:8092 AVALON_NODE_URL=http://127.0.0.1:8092 \
//! AVALON_BOOTSTRAP_PEERS=http://127.0.0.1:8091 AVALON_SETTLEMENT_REMOTE_URLS= \
//! AVALON_KNOWN_SHARDS= AVALON_SETTLEMENT_SUBMIT_KEY= AVALON_DHT_ENABLED=true \
//! cargo run -p avalon-server
//!
//! CROSS_SHARD_NODE_A_URL=http://127.0.0.1:8091 AVALON_SERVER_URL=http://127.0.0.1:8092 \
//!   cargo test -p avalon-server --test cross_node_login_cross_shard -- --ignored --test-threads=1
//! ```

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::{CredentialCreationOptions, CredentialRequestOptions};
use serde_json::Value;
use uuid::Uuid;

/// Node B — the one exposing `/auth/cross-node/*`, the one this test
/// actually drives `submit` against.
fn verifier_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8092".to_string())
}

/// Node A — where the identity is genuinely registered, and the only node
/// this test ever talks to for registration/login.
fn identity_owner_url() -> String {
    std::env::var("CROSS_SHARD_NODE_A_URL").unwrap_or_else(|_| "http://127.0.0.1:8091".to_string())
}

fn webauthn_origin() -> String {
    std::env::var("AVALON_WEBAUTHN_ORIGIN").unwrap_or_else(|_| "http://localhost:8080".to_string())
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

/// Registers a brand-new identity on node A (the owner) and logs it in
/// once, purely to fetch its real `signing_key_id` via `GET /me/devices` —
/// same shape `crates/sdk/tests/cross_node_login.rs`'s own
/// `register_identity` already establishes for the exact same reason.
async fn register_identity_on_owner(
    http: &reqwest::Client,
    display_name: &str,
) -> (Uuid, SigningKey, Uuid) {
    let base = identity_owner_url();
    let identity_id = Uuid::new_v4();
    let origin_url =
        url::Url::parse(&webauthn_origin()).expect("AVALON_WEBAUTHN_ORIGIN must be a valid URL");

    let signing_key = SigningKey::generate(&mut rand::rng());
    let event_signing_public_key = BASE64.encode(signing_key.verifying_key().to_bytes());

    let mut client = new_virtual_client();

    let start: Value = http
        .post(format!("{base}/identities/register/start"))
        .json(&serde_json::json!({ "identity_id": identity_id, "display_name": display_name }))
        .send()
        .await
        .expect("register/start on node A failed — is node A running?")
        .json()
        .await
        .unwrap();
    let ticket_id = start["ticket_id"].as_str().unwrap().to_string();
    let creation_options: CredentialCreationOptions =
        serde_json::from_value(start["challenge"].clone()).unwrap();
    let webauthn_credential = client
        .register(
            Origin::from(&origin_url),
            creation_options,
            DefaultClientData,
        )
        .await
        .expect("WebAuthn registration ceremony failed");

    let signing_bytes_for_creation =
        format!("avalon:identity.created:v1:{identity_id}:{display_name}").into_bytes();
    let signature = signing_key.sign(&signing_bytes_for_creation);

    http.post(format!("{base}/identities/register/finish"))
        .json(&serde_json::json!({
            "ticket_id": ticket_id,
            "webauthn_credential": webauthn_credential,
            "event_signing_public_key": event_signing_public_key,
            "event_signature": BASE64.encode(signature.to_bytes()),
            "device_label": null,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .expect("register/finish on node A should succeed");

    let session_start: Value = http
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
        .authenticate(
            Origin::from(&origin_url),
            request_options,
            DefaultClientData,
        )
        .await
        .expect("WebAuthn authentication ceremony failed");
    let session_finish: Value = http
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
    let session_token = session_finish["token"].as_str().unwrap().to_string();

    let devices: Value = http
        .get(format!("{base}/me/devices"))
        .bearer_auth(&session_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let signing_key_id: Uuid = devices.as_array().unwrap().first().unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    (identity_id, signing_key, signing_key_id)
}

/// Polls node A's own `GET /ledger/entries?subject=...` until the identity's
/// `identity.signing_key_added` event has actually landed in the real
/// hash-chained ledger, not just been durably enqueued. The outbox pattern
/// (`crates/server/src/outbox.rs`) writes every event to an `outbox` table
/// in the same transaction as the rest of registration, but a separate
/// background worker (`AVALON_OUTBOX_POLL_INTERVAL_SECS`, 3s by default)
/// is what actually appends it to `ledger_entries` and folds it into the
/// Merkle tree/STH — there's a real window, on the order of that poll
/// interval, where the event is durable but not yet ledger-visible or
/// cross-shard-fetchable. #635's locator propagates independently of the
/// outbox and can resolve before the outbox worker has caught up, so
/// waiting on the locator alone isn't sufficient here — this is a second,
/// separate real-world race the cross-shard fallback (#656) has to survive
/// in production too, just one this test needs to wait out rather than hit.
async fn wait_for_signing_key_ledger_entry(http: &reqwest::Client, identity_id: Uuid) {
    let owner = identity_owner_url();
    let subject = format!("identity:{identity_id}:self:signing_key_added");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let entries: Value = http
            .get(format!("{owner}/ledger/entries"))
            .query(&[
                ("subject", subject.as_str()),
                ("shard_id", "core"),
                ("limit", "10"),
            ])
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if entries.as_array().is_some_and(|arr| !arr.is_empty()) {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "node A's outbox worker never materialized the signing_key_added event into \
                 the real ledger within the wait budget — check node A's own logs"
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// Polls node B's own `GET /identities/{id}/locations` (#635) until it
/// resolves node A's real `base_url` — the real DHT propagation this test
/// depends on, not a fixed sleep. Panics with a clear message on timeout,
/// since a silent empty-locations result would otherwise look identical to
/// "hasn't propagated yet."
async fn wait_for_locator_propagation(http: &reqwest::Client, identity_id: Uuid) {
    let verifier = verifier_url();
    let owner = identity_owner_url();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let response: Value = http
            .get(format!("{verifier}/identities/{identity_id}/locations"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let locations: Vec<String> = response["locations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        if locations.iter().any(|l| l == &owner) {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "node B's own locator never resolved node A's base_url ({owner}) within the \
                 wait budget — real DHT propagation between the two live nodes never \
                 completed; check both processes' own logs"
            );
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// The headline case: node B has *never* seen this identity locally (a
/// genuinely separate database from node A's), yet a same-device-fast-path
/// grant submitted to node B still resolves to a real session — proving
/// issue #656's fallback (locator + cross-shard fetch-and-verify) actually
/// completes cross-node login end to end, not just in isolated unit tests.
#[tokio::test]
#[ignore]
async fn submit_completes_via_cross_shard_fallback_when_the_key_is_not_local() {
    let http = reqwest::Client::new();
    let display_name = format!("cross-shard-login-test-{}", Uuid::new_v4());
    let (identity_id, signing_key, signing_key_id) =
        register_identity_on_owner(&http, &display_name).await;

    wait_for_locator_propagation(&http, identity_id).await;
    wait_for_signing_key_ledger_entry(&http, identity_id).await;

    let verifier = verifier_url();
    let issued_at = time::OffsetDateTime::now_utc();
    let expires_at = issued_at + time::Duration::seconds(60);
    let nonce = Uuid::new_v4();
    let bytes = avalon_protocol::cross_node_login::signing_bytes(
        identity_id,
        signing_key_id,
        &verifier,
        &verifier,
        nonce,
        issued_at,
        expires_at,
    );
    let signature = signing_key.sign(&bytes);
    let grant = avalon_protocol::cross_node_login::CrossNodeLoginGrant {
        identity_id,
        signing_key_id,
        destination_base_url: verifier.clone(),
        requesting_context: verifier.clone(),
        nonce,
        issued_at,
        expires_at,
        signature: hex::encode(signature.to_bytes()),
    };

    let submit = http
        .post(format!("{verifier}/auth/cross-node/submit"))
        .json(&serde_json::json!({ "grant": grant }))
        .send()
        .await
        .unwrap();
    assert!(
        submit.status().is_success(),
        "cross-shard fallback should have completed verification: {:?} {}",
        submit.status(),
        submit.text().await.unwrap_or_default()
    );
    let body: Value = submit.json().await.unwrap();
    let token = body["token"]
        .as_str()
        .expect("submit response should carry a real session token");

    let me: Value = http
        .get(format!("{verifier}/me"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .expect("the minted session should authenticate against node B")
        .json()
        .await
        .unwrap();
    assert_eq!(me["identity_id"], identity_id.to_string());
    assert_eq!(me["display_name"], display_name);
}
