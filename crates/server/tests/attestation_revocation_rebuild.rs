//! A revocation's ledger event carries the issuer's signature, and a full index rebuild from the
//! ledger projects it. Gated `--ignored`: needs a running `avalon-server` and Postgres, and rebuilds
//! the whole index, so it runs in the ledger-writers group.

use avalon_chain::PostgresSettlementProvider;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use reqwest::header::HeaderMap;
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

mod attestation_support;
mod common;
use attestation_support::{issue_bytes, now_micros, revoke_bytes};

struct RegisteredIntegrator {
    signing_key: SigningKey,
    slug: String,
    key_id: String,
}

async fn register_integrator(http: &reqwest::Client, base: &str) -> RegisteredIntegrator {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let slug = format!("test-revoke-{}", &suffix[..10]);
    let body = serde_json::json!({
        "slug": slug,
        "name": format!("Revoke Test {}", &suffix[..8]),
        "owner_name": "Test Studio",
        "requested_capabilities": ["achievements.issue"],
        "initial_key": {
            "algorithm": "ed25519",
            "public_key": BASE64.encode(signing_key.verifying_key().as_bytes()),
        },
    });
    let response = http
        .post(format!("{base}/integrations"))
        .json(&body)
        .send()
        .await
        .expect("register integrator failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());
    let registered: serde_json::Value = response.json().await.unwrap();
    RegisteredIntegrator {
        signing_key,
        slug,
        key_id: registered["credential"]["key_id"]
            .as_str()
            .unwrap()
            .to_string(),
    }
}

async fn auth_headers(
    http: &reqwest::Client,
    base: &str,
    integrator: &RegisteredIntegrator,
) -> HeaderMap {
    let challenge: serde_json::Value = http
        .post(format!("{base}/integrations/{}/challenge", integrator.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let signature = integrator.signing_key.sign(&nonce);

    let mut headers = HeaderMap::new();
    headers.insert(
        "x-avalon-integrator-key-id",
        integrator.key_id.parse().unwrap(),
    );
    headers.insert(
        "x-avalon-integrator-challenge-id",
        challenge_id.parse().unwrap(),
    );
    headers.insert(
        "x-avalon-integrator-signature",
        BASE64.encode(signature.to_bytes()).parse().unwrap(),
    );
    headers
}

/// Full setup shared by every test below: register an integrator, define an
/// achievement, connect + grant, issue it. Returns the integrator and the new
/// attestation's id.
async fn issue_one(
    http: &reqwest::Client,
    base: &str,
    pool: &PgPool,
) -> (RegisteredIntegrator, String, Uuid) {
    let (identity_id, token) = common::seed_identity_session(pool).await;
    let integrator = register_integrator(http, base).await;

    let headers = auth_headers(http, base, &integrator).await;
    let define = http
        .post(format!(
            "{base}/integrations/{}/achievements",
            integrator.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "key": "dragon_slayer",
            "name": "Dragon Slayer",
            "description": "Slew the dragon",
        }))
        .send()
        .await
        .unwrap();
    let achievement_id = define.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (signing_key_id, signing_key) = common::seed_signing_key(pool, identity_id).await;
    let connect_capabilities = ["achievements.issue"];
    http.post(format!("{base}/integrations/{}/connect", integrator.slug))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "capabilities": connect_capabilities,
            "signing_key_id": signing_key_id,
            "signature": common::sign_connect(&signing_key, &integrator.slug, &connect_capabilities),
        }))
        .send()
        .await
        .unwrap();

    let issuer_ref = format!("game:{}", integrator.slug);
    let issued_at = now_micros();
    let signing_bytes = issue_bytes(
        "achievement",
        &issuer_ref,
        &integrator.key_id.to_string(),
        identity_id,
        &achievement_id,
        issued_at,
    );
    let signature = integrator.signing_key.sign(&signing_bytes);

    let mut headers = auth_headers(http, base, &integrator).await;
    headers.insert(
        "x-avalon-identity-id",
        identity_id.to_string().parse().unwrap(),
    );
    let issue = http
        .post(format!(
            "{base}/integrations/{}/achievements/dragon_slayer/issue",
            integrator.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "key_id": integrator.key_id,
            "signature": BASE64.encode(signature.to_bytes()),
            "issued_at_micros": issued_at,
        }))
        .send()
        .await
        .unwrap();
    assert!(issue.status().is_success(), "{:?}", issue.status());
    let attestation_id: Uuid = issue.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    (integrator, issuer_ref, attestation_id)
}

async fn wait_for_outbox_drain(pool: &PgPool) {
    for _ in 0..100 {
        let status = avalon_server::outbox::status(pool)
            .await
            .expect("failed to read outbox status");
        if status.pending_count == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("outbox did not drain within the timeout");
}

#[tokio::test]
#[ignore]
async fn a_revocation_event_carries_the_issuer_signature_and_survives_a_rebuild() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (integrator, issuer_ref, attestation_id) = issue_one(&http, &base, &pool).await;

    let signing_bytes = revoke_bytes(
        "achievement",
        &issuer_ref,
        &integrator.key_id,
        attestation_id,
        "cheating",
        "Used a modified client",
    );
    let signature = BASE64.encode(integrator.signing_key.sign(&signing_bytes).to_bytes());
    let headers = auth_headers(&http, &base, &integrator).await;
    let revoke = http
        .post(format!("{base}/attestations/{attestation_id}/revoke"))
        .headers(headers)
        .json(&serde_json::json!({
            "key_id": integrator.key_id,
            "signature": signature,
            "reason_code": "cheating",
            "reason": "Used a modified client",
        }))
        .send()
        .await
        .unwrap();
    assert!(revoke.status().is_success(), "{:?}", revoke.status());
    wait_for_outbox_drain(&pool).await;

    let payload: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM ledger_entries WHERE kind = 'achievement.revoked' AND payload->>'attestation_id' = $1",
    )
    .bind(attestation_id.to_string())
    .fetch_one(&pool)
    .await
    .expect("the revocation is in the ledger");
    assert_eq!(payload["proof"]["key_id"], integrator.key_id);
    assert_eq!(payload["proof"]["algorithm"], "ed25519");
    assert_eq!(payload["proof"]["bytes"], signature);

    let network_id = PostgresSettlementProvider::read_genesis_network_id(&pool)
        .await
        .unwrap()
        .expect("genesis is set on a running node");
    let chain = PostgresSettlementProvider::new_core_shard(pool.clone(), network_id);
    let report = avalon_server::rebuild::rebuild_index_from_ledger(&chain, &pool, "core")
        .await
        .expect("rebuild failed");
    assert_eq!(report.entries_skipped_undecodable, 0);

    let revoked: Option<bool> =
        sqlx::query_scalar("SELECT revoked_at IS NOT NULL FROM indexer_attestations WHERE id = $1")
            .bind(attestation_id)
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert_eq!(
        revoked,
        Some(true),
        "the rebuilt index projects the signed revocation"
    );
}
