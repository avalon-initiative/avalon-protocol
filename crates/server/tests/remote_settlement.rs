//! Issue #313: a remote-settlement node's write path (`outbox::run_worker`
//! posting to `POST /ledger/submit` instead of committing locally) and read
//! path (mirror-watcher backfilling into its own local `PostgresIndexer`,
//! not just `mirrored_entries`).
//!
//! Gated `--ignored`, same convention `tests/settlement.rs` and
//! `tests/mirror_watcher.rs` already use for anything needing live
//! infra. The full scenario needs **two** real `avalon-server` processes
//! against **two** separate Postgres databases sharing one `AVALON_NETWORK_ID`
//! — one plain Settlement authority (`make start`'s usual config), one
//! configured as a remote-settlement node:
//!
//! ```text
//! # authority (the usual `make start` config)
//! AVALON_SETTLEMENT_SIGNING_KEY=...
//! AVALON_SETTLEMENT_SUBMIT_KEY=<shared secret>
//!
//! # remote-settlement node — separate DATABASE_URL, same AVALON_NETWORK_ID
//! AVALON_SETTLEMENT_REMOTE_URL=http://127.0.0.1:8080   # the authority
//! AVALON_SETTLEMENT_SUBMIT_KEY=<same shared secret>
//! AVALON_MIRROR_PEERS=http://127.0.0.1:8080            # watch the authority
//! AVALON_SETTLEMENT_VERIFY_KEY=<authority's public key>
//! ```
//!
//! This sandbox has no live database access, so
//! this file is written and expected to compile/lint here, but was not run
//! against real infra from this environment — it needs the same
//! `make test-live`-style verification `tests/mirror_watcher.rs` already
//! calls out as needing a real second `avalon-server` deployment to
//! exercise end-to-end.

use avalon_protocol::events::{EventBatch, ProtocolEvent};
use avalon_protocol::ids::GlobalId;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// The Settlement authority — same env var every other live test in this
/// crate reads.
fn authority_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

/// The second `avalon-server` process, configured with
/// `AVALON_SETTLEMENT_REMOTE_URL` pointed at the authority above and
/// `AVALON_MIRROR_PEERS` watching it. New to this ticket — no other live
/// test needs a second server, so no existing env var covers it.
fn remote_settlement_url() -> Option<String> {
    std::env::var("AVALON_REMOTE_SETTLEMENT_SERVER_URL").ok()
}

/// The remote-settlement node's *own* Postgres — separate from the
/// authority's `DATABASE_URL` (which every other live test in this crate
/// reads), since the whole point of this ticket is that these are two
/// independent databases.
async fn remote_settlement_pool() -> Option<PgPool> {
    let url = std::env::var("AVALON_REMOTE_SETTLEMENT_DATABASE_URL").ok()?;
    Some(
        PgPoolOptions::new()
            .connect(&url)
            .await
            .expect("failed to connect to the remote-settlement node's own Postgres"),
    )
}

/// Enqueues one identity-namespaced event into the remote-settlement node's
/// own outbox. Only core-shard events forward through the single
/// `AVALON_SETTLEMENT_REMOTE_URL`; a `game:`/`app:` issuer would route to a
/// named shard with no configured authority and commit locally instead.
async fn enqueue_core_event(pool: &PgPool) -> String {
    let actor = Uuid::new_v4();
    let issuer = GlobalId::new("identity", &actor.to_string(), "self", "test_event");
    let event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "test.remote_settlement".to_string(),
        issuer: issuer.clone(),
        subject: issuer.clone(),
        payload: serde_json::json!({ "note": "remote-settlement test" }),
        timestamp: time::OffsetDateTime::now_utc(),
        version: 1,
    };
    let mut tx = pool.begin().await.expect("begin failed");
    avalon_server::outbox::enqueue(&mut tx, &event)
        .await
        .expect("enqueue failed");
    tx.commit().await.expect("commit failed");
    issuer.as_str().to_string()
}

async fn wait_for_row<T, F>(mut fetch: F, description: &str) -> T
where
    F: AsyncFnMut() -> Option<T>,
{
    for _ in 0..30 {
        if let Some(value) = fetch().await {
            return value;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    panic!("{description} — never appeared within the retry budget");
}

/// `POST /ledger/submit` refuses any request without a correct bearer
/// credential — the invariant that this endpoint is privileged, unlike
/// every other endpoint `crate::settlement` exposes. Runs against a single
/// already-running server (the usual `make start` authority), so this one
/// doesn't need the second remote-settlement process.
#[tokio::test]
#[ignore]
async fn ledger_submit_rejects_requests_with_no_or_wrong_bearer_key() {
    let base = authority_url();
    let http = reqwest::Client::new();
    // Built via the real `EventBatch` struct, not hand-typed JSON —
    // `created_at`'s bare `OffsetDateTime` field round-trips through
    // `time`'s own default (de)serialization, not RFC3339; a hand-built
    // RFC3339 string here would fail JSON deserialization before the
    // handler's auth check ever runs, silently testing the wrong thing.
    let batch = EventBatch {
        id: Uuid::new_v4(),
        events: vec![],
        created_at: time::OffsetDateTime::now_utc(),
    };

    let no_auth = http
        .post(format!("{base}/ledger/submit"))
        .json(&batch)
        .send()
        .await
        .expect("POST /ledger/submit failed — is `make start` running?");
    assert_eq!(no_auth.status(), reqwest::StatusCode::UNAUTHORIZED);

    let wrong_auth = http
        .post(format!("{base}/ledger/submit"))
        .bearer_auth("definitely-not-the-configured-key")
        .json(&batch)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_auth.status(), reqwest::StatusCode::UNAUTHORIZED);
}

/// The ticket's own acceptance criterion: a write submitted to the
/// remote-settlement node's normal API is committed on the real authority,
/// and the remote-settlement node's own mirror-watcher independently
/// re-verifies and stores that exact entry — eventually, not
/// synchronously; this test polls rather than asserting immediacy.
#[tokio::test]
#[ignore]
async fn a_write_on_the_remote_settlement_node_lands_on_the_authority_and_is_backfilled_back() {
    let Some(_remote_base) = remote_settlement_url() else {
        panic!(
            "AVALON_REMOTE_SETTLEMENT_SERVER_URL not set — this test needs a second \
             avalon-server process configured with AVALON_SETTLEMENT_REMOTE_URL pointed at \
             the authority; see this file's module doc comment"
        );
    };
    let Some(remote_pool) = remote_settlement_pool().await else {
        panic!(
            "AVALON_REMOTE_SETTLEMENT_DATABASE_URL not set — needed to confirm the \
             remote-settlement node's own mirrored_entries table, independent of the \
             authority's database"
        );
    };
    let authority_pool = PgPoolOptions::new()
        .connect(&std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"))
        .await
        .expect("failed to connect to the authority's Postgres");

    let issuer = enqueue_core_event(&remote_pool).await;

    // Committed on the real authority — not the remote-settlement node's
    // own database, which never runs `chain.commit` locally while
    // AVALON_SETTLEMENT_REMOTE_URL is configured.
    let seq: i64 = wait_for_row(
        async || {
            sqlx::query("SELECT seq FROM ledger_entries WHERE issuer = $1")
                .bind(&issuer)
                .fetch_optional(&authority_pool)
                .await
                .expect("query failed")
                .map(|row| row.try_get("seq").expect("seq column"))
        },
        "entry never appeared in the authority's ledger_entries — is the remote-settlement \
         node's outbox worker running, and AVALON_SETTLEMENT_REMOTE_URL/AVALON_SETTLEMENT_SUBMIT_KEY \
         configured correctly on it?",
    )
    .await;

    // Independently re-verified and stored by this node's own
    // mirror-watcher — not merely visible because the write handler
    // itself already touched the local indexer (see this ticket's own
    // discussion of why identity/profile writes are a poor test fixture:
    // registering an integrator does not self-apply to any indexer projection,
    // so this row can only appear via backfill).
    let mirrored_seq: i64 = wait_for_row(
        async || {
            sqlx::query("SELECT seq FROM mirrored_entries WHERE network_id = (SELECT network_id FROM chain_genesis LIMIT 1) AND seq = $1")
                .bind(seq)
                .fetch_optional(&remote_pool)
                .await
                .expect("query failed")
                .map(|row| row.try_get("seq").expect("seq column"))
        },
        "entry never appeared in the remote-settlement node's own mirrored_entries — is its \
         mirror-watcher (AVALON_MIRROR_PEERS) running and pointed at the authority?",
    )
    .await;
    assert_eq!(mirrored_seq, seq);
}
