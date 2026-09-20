//! Exercises epic #623 / issue #649's requester-verification resolution
//! (`crate::cross_node_login::resolve_requester_verification`,
//! surfaced via `GET /auth/cross-node/lookup`'s `integrator_verified`/
//! `display_name` fields) against a real, running `avalon-server` and
//! Postgres. Gated `--ignored`, same convention as every other live test
//! in this crate — but unlike most of them, this file needs the server
//! started with a **non-default** `AVALON_OWN_SHARD_ID`, since #649's
//! "owned shard" resolution path is keyed on exactly that:
//!
//! ```text
//! AVALON_OWN_SHARD_ID=game:cross-node-login-verify-test make start
//! ```
//!
//! The unowned-shard ("core") resolution path's pure logic
//! (`is_verified_seed_node`) is already unit-tested directly in
//! `crates/server/src/cross_node_login.rs` — this file only needs to prove
//! the "core" branch's live wiring (this sandbox's own
//! `docs/trusted-networks.json` entry has an empty `seed_nodes` list, so
//! it can only ever exercise the "not verified" branch live; the "is
//! verified" branch is what the unit tests cover with a controlled anchor
//! list).

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::Value;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

/// Must match whatever `AVALON_OWN_SHARD_ID=game:<slug>` the running
/// server was actually started with — see this file's own module doc
/// comment.
const OWNED_SHARD_SLUG: &str = "cross-node-login-verify-test";

struct RegisteredIntegrator {
    slug: String,
    name: String,
    root_key_id: String,
    root_signing_key: SigningKey,
}

/// Registers a fresh integrator under `OWNED_SHARD_SLUG` — same shape
/// `crates/server/tests/shard_trust_anchors.rs::register_integrator`
/// already establishes, duplicated here (not imported — integration test
/// binaries don't share code with each other) with a fixed slug rather
/// than a random one, since it has to match the running server's own
/// `AVALON_OWN_SHARD_ID`.
async fn register_owned_shard_integrator(http: &reqwest::Client) -> RegisteredIntegrator {
    let base = server_url();
    let name = format!("Cross-Node Login Verify Test {}", Uuid::new_v4().simple());
    let root_signing_key = SigningKey::generate(&mut rand::rng());
    let body = serde_json::json!({
        "slug": OWNED_SHARD_SLUG,
        "name": name,
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
        name,
        root_key_id: registered["credential"]["key_id"]
            .as_str()
            .unwrap()
            .to_string(),
        root_signing_key,
    }
}

async fn register_shard_settlement_key(http: &reqwest::Client, integrator: &RegisteredIntegrator) {
    let base = server_url();
    let challenge: Value = http
        .post(format!("{base}/integrations/{}/challenge", integrator.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap().to_string();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let signature = integrator.root_signing_key.sign(&nonce);

    let shard_key = SigningKey::generate(&mut rand::rng());
    let add = http
        .post(format!("{base}/integrations/{}/keys", integrator.slug))
        .header("x-avalon-integrator-key-id", &integrator.root_key_id)
        .header("x-avalon-integrator-challenge-id", &challenge_id)
        .header(
            "x-avalon-integrator-signature",
            BASE64.encode(signature.to_bytes()),
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
}

async fn lookup(http: &reqwest::Client, user_code: &str) -> Value {
    let base = server_url();
    http.get(format!("{base}/auth/cross-node/lookup"))
        .query(&[("user_code", user_code)])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// The headline case: before the matching integrator/shard_settlement key
/// exists, `lookup` reports unverified; once both are registered, the
/// *same running node* (no restart) reports verified with the real
/// registered name — proving `resolve_requester_verification`'s "owned
/// shard" branch resolves against live data, not a snapshot taken at
/// startup.
#[tokio::test]
#[ignore]
async fn owned_shard_verification_flips_true_once_the_integrator_registers_its_key() {
    let http = reqwest::Client::new();
    let base = server_url();

    let start: Value = http
        .post(format!("{base}/auth/cross-node/start"))
        .send()
        .await
        .expect("register/start failed — is `make start` running with AVALON_OWN_SHARD_ID=game:cross-node-login-verify-test?")
        .json()
        .await
        .unwrap();
    let user_code = start["user_code"].as_str().unwrap().to_string();

    let before = lookup(&http, &user_code).await;
    assert_eq!(
        before["integrator_verified"], false,
        "no integrator registered yet for {OWNED_SHARD_SLUG} — must not be verified"
    );
    assert!(before.get("display_name").is_none());

    let integrator = register_owned_shard_integrator(&http).await;
    register_shard_settlement_key(&http, &integrator).await;

    let after = lookup(&http, &user_code).await;
    assert_eq!(after["integrator_verified"], true);
    assert_eq!(after["display_name"].as_str().unwrap(), integrator.name);
}

/// The default, unowned ("core") shard is never verified in this sandbox
/// — its own `docs/trusted-networks.json` entry has an empty `seed_nodes`
/// list, so there is genuinely no anchor to match. Run this against the
/// *normal*, default-config server (no `AVALON_OWN_SHARD_ID` override) —
/// it will fail against the specially-configured server the test above
/// needs, since that server's own shard isn't `"core"` at all.
#[tokio::test]
#[ignore]
async fn core_shard_is_unverified_in_this_sandbox() {
    let http = reqwest::Client::new();
    let base = server_url();

    let start: Value = http
        .post(format!("{base}/auth/cross-node/start"))
        .send()
        .await
        .expect("register/start failed — is `make start` running?")
        .json()
        .await
        .unwrap();
    let user_code = start["user_code"].as_str().unwrap().to_string();

    let looked_up = lookup(&http, &user_code).await;
    assert_eq!(looked_up["integrator_verified"], false);
    assert!(looked_up.get("display_name").is_none());
}
