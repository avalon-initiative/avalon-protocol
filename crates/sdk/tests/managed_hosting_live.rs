//! Issue #564: exercises [`avalon_sdk::managed_hosting::ManagedHostingClient`]
//! against two real, independently-running `avalon-server` processes,
//! proving the prepare-race/finalize-once fan-out over actual HTTP rather
//! than mocked responses (`crates/sdk/src/managed_hosting.rs`'s own unit
//! tests already cover the logic against `wiremock`).
//!
//! Gated `--ignored` since it needs two live servers, both configured for
//! managed hosting with the *same* `AVALON_SETTLEMENT_SUBMIT_KEY` and
//! `AVALON_MANAGED_HOSTING_VERIFY_KEY`:
//!
//! ```text
//! # host A (e.g. AVALON_SERVER_ADDR=192.168.7.113:8080, or whatever's live)
//! # host B (e.g. AVALON_SERVER_ADDR=127.0.0.1:8081)
//! AVALON_MANAGED_HOSTING_LIVE_HOST_A=http://<host-a>:8080 \
//! AVALON_MANAGED_HOSTING_LIVE_HOST_B=http://<host-b>:8081 \
//! AVALON_SETTLEMENT_SUBMIT_KEY=test-submit-key \
//! AVALON_MANAGED_HOSTING_TEST_SIGNING_KEY=<hex seed matching both hosts'
//!   AVALON_MANAGED_HOSTING_VERIFY_KEY> \
//! cargo test -p avalon-sdk --test managed_hosting_live -- --ignored
//! ```

use avalon_protocol::events::{EventBatch, ProtocolEvent};
use avalon_protocol::ids::GlobalId;
use avalon_sdk::managed_hosting::{HostCandidate, ManagedHostingClient};
use ed25519_dalek::SigningKey;
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

fn env_or_panic(var: &str) -> String {
    std::env::var(var)
        .unwrap_or_else(|_| panic!("{var} not set — see this test file's own doc comment"))
}

fn test_signing_key() -> SigningKey {
    let hex_value = env_or_panic("AVALON_MANAGED_HOSTING_TEST_SIGNING_KEY");
    let bytes = hex::decode(&hex_value).expect("not valid hex");
    let seed: [u8; 32] = bytes.try_into().expect("must be exactly 32 bytes");
    SigningKey::from_bytes(&seed)
}

fn sample_batch(kind: &str) -> EventBatch {
    let actor = Uuid::new_v4();
    EventBatch {
        id: Uuid::new_v4(),
        events: vec![ProtocolEvent {
            id: Uuid::new_v4(),
            kind: kind.to_string(),
            issuer: GlobalId::new("game", "managed-hosting-live-test", "self", "test_event"),
            subject: GlobalId::new("identity", &actor.to_string(), "self", "test_event"),
            payload: json!({ "note": format!("managed hosting live SDK fan-out test — {kind}") }),
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }],
        created_at: OffsetDateTime::now_utc(),
    }
}

#[tokio::test]
#[ignore]
async fn submits_over_real_http_against_two_live_managed_hosts() {
    let host_a = env_or_panic("AVALON_MANAGED_HOSTING_LIVE_HOST_A");
    let host_b = env_or_panic("AVALON_MANAGED_HOSTING_LIVE_HOST_B");
    let submit_key = env_or_panic("AVALON_SETTLEMENT_SUBMIT_KEY");
    let signing_key = test_signing_key();

    let client = ManagedHostingClient::new(vec![
        HostCandidate {
            url: host_a,
            submit_key: submit_key.clone(),
        },
        HostCandidate {
            url: host_b,
            submit_key,
        },
    ]);

    let batch = sample_batch("test.managed_hosting_live_fanout");
    let commitment = client
        .submit(&batch, &signing_key, "live-fanout-test-key")
        .await
        .expect("submit against two real managed hosts should succeed");

    assert_eq!(commitment.batch_id, batch.id);
}
