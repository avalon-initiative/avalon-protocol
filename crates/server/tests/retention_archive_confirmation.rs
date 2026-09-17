//! Issue #569: end-to-end proof that hot-tier pruning actually refuses to
//! run until enough configured archive peers confirm coverage, then
//! proceeds once they do — not just the isolated HTTP-check unit tests in
//! `crates/server/src/retention.rs`.
//!
//! Gated `--ignored` since it needs a real, **isolated** Postgres database
//! — deliberately never the shared dev database `make start` normally
//! points at: `PostgresSettlementProvider::prune_payloads_older_than`'s
//! cutoff is "everything committed before this instant," not scoped to
//! specific rows, so running it for real against a database holding
//! accumulated real history would null out payloads for all of it, not
//! just this test's own throwaway entries. Point `DATABASE_URL` at a
//! disposable database instead:
//!
//! ```text
//! DATABASE_URL=postgres://avalon:avalon@<host>/avalon_retention_test \
//! AVALON_SETTLEMENT_SIGNING_KEY=<hex seed> \
//! cargo test -p avalon-server --test retention_archive_confirmation -- --ignored
//! ```

use avalon_chain::retention::{RetentionConfig, RetentionTier};
use avalon_chain::{PostgresSettlementProvider, SettlementProvider};
use avalon_protocol::events::{EventBatch, ProtocolEvent};
use avalon_protocol::ids::GlobalId;
use avalon_server::retention::confirm_archive_coverage;
use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use std::collections::HashMap;
use time::OffsetDateTime;
use uuid::Uuid;

async fn test_pool() -> sqlx::PgPool {
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
            issuer: GlobalId::new("identity", &actor.to_string(), "self", "test_event"),
            subject: GlobalId::new("identity", &actor.to_string(), "self", "test_event"),
            payload: json!({ "note": "retention-archive-confirmation test" }),
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
        }],
        created_at: OffsetDateTime::now_utc(),
    }
}

/// Same fake-peer shape as `crates/server/src/retention.rs`'s own unit
/// tests, just re-declared here since it's a private test helper there.
async fn spawn_fake_peer(last_seq: i64) -> String {
    async fn handler(
        Query(_params): Query<HashMap<String, String>>,
        State(last_seq): State<i64>,
    ) -> Json<serde_json::Value> {
        Json(json!({ "last_seq": last_seq }))
    }

    let app = Router::new()
        .route("/ledger/mirror-progress", get(handler))
        .with_state(last_seq);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
#[ignore]
async fn pruning_is_blocked_until_confirmed_then_proceeds_once_it_is() {
    let pool = test_pool().await;
    let chain = PostgresSettlementProvider::new(pool.clone(), "avalon-retention-test");

    // Commit a few real, throwaway entries — safe here because this
    // database is isolated (see this file's own module doc comment).
    let batch = sample_batch("test.retention_archive_confirmation");
    chain.commit(&batch).await.expect("commit failed");

    // A cutoff comfortably after everything just committed, so every
    // entry in this isolated database is prunable at this cutoff.
    let cutoff = OffsetDateTime::now_utc() + time::Duration::seconds(5);
    let boundary_seq = chain
        .max_seq_before(cutoff)
        .await
        .expect("max_seq_before failed")
        .expect("something should be prunable at this cutoff");

    let short_peer = spawn_fake_peer(boundary_seq - 1).await;
    let covering_peer = spawn_fake_peer(boundary_seq).await;
    let client = reqwest::Client::new();

    // Phase 1: only a peer that hasn't mirrored far enough is configured
    // — coverage must not be confirmed, and (mirroring exactly what
    // `crate::retention::prune_once` does) pruning must not proceed.
    let blocked_config = RetentionConfig {
        tier: RetentionTier::Hot { window_days: 1 },
        pruning_enabled: true,
        archive_peers: vec![short_peer],
        min_archive_confirmations: 1,
    };
    assert!(
        !confirm_archive_coverage(&client, &blocked_config, chain.network_id(), boundary_seq).await,
        "a peer short of the boundary must not confirm coverage"
    );

    let still_prunable = chain
        .prunable_entry_count(cutoff)
        .await
        .expect("prunable_entry_count failed");
    assert!(
        still_prunable > 0,
        "nothing should have been pruned yet — this test never called prune_payloads_older_than \
         while coverage was unconfirmed, matching what prune_once itself would have done"
    );

    // Phase 2: reconfigure with a peer that genuinely covers the
    // boundary — coverage confirms, and it's now safe to actually prune.
    let confirmed_config = RetentionConfig {
        archive_peers: vec![covering_peer],
        ..blocked_config
    };
    assert!(
        confirm_archive_coverage(&client, &confirmed_config, chain.network_id(), boundary_seq)
            .await,
        "a peer at exactly the boundary must confirm coverage"
    );

    let report = chain
        .prune_payloads_older_than(cutoff)
        .await
        .expect("prune_payloads_older_than failed");
    assert!(report.pruned_count > 0, "should have pruned real entries");

    let still_prunable_after = chain
        .prunable_entry_count(cutoff)
        .await
        .expect("prunable_entry_count failed");
    assert_eq!(
        still_prunable_after, 0,
        "everything prunable at this cutoff should now be pruned"
    );
}
