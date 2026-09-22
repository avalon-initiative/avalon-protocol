//! Exercises epic #623 / issue #636's cross-shard fetch-and-verify
//! primitive against a real, running `avalon-server` and Postgres. Gated
//! `--ignored`, same convention as every other live test in this crate.
//!
//! This sandbox only ever runs one real `avalon-server` process against
//! this database (same limitation `crates/server/tests/mirror_watcher.rs`'s
//! own module doc already notes for a different feature) — every test
//! here fetches from that one node's own `"core"` shard, treating it
//! exactly as a genuinely remote shard would be treated: nothing in
//! `cross_shard_fetch::fetch_verified_entries` distinguishes "this node"
//! from "some other node," it only ever talks HTTP to `base_url`. `"core"`
//! has no per-integrator issuer key (`resolve_shard_verify_keys_from_db`
//! only ever resolves `"{namespace}:{owner}"`-shaped shard ids), so these
//! tests supply `AVALON_SETTLEMENT_VERIFY_KEY` (already configured in this
//! sandbox's `.env`, the same key the running server signs its own STHs
//! with) as the static verify key — the real trust anchor a genuine
//! `"core"`-shard consumer would also need to already know.

use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::CredentialCreationOptions;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

use avalon_server::cross_shard_fetch::{fetch_verified_entries, CrossShardFetchError};

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn rp_origin() -> url::Url {
    let origin = std::env::var("AVALON_WEBAUTHN_ORIGIN")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    url::Url::parse(&origin).expect("AVALON_WEBAUTHN_ORIGIN must be a valid URL")
}

fn network_id() -> String {
    std::env::var("AVALON_NETWORK_ID").unwrap_or_else(|_| "avalon-dev-local".to_string())
}

async fn test_pool() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

/// This sandbox's real settlement verify key, as the running server itself
/// signs its own `"core"`-shard STHs with — the trust anchor these tests
/// supply as `static_verify_keys["core"]`, same as a genuine consumer of a
/// shared Layer-1 shard would need to already know out-of-band.
fn core_verify_keys() -> HashMap<String, ed25519_dalek::VerifyingKey> {
    let key = avalon_protocol::sth::load_verify_key_from_env()
        .expect("AVALON_SETTLEMENT_VERIFY_KEY must be set to run this test");
    HashMap::from([("core".to_string(), key)])
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

/// Registers a brand-new identity, returning its id, its subject string
/// (`identity:{id}:self:signing_key_added`, the same `GlobalId` shape
/// `crate::devices::identity_ref` produces server-side), and the real
/// `SigningKey` used for `identity.created` — enough to produce a real,
/// durable `identity.signing_key_added` ledger entry to fetch and verify.
async fn register_identity(http: &reqwest::Client, base: &str) -> (Uuid, String) {
    let identity_id = Uuid::new_v4();
    let display_name = format!("cross-shard-fetch-test-{identity_id}");

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

    let subject = format!("identity:{identity_id}:self:signing_key_added");
    (identity_id, subject)
}

/// The headline case: a freshly registered identity's real
/// `identity.signing_key_added` entry, fetched from a "remote" shard and
/// fully verified (signed STH, inclusion proof, recomputed entry_hash),
/// carries the right payload content.
#[tokio::test]
#[ignore]
async fn a_real_entry_is_fetched_and_verified_end_to_end() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;

    let (identity_id, subject) = register_identity(&http, &base).await;

    // Registration commits via the outbox (`crate::outbox::run_worker`,
    // default 3s poll), not synchronously — the ledger entry this test
    // fetches doesn't exist yet the instant register/finish returns, only
    // the (separately, directly-written) `identity_signing_keys`
    // projection does. Real wait for a real async commit, not a race.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let entries;
    loop {
        let attempt = fetch_verified_entries(
            &pool,
            &network_id(),
            "core",
            &base,
            &subject,
            &core_verify_keys(),
        )
        .await
        .expect("a freshly registered identity's signing_key_added entry should verify");
        if !attempt.is_empty() || std::time::Instant::now() >= deadline {
            entries = attempt;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    assert_eq!(
        entries.len(),
        1,
        "a fresh identity has exactly one signing key"
    );
    let entry = &entries[0];
    assert_eq!(entry.kind, "identity.signing_key_added");
    assert_eq!(entry.subject, subject);
    assert_eq!(entry.payload["identity_id"], serde_json::json!(identity_id));
}

/// A subject nobody ever registered verifies to an empty list — the
/// negative case, proving this isn't just always returning *something*.
#[tokio::test]
#[ignore]
async fn an_unknown_subject_returns_no_entries() {
    let pool = test_pool().await;
    let subject = format!("identity:{}:self:signing_key_added", Uuid::new_v4());

    let entries = fetch_verified_entries(
        &pool,
        &network_id(),
        "core",
        &server_url(),
        &subject,
        &core_verify_keys(),
    )
    .await
    .expect("an unknown subject should succeed with zero entries, not error");
    assert!(entries.is_empty());
}

/// No verify key resolves for a shard this test deliberately gives none
/// for — the STH fetch succeeds, but verification against an empty
/// key set must fail closed, never silently treated as trusted.
#[tokio::test]
#[ignore]
async fn a_shard_with_no_resolvable_verify_key_fails_closed() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_identity_id, subject) = register_identity(&http, &base).await;

    let result = fetch_verified_entries(
        &pool,
        &network_id(),
        "core",
        &base,
        &subject,
        &HashMap::new(),
    )
    .await;

    assert!(matches!(
        result,
        Err(CrossShardFetchError::SthVerificationFailed(_))
    ));
}

/// A verify key that's real but simply wrong for this shard (a fresh,
/// unrelated key) must also fail closed — proves this isn't just checking
/// "a key was supplied," but that the signature genuinely verifies against
/// it.
#[tokio::test]
#[ignore]
async fn a_wrong_verify_key_fails_closed() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_identity_id, subject) = register_identity(&http, &base).await;

    let wrong_key = SigningKey::generate(&mut rand::rng()).verifying_key();
    let wrong_keys = HashMap::from([("core".to_string(), wrong_key)]);

    let result =
        fetch_verified_entries(&pool, &network_id(), "core", &base, &subject, &wrong_keys).await;

    assert!(matches!(
        result,
        Err(CrossShardFetchError::SthVerificationFailed(_))
    ));
}
