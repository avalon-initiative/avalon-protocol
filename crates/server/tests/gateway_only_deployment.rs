//! Live proof that a Gateway-only `avalon-server` process
//! (`AVALON_NODE_ROLES=gateway`, no local `PostgresIndexer`) produces the
//! exact same persisted projection state as a combined process, by routing
//! every indexer write through `IndexerHandle::Remote` (the
//! `RemoteIndexer`/`/internal/indexer/*` protocol) to a separately-running
//! Indexer-capable process instead.
//!
//! Gated `--ignored`, same convention as
//! `crates/server/tests/internal_role_protocol.rs`, whose
//! schema-scoped, manually-started-second-process setup this file reuses
//! directly — see that file's own module doc comment for the general
//! pattern. Unlike that file (which talks to the second process only via a
//! bare `RemoteIndexer`, never through a full `avalon-server` HTTP
//! surface), this test hits real request handlers (`PATCH /me`) on a real
//! Gateway-only process, proving the whole call path — `AppState.indexer`
//! resolved to `IndexerHandle::Remote` at startup,
//! `IndexerHandle::apply_in_tx` (a no-op) inside the handler's own Postgres
//! transaction, `IndexerHandle::apply_after_commit` making the real HTTP
//! call once that transaction commits — not just the `RemoteIndexer`
//! trait implementation in isolation.
//!
//! This test does not spawn or kill either `avalon-server` process itself
//! — start both exactly as documented below (a manually-run pair of
//! processes, matching this crate's established live-test convention).
//!
//! ```text
//! # Terminal 1 — the "Indexer role" process: an ordinary combined process
//! # (AVALON_NODE_ROLES unset/"combined"), serving /internal/indexer/*.
//! DATABASE_URL='...?options=-c%20search_path%3Dtest_gateway_only_662' \
//! AVALON_SERVER_ADDR=127.0.0.1:18081 AVALON_NODE_URL=http://127.0.0.1:18081 \
//! AVALON_INTERNAL_ROLE_KEY=test-gateway-only-role-key-662 \
//! AVALON_BOOTSTRAP_PEERS= AVALON_SETTLEMENT_REMOTE_URLS= AVALON_KNOWN_SHARDS= \
//! AVALON_MIRROR_PEERS= AVALON_SETTLEMENT_SUBMIT_KEY= AVALON_DHT_ENABLED=false \
//! cargo run -p avalon-server
//!
//! # Terminal 2 — the Gateway-only process: excludes "indexer" from its
//! # roles, so it refuses to start without AVALON_INDEXER_REMOTE_URL, and
//! # once started routes every indexer read/write to Terminal 1's process.
//! DATABASE_URL='...?options=-c%20search_path%3Dtest_gateway_only_662' \
//! AVALON_SERVER_ADDR=127.0.0.1:18082 AVALON_NODE_URL=http://127.0.0.1:18082 \
//! AVALON_NODE_ROLES=gateway \
//! AVALON_INDEXER_REMOTE_URL=http://127.0.0.1:18081 \
//! AVALON_INTERNAL_ROLE_KEY=test-gateway-only-role-key-662 \
//! AVALON_BOOTSTRAP_PEERS= AVALON_SETTLEMENT_REMOTE_URLS= AVALON_KNOWN_SHARDS= \
//! AVALON_MIRROR_PEERS= AVALON_SETTLEMENT_SUBMIT_KEY= AVALON_DHT_ENABLED=false \
//! cargo run -p avalon-server
//!
//! # Terminal 3
//! AVALON_COMBINED_SERVER_URL=http://127.0.0.1:18081 \
//! AVALON_GATEWAY_ONLY_SERVER_URL=http://127.0.0.1:18082 \
//! cargo test -p avalon-server --test gateway_only_deployment -- --ignored --test-threads=1
//!
//! # cleanup
//! psql "$DATABASE_URL" -c 'DROP SCHEMA IF EXISTS test_gateway_only_662 CASCADE'
//! ```
//!
//! Live-verified in this environment against real Postgres and two real,
//! separately-running `avalon-server` processes.

use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

const SCHEMA: &str = "test_gateway_only_662";

fn combined_server_url() -> String {
    std::env::var("AVALON_COMBINED_SERVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:18081".to_string())
}

fn gateway_only_server_url() -> String {
    std::env::var("AVALON_GATEWAY_ONLY_SERVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:18082".to_string())
}

fn base_database_url() -> String {
    std::env::var("DATABASE_URL").expect("DATABASE_URL must be set")
}

fn schema_database_url() -> String {
    let base = base_database_url();
    let sep = if base.contains('?') { "&" } else { "?" };
    format!("{base}{sep}options=-c%20search_path%3D{SCHEMA}")
}

fn migrations_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("db/migrations")
}

/// Same self-contained schema setup `internal_role_protocol.rs` uses — this
/// test's own schema-scoped `DATABASE_URL` is what both manually-started
/// processes above must also be given.
async fn setup_schema() -> PgPool {
    let admin_pool = PgPoolOptions::new()
        .connect(&base_database_url())
        .await
        .expect("failed to connect to Postgres — is it reachable?");
    sqlx::query("CREATE SCHEMA IF NOT EXISTS test_gateway_only_662")
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

/// Seeds an identity/profile/session directly via SQL — same shortcut
/// `crates/server/tests/profile_events.rs::seed_identity_session` takes to
/// avoid a full WebAuthn ceremony for a test that isn't about login itself.
async fn seed_identity_session(pool: &PgPool, label: &str) -> (Uuid, String) {
    let identity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(identity_id)
        .execute(pool)
        .await
        .expect("failed to seed identity");
    sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(format!("gateway-only-test-{label}-{identity_id}"))
        .execute(pool)
        .await
        .expect("failed to seed profile");

    let token = format!("test-token-{}", Uuid::new_v4());
    let expires_at = OffsetDateTime::now_utc() + time::Duration::hours(1);
    sqlx::query("INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)")
        .bind(&token)
        .bind(identity_id)
        .bind(expires_at)
        .execute(pool)
        .await
        .expect("failed to seed session");

    (identity_id, token)
}

async fn update_bio(http: &reqwest::Client, base: &str, token: &str, bio: &str) {
    http.patch(format!("{base}/me"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "bio": bio }))
        .send()
        .await
        .expect("PATCH /me request failed — is the process running?")
        .error_for_status()
        .expect("PATCH /me should succeed");
}

async fn profile_bio(pool: &PgPool, identity_id: Uuid) -> Option<String> {
    sqlx::query("SELECT bio FROM profiles WHERE identity_id = $1")
        .bind(identity_id)
        .fetch_one(pool)
        .await
        .expect("query failed")
        .get::<Option<String>, _>("bio")
}

/// The core #662 acceptance bar: a `PATCH /me` handled entirely by a
/// Gateway-only process (no local `PostgresIndexer`, routing through
/// `IndexerHandle::Remote`) persists the exact same `profiles` row shape a
/// combined process's own handling of the identical request would — same
/// schema, same table, read back with one plain SQL query so neither
/// process's own read path can mask a difference between them.
///
/// By the time `update_bio` returns, `apply_after_commit`'s real HTTP call
/// to the combined process has already been awaited inside the handler
/// (see `AppState::indexer`'s own doc comment) — so no polling/retry is
/// needed here the way `profile_events.rs`'s outbox-lag test needs one for
/// the ledger; the projection is expected to already be correct the
/// instant the HTTP response comes back.
#[tokio::test]
#[ignore]
async fn profile_update_through_gateway_only_process_matches_a_combined_process() {
    let pool = setup_schema().await;
    let http = reqwest::Client::new();

    let (combined_identity, combined_token) = seed_identity_session(&pool, "combined").await;
    let (gateway_identity, gateway_token) = seed_identity_session(&pool, "gateway-only").await;

    update_bio(
        &http,
        &combined_server_url(),
        &combined_token,
        "bio set via the combined process's own local PostgresIndexer",
    )
    .await;
    update_bio(
        &http,
        &gateway_only_server_url(),
        &gateway_token,
        "bio set via the Gateway-only process's RemoteIndexer",
    )
    .await;

    let combined_bio = profile_bio(&pool, combined_identity).await;
    let gateway_bio = profile_bio(&pool, gateway_identity).await;

    assert_eq!(
        combined_bio.as_deref(),
        Some("bio set via the combined process's own local PostgresIndexer")
    );
    assert_eq!(
        gateway_bio.as_deref(),
        Some("bio set via the Gateway-only process's RemoteIndexer"),
        "a PATCH /me handled by a Gateway-only process must land in the shared `profiles` \
         projection exactly like a combined process's own request does — proving \
         IndexerHandle::Remote's apply_in_tx (no-op) + apply_after_commit (real HTTP call) \
         split produces the same end state as IndexerHandle::Local's single atomic apply_in_tx"
    );
}

/// A second, independent write path through the same Gateway-only process —
/// `GET /me` right after the `PATCH` reading back through the *same*
/// process's own read path (`avalon_indexer::projections::profiles::fetch`
/// against its local Postgres pool — reads never go over the
/// `RemoteIndexer` wire, only writes do, since the projection tables
/// themselves are still the one shared Postgres database every role reads
/// directly), proving a Gateway-only process is self-consistent, not just
/// consistent with a combined process's separate view.
#[tokio::test]
#[ignore]
async fn gateway_only_process_reads_back_its_own_remote_write_immediately() {
    let pool = setup_schema().await;
    let http = reqwest::Client::new();
    let base = gateway_only_server_url();

    let (_identity_id, token) = seed_identity_session(&pool, "self-read").await;
    update_bio(&http, &base, &token, "self-read-after-remote-write").await;

    let me: serde_json::Value = http
        .get(format!("{base}/me"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("GET /me request failed")
        .json()
        .await
        .expect("GET /me should return JSON");

    assert_eq!(
        me["bio"].as_str(),
        Some("self-read-after-remote-write"),
        "GET /me on the same Gateway-only process must reflect a PATCH /me it just handled \
         itself, since apply_after_commit is awaited before the PATCH handler returns"
    );
}
