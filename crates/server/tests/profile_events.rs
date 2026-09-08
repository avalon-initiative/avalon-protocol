//! Exercises `profile.updated` (issue #86) against a real, running
//! `avalon-server`, Postgres, and outbox worker. Gated `--ignored` since it
//! needs live infra — see `make test-live` / `make start`.
//!
//! Reads the result back through `GET /me/history` rather than peeking at
//! `protocol_outbox` directly: the property that matters is that the change
//! actually reaches the ledger, which means waiting for the outbox worker's
//! next tick (3s) rather than asserting the moment the request returns.

use std::time::Duration;

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
        .bind(format!("profile-events-test-{identity_id}"))
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

async fn history(http: &reqwest::Client, base: &str, token: &str) -> Vec<serde_json::Value> {
    http.get(format!("{base}/me/history"))
        .bearer_auth(token)
        .send()
        .await
        .expect("history request failed — is `make start` running?")
        .json::<Vec<serde_json::Value>>()
        .await
        .unwrap()
}

/// Polls `GET /me/history` until `predicate` holds or `timeout` elapses —
/// the outbox worker drains on its own schedule, so the ledger lags the
/// request by up to one tick.
async fn wait_for_history(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    timeout: Duration,
    predicate: impl Fn(&[serde_json::Value]) -> bool,
) -> Vec<serde_json::Value> {
    let started = std::time::Instant::now();
    loop {
        let entries = history(http, base, token).await;
        if predicate(&entries) || started.elapsed() > timeout {
            return entries;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test]
#[ignore]
async fn a_display_name_change_reaches_the_ledger_as_profile_updated() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (_identity_id, token) = seed_identity_session(&pool).await;

    let update = http
        .patch(format!("{base}/me"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "display_name": "renamed-by-test" }))
        .send()
        .await
        .expect("update request failed — is `make start` running?");
    assert!(update.status().is_success(), "{:?}", update.status());
    let profile: serde_json::Value = update.json().await.unwrap();
    let handle = profile["handle"].as_str().unwrap().to_string();
    let discriminator = handle.rsplit_once('#').unwrap().1.to_string();

    let entries = wait_for_history(&http, &base, &token, Duration::from_secs(15), |entries| {
        entries.iter().any(|e| e["kind"] == "profile.updated")
    })
    .await;
    let event = entries
        .iter()
        .find(|e| e["kind"] == "profile.updated")
        .expect("profile.updated never reached the ledger");

    assert_eq!(event["payload"]["display_name"], "renamed-by-test");
    // The discriminator in the event is the one the handle actually landed
    // on — what a rebuild would need to reproduce `name#1234` exactly.
    assert_eq!(event["payload"]["discriminator"], discriminator);
    // avatar_url wasn't part of this request, so it isn't part of the event.
    assert!(event["payload"].get("avatar_url").is_none());
}

#[tokio::test]
#[ignore]
async fn clearing_the_avatar_is_recorded_as_an_explicit_null() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (_identity_id, token) = seed_identity_session(&pool).await;

    for body in [
        serde_json::json!({ "avatar_url": "https://example.com/a.png" }),
        serde_json::json!({ "avatar_url": "" }),
    ] {
        let update = http
            .patch(format!("{base}/me"))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(update.status().is_success(), "{:?}", update.status());
    }

    let entries = wait_for_history(&http, &base, &token, Duration::from_secs(15), |entries| {
        entries
            .iter()
            .filter(|e| e["kind"] == "profile.updated")
            .count()
            >= 2
    })
    .await;
    let avatar_events: Vec<&serde_json::Value> = entries
        .iter()
        .filter(|e| e["kind"] == "profile.updated")
        .collect();
    assert_eq!(avatar_events.len(), 2);

    // Newest first (see `list_entries_for_issuer_prefix`): the clear, then
    // the set.
    assert!(avatar_events[0]["payload"]["avatar_url"].is_null());
    assert!(avatar_events[0]["payload"].get("avatar_url").is_some());
    assert_eq!(
        avatar_events[1]["payload"]["avatar_url"],
        "https://example.com/a.png"
    );
    // Neither request touched the display name.
    assert!(avatar_events[0]["payload"].get("display_name").is_none());
}

#[tokio::test]
#[ignore]
async fn a_request_that_changes_nothing_emits_nothing() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (_identity_id, token) = seed_identity_session(&pool).await;

    let update = http
        .patch(format!("{base}/me"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert!(update.status().is_success(), "{:?}", update.status());

    // Give the outbox worker more than one tick to publish anything it
    // might wrongly have been handed.
    tokio::time::sleep(Duration::from_secs(4)).await;
    let entries = history(&http, &base, &token).await;
    assert!(
        entries.iter().all(|e| e["kind"] != "profile.updated"),
        "a no-op update must not emit profile.updated"
    );
}
