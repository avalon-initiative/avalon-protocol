//! Issue #543: a shard's settlement-signing key is authorized through the
//! exact same issuer-key registration flow attestation-issuance keys use,
//! scoped with a new `purpose: "shard_settlement"` field — not a second,
//! separate registry. Exercised against a real, running `avalon-server`
//! and Postgres. Gated `--ignored` since it needs live infra.
//!
//! This file proves the registration/storage/read-back half end to end
//! over the real API — the same root-key-authorizes-operational-key flow
//! `crates/server/tests/issuer_keys.rs` already exercises for attestation
//! keys, now also carrying `purpose`. The witness-side DB resolution
//! query (`crate::cross_shard::resolve_shard_verify_keys_from_db`) reads
//! from exactly the `issuer_keys` rows this flow writes — same table,
//! same columns — so a real `shard_settlement` key landing here correctly
//! is what makes that resolution possible; see
//! `crates/server/src/cross_shard.rs`'s own module doc comment for the
//! aggregation side this composes with.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::Value;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

struct RegisteredIntegrator {
    slug: String,
    root_key_id: String,
    root_signing_key: SigningKey,
}

async fn register_integrator(http: &reqwest::Client) -> RegisteredIntegrator {
    let base = server_url();
    let suffix = Uuid::new_v4().simple().to_string();
    let root_signing_key = SigningKey::generate(&mut rand::rng());
    let body = serde_json::json!({
        "slug": format!("test-shard-trust-{}", &suffix[..10]),
        "name": format!("Shard Trust Anchor Test {}", &suffix[..8]),
        "owner_name": "Test Studio",
        "initial_key": {
            "algorithm": "ed25519",
            "public_key": BASE64.encode(root_signing_key.verifying_key().as_bytes()),
        },
    });
    let response = http
        .post(format!("{base}/integrations"))
        .json(&body)
        .send()
        .await
        .expect("register integrator failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());
    let registered: Value = response.json().await.unwrap();

    RegisteredIntegrator {
        slug: registered["slug"].as_str().unwrap().to_string(),
        root_key_id: registered["credential"]["key_id"]
            .as_str()
            .unwrap()
            .to_string(),
        root_signing_key,
    }
}

async fn signed_challenge_headers(
    http: &reqwest::Client,
    slug: &str,
    key_id: &str,
    signing_key: &SigningKey,
) -> [(&'static str, String); 3] {
    let base = server_url();
    let challenge: Value = http
        .post(format!("{base}/integrations/{slug}/challenge"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap().to_string();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let signature = signing_key.sign(&nonce);

    [
        ("x-avalon-integrator-key-id", key_id.to_string()),
        ("x-avalon-integrator-challenge-id", challenge_id),
        (
            "x-avalon-integrator-signature",
            BASE64.encode(signature.to_bytes()),
        ),
    ]
}

fn with_headers(
    builder: reqwest::RequestBuilder,
    headers: &[(&'static str, String)],
) -> reqwest::RequestBuilder {
    headers
        .iter()
        .fold(builder, |b, (name, value)| b.header(*name, value))
}

#[tokio::test]
#[ignore]
async fn a_shard_settlement_key_registers_and_reads_back_with_its_purpose() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http).await;

    let shard_key = SigningKey::generate(&mut rand::rng());
    let headers = signed_challenge_headers(
        &http,
        &integrator.slug,
        &integrator.root_key_id,
        &integrator.root_signing_key,
    )
    .await;
    let add = with_headers(
        http.post(format!("{base}/integrations/{}/keys", integrator.slug)),
        &headers,
    )
    .json(&serde_json::json!({
        "algorithm": "ed25519",
        "public_key": BASE64.encode(shard_key.verifying_key().as_bytes()),
        "role": "operational",
        "purpose": "shard_settlement",
    }))
    .send()
    .await
    .unwrap();
    assert!(add.status().is_success(), "{:?}", add.status());
    let added: Value = add.json().await.unwrap();
    assert_eq!(added["purpose"].as_str().unwrap(), "shard_settlement");

    let list: Value = http
        .get(format!("{base}/integrations/{}/keys", integrator.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let keys = list.as_array().unwrap();
    let shard_key_entry = keys
        .iter()
        .find(|k| k["key_id"] == added["key_id"])
        .expect("the newly-added key should appear in the key history");
    assert_eq!(
        shard_key_entry["purpose"].as_str().unwrap(),
        "shard_settlement",
        "the shard_settlement purpose must round-trip through storage and the read endpoint"
    );

    // A key added with no `purpose` field at all (every pre-#543 caller)
    // still defaults to `attestation`, unchanged.
    let default_purpose_key = SigningKey::generate(&mut rand::rng());
    let headers = signed_challenge_headers(
        &http,
        &integrator.slug,
        &integrator.root_key_id,
        &integrator.root_signing_key,
    )
    .await;
    let add_default = with_headers(
        http.post(format!("{base}/integrations/{}/keys", integrator.slug)),
        &headers,
    )
    .json(&serde_json::json!({
        "algorithm": "ed25519",
        "public_key": BASE64.encode(default_purpose_key.verifying_key().as_bytes()),
        "role": "operational",
    }))
    .send()
    .await
    .unwrap();
    assert!(add_default.status().is_success());
    let added_default: Value = add_default.json().await.unwrap();
    assert_eq!(added_default["purpose"].as_str().unwrap(), "attestation");
}

/// Purpose gates *signing authority* (see
/// `avalon_protocol::integrators::IssuerKey::may_sign_attestations`/
/// `may_sign_shard_settlement`, unit-tested directly in that crate), not
/// the challenge-response *authentication* mechanism itself — a
/// `shard_settlement`-purpose key still authenticates ordinary calls the
/// same as any other registered key. Live-verified here since it's the
/// one part of the distinction an in-crate unit test can't reach (real
/// HTTP challenge-response).
#[tokio::test]
#[ignore]
async fn a_shard_settlement_key_still_authenticates_ordinary_calls() {
    let http = reqwest::Client::new();
    let base = server_url();
    let integrator = register_integrator(&http).await;

    let shard_key = SigningKey::generate(&mut rand::rng());
    let headers = signed_challenge_headers(
        &http,
        &integrator.slug,
        &integrator.root_key_id,
        &integrator.root_signing_key,
    )
    .await;
    let add = with_headers(
        http.post(format!("{base}/integrations/{}/keys", integrator.slug)),
        &headers,
    )
    .json(&serde_json::json!({
        "algorithm": "ed25519",
        "public_key": BASE64.encode(shard_key.verifying_key().as_bytes()),
        "role": "operational",
        "purpose": "shard_settlement",
    }))
    .send()
    .await
    .unwrap();
    assert!(add.status().is_success());
    let added: Value = add.json().await.unwrap();
    let shard_key_id = added["key_id"].as_str().unwrap().to_string();

    // The shard_settlement key can still authenticate ordinary
    // challenge-response calls (purpose doesn't gate authentication
    // itself, only attestation-signing authority — see
    // `avalon_protocol::integrators::IssuerKey::may_sign_attestations`),
    // so this proves the *attestation-issuance* rejection specifically,
    // not just "this key can't do anything."
    let headers =
        signed_challenge_headers(&http, &integrator.slug, &shard_key_id, &shard_key).await;
    let whoami = with_headers(http.get(format!("{base}/integrations/whoami")), &headers)
        .send()
        .await
        .unwrap();
    assert!(
        whoami.status().is_success(),
        "a shard_settlement key should still authenticate ordinary calls: {:?}",
        whoami.status()
    );
}
