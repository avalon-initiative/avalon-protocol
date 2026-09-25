//! Issue #531: `POST /ledger/prepare-batch`/`POST /ledger/finalize-batch`
//! (`crate::settlement::prepare_batch`/`finalize_batch`) against a real,
//! running `avalon-server`. Gated `--ignored` since it needs live infra —
//! see `make test-live` / `make start`, with both
//! `AVALON_SETTLEMENT_SUBMIT_KEY` and `AVALON_MANAGED_HOSTING_VERIFY_KEY`
//! set (the latter to the public half of a throwaway keypair this test
//! generates its own private half for, standing in for a real
//! integrator's settlement key):
//!
//! ```text
//! AVALON_SETTLEMENT_SUBMIT_KEY=test-submit-key
//! AVALON_MANAGED_HOSTING_VERIFY_KEY=<hex-encoded public key — print via
//!   the small snippet in this file's own module doc, or any Ed25519
//!   keygen tool>
//! make start
//! cargo test -p avalon-server --test managed_hosting -- --ignored
//! ```

use avalon_protocol::events::{EventBatch, ProtocolEvent};
use avalon_protocol::ids::GlobalId;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn require_submit_key() -> String {
    std::env::var("AVALON_SETTLEMENT_SUBMIT_KEY").unwrap_or_else(|_| {
        panic!(
            "AVALON_SETTLEMENT_SUBMIT_KEY not set on this test process — must match the value \
             the running avalon-server was started with"
        )
    })
}

async fn test_pool() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    sqlx::postgres::PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

fn sample_batch(kind: &str) -> EventBatch {
    let actor = Uuid::new_v4();
    EventBatch {
        id: Uuid::new_v4(),
        events: vec![ProtocolEvent {
            id: Uuid::new_v4(),
            kind: kind.to_string(),
            issuer: GlobalId::new("game", "managed-hosting-http-test", "self", "test_event"),
            subject: GlobalId::new("identity", &actor.to_string(), "self", "test_event"),
            payload: json!({ "note": format!("managed hosting HTTP test — {kind}") }),
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
            identity_chain: None,
        }],
        created_at: OffsetDateTime::now_utc(),
    }
}

/// This test's own stand-in for the integrator's settlement key — its
/// public half must match whatever `AVALON_MANAGED_HOSTING_VERIFY_KEY`
/// the running server was started with, or every `finalize` call below
/// fails, correctly, with a clear message pointing at the setup gap
/// rather than a confusing assertion failure.
fn require_test_signing_key_matching_the_configured_verify_key() -> SigningKey {
    let hex_value = std::env::var("AVALON_MANAGED_HOSTING_TEST_SIGNING_KEY").unwrap_or_else(|_| {
        panic!(
            "AVALON_MANAGED_HOSTING_TEST_SIGNING_KEY not set — this test needs the *private* \
             half of whatever public key AVALON_MANAGED_HOSTING_VERIFY_KEY names on the \
             running server, hex-encoded (32-byte Ed25519 seed)"
        )
    });
    let bytes = hex::decode(&hex_value).expect("not valid hex");
    let seed: [u8; 32] = bytes.try_into().expect("must be exactly 32 bytes");
    SigningKey::from_bytes(&seed)
}

#[tokio::test]
#[ignore]
async fn prepare_then_finalize_over_http_commits_the_batch() {
    let http = reqwest::Client::new();
    let base = server_url();
    let submit_key = require_submit_key();
    let integrator_key = require_test_signing_key_matching_the_configured_verify_key();
    let pool = test_pool().await;

    let batch = sample_batch("test.managed_hosting_http_ok");

    let prepare_response = http
        .post(format!("{base}/ledger/prepare-batch"))
        .bearer_auth(&submit_key)
        .json(&batch)
        .send()
        .await
        .expect("prepare-batch request failed — is `make start` running?");
    assert!(
        prepare_response.status().is_success(),
        "{:?}",
        prepare_response.status()
    );
    let preview: serde_json::Value = prepare_response.json().await.unwrap();
    let tree_size = preview["tree_size"].as_i64().unwrap();
    let root_hash = preview["root_hash"].as_str().unwrap().to_string();
    let network_id = preview["network_id"].as_str().unwrap().to_string();
    let created_at_str = preview["created_at"].as_str().unwrap().to_string();
    let created_at = time::OffsetDateTime::parse(
        &created_at_str,
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();

    // Sign locally, exactly as a real integrator using a managed host
    // would — this key never appears in any request to the server.
    let message =
        avalon_protocol::sth::signing_message(tree_size, &root_hash, &network_id, created_at);
    let signature = integrator_key.sign(&message);

    let finalize_response = http
        .post(format!("{base}/ledger/finalize-batch"))
        .bearer_auth(&submit_key)
        .json(&json!({
            "batch": batch,
            "created_at": created_at_str,
            "signing_key_id": "http-test-key-1",
            "signature": hex::encode(signature.to_bytes()),
        }))
        .send()
        .await
        .expect("finalize-batch request failed");
    assert!(
        finalize_response.status().is_success(),
        "{:?}",
        finalize_response.status()
    );

    let row = sqlx::query("SELECT event_id FROM ledger_entries WHERE event_id = $1")
        .bind(batch.events[0].id)
        .fetch_optional(&pool)
        .await
        .expect("query failed");
    assert!(
        row.is_some(),
        "finalize-batch should have actually committed the event"
    );
}

#[tokio::test]
#[ignore]
async fn finalize_over_http_with_the_wrong_key_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let submit_key = require_submit_key();
    let pool = test_pool().await;

    let batch = sample_batch("test.managed_hosting_http_wrong_key");

    let prepare_response = http
        .post(format!("{base}/ledger/prepare-batch"))
        .bearer_auth(&submit_key)
        .json(&batch)
        .send()
        .await
        .expect("prepare-batch request failed");
    assert!(prepare_response.status().is_success());
    let preview: serde_json::Value = prepare_response.json().await.unwrap();
    let tree_size = preview["tree_size"].as_i64().unwrap();
    let root_hash = preview["root_hash"].as_str().unwrap().to_string();
    let network_id = preview["network_id"].as_str().unwrap().to_string();
    let created_at_str = preview["created_at"].as_str().unwrap().to_string();
    let created_at = time::OffsetDateTime::parse(
        &created_at_str,
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();

    // A random key, never registered as AVALON_MANAGED_HOSTING_VERIFY_KEY.
    let wrong_key = SigningKey::generate(&mut rand::rng());
    let message =
        avalon_protocol::sth::signing_message(tree_size, &root_hash, &network_id, created_at);
    let signature = wrong_key.sign(&message);

    let finalize_response = http
        .post(format!("{base}/ledger/finalize-batch"))
        .bearer_auth(&submit_key)
        .json(&json!({
            "batch": batch,
            "created_at": created_at_str,
            "signing_key_id": "wrong-key",
            "signature": hex::encode(signature.to_bytes()),
        }))
        .send()
        .await
        .expect("finalize-batch request failed");
    assert!(
        !finalize_response.status().is_success(),
        "finalize-batch must reject a signature from an unregistered key"
    );

    let row = sqlx::query("SELECT event_id FROM ledger_entries WHERE event_id = $1")
        .bind(batch.events[0].id)
        .fetch_optional(&pool)
        .await
        .expect("query failed");
    assert!(
        row.is_none(),
        "a rejected finalize-batch must never commit the event"
    );
}

#[tokio::test]
#[ignore]
async fn prepare_batch_without_the_submit_key_is_unauthorized() {
    let http = reqwest::Client::new();
    let base = server_url();
    let batch = sample_batch("test.managed_hosting_http_unauth");

    let response = http
        .post(format!("{base}/ledger/prepare-batch"))
        .json(&batch)
        .send()
        .await
        .expect("request failed");
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}
