//! Issue #596's own live acceptance test: push-based mirror sync actually
//! reduces observed mirror-sync latency versus the old poll-only path, and
//! a mirror with no push available (or that never registered interest)
//! still converges via the poll fallback alone. Exercised against two
//! real, separately-running `avalon-server` processes sharing one
//! Postgres — the same "second URL via env var" pattern
//! `crates/server/tests/realtime_relay.rs`/`remote_settlement.rs` already
//! established, rather than spawning child processes from inside the test
//! itself. Gated `--ignored`/live like every other multi-node test in this
//! crate.
//!
//! Setup, node A (the authority this test writes through) and node B (the
//! mirror this test observes convergence on), both against the same
//! `DATABASE_URL`/`AVALON_NETWORK_ID`:
//!
//! ```text
//! # node A (authority)
//! AVALON_SERVER_ADDR=127.0.0.1:8080
//! AVALON_NODE_URL=http://127.0.0.1:8080
//! AVALON_DHT_ENABLED=true      # default since ADR #593; spelled out for clarity
//!
//! # node B (mirror) — a distinct port, pointed at node A
//! AVALON_SERVER_ADDR=127.0.0.1:8090
//! AVALON_NODE_URL=http://127.0.0.1:8090
//! AVALON_MIRROR_PEERS=http://127.0.0.1:8080
//! AVALON_BOOTSTRAP_PEERS=http://127.0.0.1:8080
//! AVALON_ANNOUNCE_INTERVAL_SECS=5    # so the two nodes' peer tables (and DHT dial) settle quickly
//! AVALON_DHT_ENABLED=true
//!
//! AVALON_SERVER_URL=http://127.0.0.1:8080                     # this test's default target for node A
//! AVALON_MIRROR_PUSH_PEER_SERVER_URL=http://127.0.0.1:8090    # node B, required
//! ```
//!
//! `mirroring_still_converges_via_poll_alone_when_push_is_unavailable`
//! additionally needs a *third* node (node C), configured exactly like
//! node B above except `AVALON_DHT_ENABLED=false` and a short
//! `AVALON_MIRROR_POLL_INTERVAL_SECS` (e.g. `5`) — a mirror that never
//! registers interest at all, proving tier 1 (permissionless polling)
//! still fully converges on its own:
//!
//! ```text
//! AVALON_MIRROR_PUSH_NO_DHT_PEER_SERVER_URL=http://127.0.0.1:8091   # node C, required for that one test
//! ```

use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::ids::GlobalId;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

fn authority_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn require_mirror_url() -> String {
    std::env::var("AVALON_MIRROR_PUSH_PEER_SERVER_URL").unwrap_or_else(|_| {
        panic!(
            "AVALON_MIRROR_PUSH_PEER_SERVER_URL not set — this test needs a second, \
             independently-running avalon-server process mirroring the authority named by \
             AVALON_SERVER_URL (AVALON_MIRROR_PEERS/AVALON_BOOTSTRAP_PEERS pointed at it, DHT \
             enabled) — see this file's module doc comment"
        )
    })
}

fn require_no_dht_mirror_url() -> String {
    std::env::var("AVALON_MIRROR_PUSH_NO_DHT_PEER_SERVER_URL").unwrap_or_else(|_| {
        panic!(
            "AVALON_MIRROR_PUSH_NO_DHT_PEER_SERVER_URL not set — this test needs a third \
             avalon-server process mirroring the authority with AVALON_DHT_ENABLED=false and a \
             short AVALON_MIRROR_POLL_INTERVAL_SECS, to prove poll-only convergence still works \
             with no push available at all — see this file's module doc comment"
        )
    })
}

async fn test_pool() -> PgPool {
    avalon_devenv::load();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

/// This node's current `tree_size`, or `None` if it hasn't produced/mirrored
/// anything yet (a fresh mirror's expected pre-backfill 404, per #519 —
/// never treated as an error here).
async fn current_tree_size(http: &reqwest::Client, base_url: &str) -> Option<i64> {
    let response = http
        .get(format!("{base_url}/ledger/sth/latest"))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: serde_json::Value = response.json().await.ok()?;
    body["tree_size"].as_i64()
}

/// A minimal, well-formed `identity.created` event, inserted directly into
/// `protocol_outbox` — the same entry point every real write in this
/// codebase goes through (`crate::outbox::enqueue`), just reached here via
/// raw SQL rather than an HTTP endpoint (which would additionally need a
/// full WebAuthn ceremony) since this test's whole point is what happens
/// *after* a commit, not how one gets enqueued. The authority's own
/// already-running outbox worker (`AVALON_OUTBOX_POLL_INTERVAL_SECS`,
/// default 3s) picks this up on its own, commits it, and — per #596 —
/// pushes the resulting STH to any peer registered as interested.
async fn enqueue_real_event(pool: &PgPool) {
    let actor = Uuid::new_v4();
    let subject = GlobalId::new("identity", &actor.to_string(), "self", "created");
    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "identity.created".to_string(),
        issuer: subject.clone(),
        subject,
        payload: serde_json::json!({ "display_name": format!("mirror-push-test-{actor}") }),
        timestamp: OffsetDateTime::now_utc(),
        version: 1,
    };
    // Serialized exactly the way `crate::outbox::enqueue` does — a hand-built
    // JSON literal here (rather than a real `ProtocolEvent`) previously used
    // an RFC3339 string timestamp, which doesn't match `OffsetDateTime`'s
    // actual (non-human-readable) derived wire format, so the outbox worker
    // silently dropped every row this test enqueued as unparseable.
    let event_json = serde_json::to_value(&event).expect("ProtocolEvent should serialize");
    sqlx::query("INSERT INTO protocol_outbox (event) VALUES ($1)")
        .bind(event_json)
        .execute(pool)
        .await
        .expect("failed to enqueue a real event directly into protocol_outbox");
}

/// Polls `base_url` until its `tree_size` is strictly greater than
/// `baseline`, returning how long that took — `None` if it never happens
/// within `timeout`.
async fn time_to_converge(
    http: &reqwest::Client,
    base_url: &str,
    baseline: Option<i64>,
    timeout: std::time::Duration,
) -> Option<std::time::Duration> {
    let start = std::time::Instant::now();
    let floor = baseline.unwrap_or(-1);
    while start.elapsed() < timeout {
        if let Some(size) = current_tree_size(http, base_url).await {
            if size > floor {
                return Some(start.elapsed());
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    None
}

/// #596's headline acceptance criterion: a registered mirror observes a
/// freshly-committed STH far faster than the (now 120s-default) poll
/// interval alone would ever allow — proof the push path, not the poll
/// tick, is what delivered it. This does not disable node B's poll
/// fallback (nothing in this setup could, short of restarting it); it
/// just proves the observed latency is inconsistent with poll being the
/// delivery mechanism.
#[tokio::test]
#[ignore]
async fn a_push_notification_measurably_reduces_observed_mirror_sync_latency() {
    let http = reqwest::Client::new();
    // Only used to document/require the authority's own URL is configured
    // (per this file's module doc) — the actual write below goes straight
    // into `protocol_outbox`, not through node A's HTTP surface.
    let _node_a = authority_url();
    let node_b = require_mirror_url();
    let pool = test_pool().await;

    let baseline = current_tree_size(&http, &node_b).await;

    enqueue_real_event(&pool).await;

    let elapsed = time_to_converge(&http, &node_b, baseline, std::time::Duration::from_secs(20))
        .await
        .expect(
            "node B never observed a newer tree_size within 20s of a real commit on node A — \
             push notification did not arrive (or backfill failed) within a bound far shorter \
             than the poll interval",
        );

    tracing::info!(?elapsed, "mirror-push: node B converged");
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "convergence took {elapsed:?}, which is not meaningfully faster than waiting out a poll \
         tick — push does not appear to be doing anything here",
    );
}

/// #596's other invariant: a mirror with no push available at all (DHT
/// disabled — never registered any interest to be pushed to) still fully
/// converges, bounded only by its own configured poll interval — tier 1
/// (permissionless polling) is completely unaffected by this ticket.
#[tokio::test]
#[ignore]
async fn mirroring_still_converges_via_poll_alone_when_push_is_unavailable() {
    let http = reqwest::Client::new();
    let node_c = require_no_dht_mirror_url();
    let pool = test_pool().await;

    let baseline = current_tree_size(&http, &node_c).await;

    enqueue_real_event(&pool).await;

    // Generous relative to node C's short test-configured
    // AVALON_MIRROR_POLL_INTERVAL_SECS (documented as 5s in this file's
    // module doc) — proves eventual convergence, not speed, for this tier.
    let elapsed = time_to_converge(&http, &node_c, baseline, std::time::Duration::from_secs(30))
        .await
        .expect(
            "node C (DHT disabled, poll-only) never converged within 30s — the poll fallback \
             itself is broken, independent of anything push-related",
        );
    tracing::info!(?elapsed, "mirror-push: poll-only node C converged");
}
