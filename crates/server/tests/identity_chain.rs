//! Live checks that the server chains an identity's own events and freezes
//! key-dependent operations while its chain is forked. Gated `--ignored`:
//! needs a running `avalon-server` and migrated Postgres.

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

async fn test_pool() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new().connect(&database_url).await.unwrap()
}

async fn seed_identity_session(pool: &PgPool) -> (Uuid, String) {
    let identity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(identity_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(format!("chain-live-{identity_id}"))
        .execute(pool)
        .await
        .unwrap();
    let token = format!("test-token-{}", Uuid::new_v4());
    sqlx::query("INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)")
        .bind(&token)
        .bind(identity_id)
        .bind(OffsetDateTime::now_utc() + time::Duration::hours(1))
        .execute(pool)
        .await
        .unwrap();
    (identity_id, token)
}

async fn patch_bio(http: &reqwest::Client, token: &str, bio: &str) -> reqwest::StatusCode {
    http.patch(format!("{}/me", server_url()))
        .bearer_auth(token)
        .json(&serde_json::json!({ "bio": bio }))
        .send()
        .await
        .expect("request failed — is the server running?")
        .status()
}

#[tokio::test]
#[ignore]
async fn profile_edits_are_chained_and_reach_the_ledger_with_their_position() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let (identity_id, token) = seed_identity_session(&pool).await;

    assert!(patch_bio(&http, &token, "one").await.is_success());
    assert!(patch_bio(&http, &token, "two").await.is_success());

    let state =
        sqlx::query("SELECT seq, forked_at_seq FROM identity_chain_state WHERE identity_id = $1")
            .bind(identity_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state.try_get::<i64, _>("seq").unwrap(), 2);
    assert_eq!(
        state.try_get::<Option<i64>, _>("forked_at_seq").unwrap(),
        None
    );

    let started = std::time::Instant::now();
    loop {
        let rows = sqlx::query(
            "SELECT payload FROM ledger_entries WHERE kind = 'profile.updated' AND subject LIKE $1 \
             ORDER BY seq",
        )
        .bind(format!("identity:{identity_id}:%"))
        .fetch_all(&pool)
        .await
        .unwrap();
        if rows.len() == 2 {
            let seqs: Vec<i64> = rows
                .iter()
                .map(|r| {
                    let p: serde_json::Value = r.try_get("payload").unwrap();
                    p["_identity_chain"]["seq"].as_i64().unwrap()
                })
                .collect();
            assert_eq!(seqs, vec![1, 2]);
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "profile.updated events never reached the ledger"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test]
#[ignore]
async fn a_forked_identity_refuses_key_dependent_operations_but_allows_profile_edits() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (identity_id, token) = seed_identity_session(&pool).await;

    assert!(patch_bio(&http, &token, "before").await.is_success());
    sqlx::query("UPDATE identity_chain_state SET forked_at_seq = 2 WHERE identity_id = $1")
        .bind(identity_id)
        .execute(&pool)
        .await
        .unwrap();

    let revoke_device = http
        .post(format!("{base}/me/devices/{}/revoke", Uuid::new_v4()))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(revoke_device.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = revoke_device.json().await.unwrap();
    assert_eq!(body["code"], "IDENTITY_CHAIN_FORKED");

    let revoke_passkey = http
        .post(format!("{base}/me/passkeys/{}/revoke", Uuid::new_v4()))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(revoke_passkey.status(), reqwest::StatusCode::CONFLICT);

    // Profile edits stay available; they are simply unchained while forked.
    assert!(patch_bio(&http, &token, "after").await.is_success());
    let seq: i64 =
        sqlx::query_scalar("SELECT seq FROM identity_chain_state WHERE identity_id = $1")
            .bind(identity_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(seq, 1);
}
