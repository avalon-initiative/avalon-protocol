//! Exercises `GET /me/history` (issue #121) against a real, running
//! `avalon-server` and Postgres. Gated `--ignored` since it needs live
//! infra — see `make test-live` / `make start`.
//!
//! Ledger entries are seeded directly via SQL rather than through the
//! outbox worker — same reasoning `crates/server/tests/friends.rs` already
//! documents for seeding identities/sessions directly: this endpoint
//! doesn't care how an entry got into `ledger_entries`, only that it reads
//! back correctly filtered by issuer.

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

async fn test_pool() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

async fn seed_identity_session(pool: &PgPool) -> (Uuid, String) {
    let identity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(identity_id)
        .execute(pool)
        .await
        .expect("failed to seed identity");
    sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(format!("history-test-{identity_id}"))
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

/// Appends one ledger entry directly, issued by `identity_id` under `verb`
/// (mirrors `crates/server/src/friends.rs`'s `identity_ref` issuer shape:
/// `identity:<id>:self:<verb>`). `prev_hash`/`entry_hash` are junk — this
/// endpoint doesn't verify chain integrity (see
/// `avalon_chain::PostgresSettlementProvider::list_entries_for_issuer_prefix`'s
/// own docs on why), only reads issuer/kind/subject/payload/timestamp back.
async fn seed_ledger_entry(pool: &PgPool, identity_id: Uuid, kind: &str, verb: &str) {
    // ledger_entries.batch_id is NOT NULL with an FK to ledger_batches
    // (issue #38) — this endpoint doesn't care about real batching either
    // (same "junk is fine" reasoning as prev_hash/entry_hash above), but a
    // referenced row still has to exist.
    let batch_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO ledger_batches (batch_id, first_seq, last_seq, batch_root) \
         VALUES ($1, 0, 0, 'seed')",
    )
    .bind(batch_id)
    .execute(pool)
    .await
    .expect("failed to seed ledger batch");

    let issuer = format!("identity:{identity_id}:self:{verb}");
    sqlx::query(
        r#"
        INSERT INTO ledger_entries
            (event_id, kind, issuer, subject, payload, event_timestamp, version, prev_hash, entry_hash, batch_id)
        VALUES ($1, $2, $3, $4, $5, $6, 1, 'seed', $7, $8)
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(kind)
    .bind(&issuer)
    .bind(&issuer)
    .bind(serde_json::json!({ "identity_id": identity_id }))
    .bind(OffsetDateTime::now_utc())
    .bind(format!("seed-{}", Uuid::new_v4()))
    .bind(batch_id)
    .execute(pool)
    .await
    .expect("failed to seed ledger entry");
}

fn auth(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request.bearer_auth(token)
}

#[tokio::test]
#[ignore]
async fn my_history_returns_only_the_callers_own_events() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (alice_id, alice_token) = seed_identity_session(&pool).await;
    let (bob_id, bob_token) = seed_identity_session(&pool).await;

    seed_ledger_entry(&pool, alice_id, "identity.created", "created").await;
    seed_ledger_entry(&pool, alice_id, "friend.requested", "friend_requested").await;
    seed_ledger_entry(&pool, bob_id, "identity.created", "created").await;

    let alice_history: serde_json::Value =
        auth(http.get(format!("{base}/me/history")), &alice_token)
            .send()
            .await
            .expect("history request failed — is `make start` running?")
            .json()
            .await
            .unwrap();
    let alice_entries = alice_history.as_array().unwrap();
    assert_eq!(alice_entries.len(), 2);
    assert!(alice_entries.iter().all(|e| e["subject"]
        .as_str()
        .unwrap()
        .contains(&alice_id.to_string())));

    let bob_history: serde_json::Value = auth(http.get(format!("{base}/me/history")), &bob_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(bob_history.as_array().unwrap().len(), 1);

    // These rows carry junk (non-hex) entry_hash/prev_hash values by
    // design — see seed_ledger_entry's doc comment. Left in place, the
    // most recent one becomes `PostgresSettlementProvider::tip_hash`'s
    // answer forever, since it just reads the latest `ledger_entries` row
    // unconditionally — which permanently breaks every real
    // `SettlementProvider::commit` after this test runs, since it can
    // never parse that value as hex. Must not outlive the test.
    cleanup_seeded_ledger_entries(&pool, &[alice_id, bob_id]).await;
}

async fn cleanup_seeded_ledger_entries(pool: &PgPool, identity_ids: &[Uuid]) {
    for id in identity_ids {
        sqlx::query("DELETE FROM ledger_entries WHERE issuer LIKE $1")
            .bind(format!("identity:{id}:%"))
            .execute(pool)
            .await
            .expect("failed to clean up seeded ledger entries");
    }
}

#[tokio::test]
#[ignore]
async fn my_history_requires_a_session() {
    let http = reqwest::Client::new();
    let base = server_url();

    let response = http.get(format!("{base}/me/history")).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}
