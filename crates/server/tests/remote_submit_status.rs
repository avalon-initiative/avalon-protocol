//! Issue #526: `GET /ledger/remote-submit-status`
//! (`crate::settlement::remote_submit_status`) against a real, running
//! **forwarding** `avalon-server` — one started with
//! `AVALON_SETTLEMENT_REMOTE_URL` pointed at an authority this test can
//! control the reachability of. Gated `--ignored` since it needs live
//! infra distinct from the usual `make start` dev server:
//!
//! ```text
//! # A forwarding node, pointed at an address nothing listens on so every
//! # remote submit fails with a connection error:
//! AVALON_SETTLEMENT_REMOTE_URL=http://127.0.0.1:9 \
//! AVALON_SERVER_ADDR=127.0.0.1:8090 \
//! cargo run -p avalon-server &
//!
//! AVALON_SERVER_URL=http://127.0.0.1:8090 \
//! cargo test -p avalon-server --test remote_submit_status -- --ignored --test-threads=1
//! ```
//!
//! `AVALON_SETTLEMENT_REMOTE_URL` must be readable back from this test's
//! own environment too (it asserts the surfaced `authority` matches it
//! exactly — see this file's own `configured_authority_url` helper).

use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::ids::GlobalId;
use serde_json::json;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8090".to_string())
}

fn configured_authority_url() -> String {
    std::env::var("AVALON_SETTLEMENT_REMOTE_URL").unwrap_or_else(|_| {
        panic!(
            "AVALON_SETTLEMENT_REMOTE_URL not set on this test process — must match the value \
             the running (forwarding) avalon-server was started with, see this file's own doc \
             comment"
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

/// Enqueues one throwaway event directly into `protocol_outbox` — the
/// forwarding node's own next drain tick will try (and, given this test's
/// setup, fail) to forward it to its configured (unreachable) authority.
async fn enqueue_one_event(pool: &PgPool, kind: &str) {
    let actor = Uuid::new_v4();
    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: kind.to_string(),
        issuer: GlobalId::new("identity", &actor.to_string(), "self", "test_event"),
        subject: GlobalId::new("identity", &actor.to_string(), "self", "test_event"),
        payload: json!({ "note": format!("remote-submit-status test — {kind}") }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    };
    let mut tx = pool.begin().await.expect("begin failed");
    avalon_server::outbox::enqueue(&mut tx, &event)
        .await
        .expect("enqueue failed");
    tx.commit().await.expect("commit failed");
}

#[tokio::test]
#[ignore]
async fn an_unreachable_authority_surfaces_as_503_naming_its_own_configured_url() {
    let http = reqwest::Client::new();
    let base = server_url();
    let authority = configured_authority_url();
    let pool = test_pool().await;

    enqueue_one_event(&pool, "test.remote_submit_status_unreachable").await;

    // Poll rather than a fixed sleep — the forwarding node's own drain
    // cadence (`AVALON_OUTBOX_POLL_INTERVAL_SECS`, default 3s) isn't this
    // test's business to assume exactly.
    let mut body: Option<serde_json::Value> = None;
    for _ in 0..30 {
        let response = http
            .get(format!("{base}/ledger/remote-submit-status"))
            .send()
            .await
            .expect("request failed");
        if response.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            body = Some(response.json().await.expect("body should be JSON"));
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    let body = body.expect(
        "expected GET /ledger/remote-submit-status to eventually report 503 once the \
         forwarding node's drain tick hits its unreachable authority",
    );
    let failing_shards = body["failing_shards"]
        .as_array()
        .expect("failing_shards should be an array");
    assert!(
        !failing_shards.is_empty(),
        "failing_shards must be non-empty on a 503 response"
    );
    let core = failing_shards
        .iter()
        .find(|s| s["shard_id"] == "core")
        .expect("the core shard should be the one failing here");
    assert_eq!(
        core["authority"], authority,
        "the surfaced authority must be exactly this node's own configured target, \
         never an inferred or alternate one"
    );
    assert!(
        !core["reason"].as_str().unwrap_or_default().is_empty(),
        "a rejection/failure reason must be present"
    );
}
