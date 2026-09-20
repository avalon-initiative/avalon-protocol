//! Issue #661: live proof that `crate::internal_role::RemoteIndexer` — a
//! real `avalon_indexer::Indexer` backed by HTTP calls to
//! `POST /internal/indexer/apply`/`/internal/indexer/rebuild` — behaves
//! identically to the in-process `PostgresIndexer` it's standing in for,
//! against a real, separately-running `avalon-server` process, and fails
//! cleanly (not a hang, not a misleading generic error) when that process
//! goes away mid-use.
//!
//! Gated `--ignored`, same convention every other live test in this crate
//! uses. Unlike `tests/cross_node_login_cross_shard.rs` (which needs two
//! processes with genuinely separate database state to prove a real
//! cross-shard fallback), this test's whole point is the opposite: one
//! "Indexer role" process and this test itself playing the calling
//! ("Gateway") side — proving `RemoteIndexer` gets the exact same answer
//! a direct `PostgresIndexer` call against the same rows would, so they
//! deliberately share one schema.
//!
//! This test creates and migrates its own schema (`setup_schema` below,
//! via a plain, unscoped `DATABASE_URL` connection for the `CREATE SCHEMA`
//! and `avalon_server::migrate::migrate_up` for the rest — no external
//! `make migrate` step needed) but does **not** spawn or kill the "Indexer
//! role" `avalon-server` process itself — start it exactly as documented
//! below, matching this crate's established live-test convention of a
//! manually-run second process rather than a test-spawned child.
//!
//! **Ordering matters once**: that second process is only ever given a
//! `search_path`-scoped `DATABASE_URL`, and its own boot-time migration
//! needs the schema to already exist (Postgres refuses `CREATE ... IN
//! SCHEMA` against a `search_path` naming a schema that doesn't exist yet
//! — "no schema has been selected to create in"). Run any one test in
//! this file once first (its `setup_schema` call creates the schema via
//! an unscoped connection) — or create it directly
//! (`psql "$DATABASE_URL" -c 'CREATE SCHEMA IF NOT EXISTS test_internal_role_661'`)
//! — before starting the process below for the first time against a fresh
//! database.
//!
//! ```text
//! DATABASE_URL='...?options=-c%20search_path%3Dtest_internal_role_661' \
//! AVALON_SERVER_ADDR=127.0.0.1:8097 AVALON_INTERNAL_ROLE_KEY=test-internal-role-key-661 \
//! AVALON_BOOTSTRAP_PEERS= AVALON_SETTLEMENT_REMOTE_URLS= AVALON_KNOWN_SHARDS= \
//! AVALON_MIRROR_PEERS= AVALON_SETTLEMENT_SUBMIT_KEY= AVALON_DHT_ENABLED= \
//! cargo run -p avalon-server
//!
//! AVALON_INTERNAL_ROLE_SERVER_URL=http://127.0.0.1:8097 \
//! AVALON_INTERNAL_ROLE_KEY=test-internal-role-key-661 \
//! cargo test -p avalon-server --test internal_role_protocol -- --ignored --test-threads=1
//!
//! # cleanup
//! psql "$DATABASE_URL" -c 'DROP SCHEMA IF EXISTS test_internal_role_661 CASCADE'
//! ```
//!
//! Live-verified in this environment against real Postgres and a real,
//! separately-running `avalon-server` process — all three tests below
//! pass, including the process-kill scenario.
//!
//! The last two tests below (`apply_via_remote_matches_direct_postgres_indexer`,
//! `rebuild_via_remote_matches_direct_postgres_indexer`) need that process
//! running. `remote_indexer_reports_a_clear_failure_when_the_role_is_killed`
//! is the mid-request-kill proof: it asserts success first (the process is
//! genuinely up), then this test itself kills it (via the real OS `kill`,
//! given the PID printed to `AVALON_INTERNAL_ROLE_SERVER_PID` if set — see
//! that test's own doc comment for the exact manual step if run by hand
//! instead) and asserts the very next request fails fast and clearly.

use avalon_indexer::postgres::PostgresIndexer;
use avalon_indexer::{IndexError, Indexer};
use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::ids::GlobalId;
use avalon_server::internal_role::RemoteIndexer;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

const SCHEMA: &str = "test_internal_role_661";
const DEFAULT_ROLE_KEY: &str = "test-internal-role-key-661";

fn role_key() -> String {
    std::env::var("AVALON_INTERNAL_ROLE_KEY").unwrap_or_else(|_| DEFAULT_ROLE_KEY.to_string())
}

fn remote_indexer_url() -> String {
    std::env::var("AVALON_INTERNAL_ROLE_SERVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8097".to_string())
}

fn base_database_url() -> String {
    std::env::var("DATABASE_URL").expect("DATABASE_URL must be set")
}

/// This test's own schema-scoped `DATABASE_URL`, matching the connection
/// string the manually-started "Indexer role" process above must also be
/// given — same construction `docs/architecture/nodes.md`'s
/// `feedback_live_verify_two_local_processes`-style live tests already use.
fn schema_database_url() -> String {
    let base = base_database_url();
    let sep = if base.contains('?') { "&" } else { "?" };
    format!("{base}{sep}options=-c%20search_path%3D{SCHEMA}")
}

fn migrations_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("db/migrations")
}

/// Creates (if needed) and migrates this test's own isolated schema, then
/// returns a pool connected to it — self-contained so this test doesn't
/// need a separate `make migrate` invocation against a hand-picked schema
/// first. Safe to call repeatedly: `CREATE SCHEMA IF NOT EXISTS` plus
/// `avalon_server::migrate::migrate_up`'s own `sqlx` migration tracking
/// make this idempotent.
async fn setup_schema() -> PgPool {
    let admin_pool = PgPoolOptions::new()
        .connect(&base_database_url())
        .await
        .expect("failed to connect to Postgres — is it reachable?");
    sqlx::query("CREATE SCHEMA IF NOT EXISTS test_internal_role_661")
        .execute(&admin_pool)
        .await
        .expect("failed to create the test schema");
    admin_pool.close().await;

    let pool = PgPoolOptions::new()
        .connect(&schema_database_url())
        .await
        .expect("failed to connect to the test schema");
    avalon_server::migrate::migrate_up(&pool, &migrations_dir())
        .await
        .expect("failed to migrate the test schema");
    pool
}

/// `profiles.identity_id` has a real FK into `identities` — same seeding
/// step every other live test in this crate uses
/// (`crates/indexer/tests/postgres_indexer.rs`'s own `seed_identity`)
/// before an `identity.created` event can be applied.
async fn seed_identity(pool: &PgPool, identity_id: Uuid) {
    sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(identity_id)
        .execute(pool)
        .await
        .expect("failed to seed identity");
}

fn identity_created_event(identity_id: Uuid, display_name: &str) -> ProtocolEvent {
    ProtocolEvent {
        id: Uuid::new_v4(),
        kind: "identity.created".to_string(),
        issuer: GlobalId::new("identity", &identity_id.to_string(), "self", "created"),
        subject: GlobalId::new("identity", &identity_id.to_string(), "self", "created"),
        payload: serde_json::json!({
            "identity_id": identity_id,
            "display_name": display_name,
        }),
        timestamp: time::OffsetDateTime::now_utc(),
        version: 1,
    }
}

async fn profile_display_name(pool: &PgPool, identity_id: Uuid) -> Option<String> {
    sqlx::query("SELECT display_name FROM profiles WHERE identity_id = $1")
        .bind(identity_id)
        .fetch_optional(pool)
        .await
        .expect("query failed")
        .map(|row| row.get::<String, _>("display_name"))
}

/// `RemoteIndexer::apply` against the real, separately-running "Indexer
/// role" process produces the exact same projected row a direct
/// `PostgresIndexer::apply` call (against the same schema) would — proving
/// the network path and the in-process path are behaviorally
/// indistinguishable, the whole point of #661's remote-role pattern.
#[tokio::test]
#[ignore]
async fn apply_via_remote_matches_direct_postgres_indexer() {
    let pool = setup_schema().await;
    let local_indexer = PostgresIndexer::new(pool.clone());
    let remote_indexer = RemoteIndexer::new(remote_indexer_url(), Some(role_key()));

    let local_identity = Uuid::new_v4();
    let remote_identity = Uuid::new_v4();
    seed_identity(&pool, local_identity).await;
    seed_identity(&pool, remote_identity).await;

    local_indexer
        .apply(&identity_created_event(
            local_identity,
            "direct-postgres-indexer",
        ))
        .await
        .expect("direct PostgresIndexer apply failed");
    remote_indexer
        .apply(&identity_created_event(remote_identity, "remote-http-indexer"))
        .await
        .expect("RemoteIndexer apply failed — is the Indexer-role process running? See this file's own module doc comment.");

    let local_name = profile_display_name(&pool, local_identity)
        .await
        .expect("direct-applied identity should have a profile row");
    let remote_name = profile_display_name(&pool, remote_identity)
        .await
        .expect("remotely-applied identity should have a profile row");

    assert_eq!(local_name, "direct-postgres-indexer");
    assert_eq!(remote_name, "remote-http-indexer");
}

/// `RemoteIndexer::rebuild` — a second, distinct trait operation (not just
/// `apply` looped) — proven the same way: identical outcome to a direct
/// `PostgresIndexer::rebuild` call over an equivalent event set.
#[tokio::test]
#[ignore]
async fn rebuild_via_remote_matches_direct_postgres_indexer() {
    let pool = setup_schema().await;
    let local_indexer = PostgresIndexer::new(pool.clone());
    let remote_indexer = RemoteIndexer::new(remote_indexer_url(), Some(role_key()));

    let local_identity = Uuid::new_v4();
    let remote_identity = Uuid::new_v4();
    seed_identity(&pool, local_identity).await;
    seed_identity(&pool, remote_identity).await;
    let local_events = vec![identity_created_event(local_identity, "direct-rebuild")];
    let remote_events = vec![identity_created_event(remote_identity, "remote-rebuild")];

    local_indexer
        .rebuild(&local_events)
        .await
        .expect("direct PostgresIndexer rebuild failed");
    remote_indexer
        .rebuild(&remote_events)
        .await
        .expect("RemoteIndexer rebuild failed — is the Indexer-role process running?");

    assert_eq!(
        profile_display_name(&pool, local_identity).await.as_deref(),
        Some("direct-rebuild")
    );
    assert_eq!(
        profile_display_name(&pool, remote_identity)
            .await
            .as_deref(),
        Some("remote-rebuild")
    );
}

/// The clear-failure-mode proof issue #661 explicitly asks for: once the
/// "Indexer role" process this test has been calling is gone, the very
/// next call must fail fast (well under `RemoteIndexer`'s own 10s
/// client-side timeout — this asserts a much tighter bound, since a killed
/// process's listening socket closes immediately, giving a fast connection
/// refusal rather than a real timeout) with `IndexError::RemoteUnreachable`
/// — never a hang, and never indistinguishable from "the data doesn't
/// exist" (`IndexError::Storage`/`DisplayNameTaken`).
///
/// This test does the killing itself when `AVALON_INTERNAL_ROLE_SERVER_PID`
/// is set (the PID the manually-started process above printed on
/// startup — `avalon-server` logs its own process id at boot). Run last
/// (`--test-threads=1` in the module doc comment's invocation keeps test
/// order predictable) since it deliberately ends the process the other
/// two tests in this file depend on.
#[tokio::test]
#[ignore]
async fn remote_indexer_reports_a_clear_failure_when_the_role_is_killed() {
    let pool = setup_schema().await;
    let remote_indexer = RemoteIndexer::new(remote_indexer_url(), Some(role_key()));

    // First: prove the process really is up before killing it — a failure
    // here means the process was never running, not that killing it works.
    let liveness_identity = Uuid::new_v4();
    seed_identity(&pool, liveness_identity).await;
    remote_indexer
        .apply(&identity_created_event(liveness_identity, "pre-kill-liveness-check"))
        .await
        .expect("Indexer-role process must be running before this test kills it — see this file's own module doc comment for how to start it");

    let Ok(pid) = std::env::var("AVALON_INTERNAL_ROLE_SERVER_PID") else {
        eprintln!(
            "AVALON_INTERNAL_ROLE_SERVER_PID not set — skipping the actual kill. \
             To exercise it, kill the process manually (kill -9 <pid>) and re-run \
             just this test, or set AVALON_INTERNAL_ROLE_SERVER_PID and re-run."
        );
        return;
    };
    let status = std::process::Command::new("kill")
        .args(["-9", &pid])
        .status()
        .expect("failed to invoke `kill`");
    assert!(status.success(), "`kill -9 {pid}` failed");

    // Give the OS a moment to actually tear the socket down before the
    // next connection attempt.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let started = Instant::now();
    let err = remote_indexer
        .apply(&identity_created_event(
            Uuid::new_v4(),
            "post-kill-should-fail",
        ))
        .await
        .expect_err("apply against a killed Indexer-role process must fail, not succeed");
    let elapsed = started.elapsed();

    assert!(
        matches!(err, IndexError::RemoteUnreachable(_)),
        "expected IndexError::RemoteUnreachable, got: {err:?}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "a killed process should fail fast (connection refused), not hang toward the 10s client timeout — took {elapsed:?}"
    );
}
