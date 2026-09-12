//! Exercises `GET /games/{slug}/registry` (issue #261, first slice of the
//! epic-sized #89) against a real, running `avalon-server` and Postgres.
//! Gated `--ignored` since it needs live infra — see `make test-live` /
//! `make start`. Skipped in this sandbox per `.claude/CLAUDE.md` (no
//! reachable Postgres here); written but not run against a live database.
//!
//! Mirrors `crates/server/tests/games.rs`'s own pattern for the
//! challenge-response game-auth flow and `crates/server/tests/connections.rs`'s
//! pattern for seeding a bare identity/session directly via SQL rather than
//! a real WebAuthn ceremony. There is no HTTP endpoint yet to issue an
//! achievement attestation (issuance itself is Epic #30, not landed) — so
//! the "real activity" case seeds `indexer_attestations` directly, the same
//! way `crates/server/tests/connections.rs::seed_identity_session` seeds
//! `identities`/`sessions` directly for endpoints that only care the row
//! exists.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::SigningKey;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
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
        .bind(format!("registry-test-{identity_id}"))
        .execute(pool)
        .await
        .expect("failed to seed profile");

    let token = format!("test-token-{}", Uuid::new_v4());
    let expires_at = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    sqlx::query("INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)")
        .bind(&token)
        .bind(identity_id)
        .bind(expires_at)
        .execute(pool)
        .await
        .expect("failed to seed session");

    (identity_id, token)
}

async fn register_unique_game(http: &reqwest::Client, base: &str) -> (String, SigningKey) {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let slug = format!("test-registry-{}", &suffix[..12]);
    let body = serde_json::json!({
        "slug": slug,
        "name": format!("Registry Test Game {}", &suffix[..8]),
        "developer": "Test Studio",
        "requested_capabilities": [],
        "initial_key": {
            "algorithm": "ed25519",
            "public_key": BASE64.encode(signing_key.verifying_key().as_bytes()),
        },
    });

    let response = http
        .post(format!("{base}/games"))
        .json(&body)
        .send()
        .await
        .expect("register game failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());
    (slug, signing_key)
}

/// Every metric is present with its `durable-derived` class label and a
/// non-empty definition — the ticket's hard contract — and every count is
/// zero, not an error or a missing field, for a game with no binding or
/// achievement activity at all.
#[tokio::test]
#[ignore]
async fn a_game_with_no_activity_returns_zeros_for_every_labeled_metric() {
    let http = reqwest::Client::new();
    let base = server_url();
    let (slug, _) = register_unique_game(&http, &base).await;

    let response = http
        .get(format!("{base}/games/{slug}/registry"))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
    let body: serde_json::Value = response.json().await.unwrap();

    for field in [
        "players",
        "total_players_ever",
        "achievements_issued",
        "achievements_revoked",
        "unique_achievement_holders",
    ] {
        let metric = &body[field];
        assert_eq!(metric["value"], 0, "{field} should be 0 with no activity");
        assert_eq!(metric["class"], "durable-derived", "{field} class label");
        assert!(
            metric["definition"].as_str().is_some_and(|s| !s.is_empty()),
            "{field} must carry a non-empty definition"
        );
        // Issue #96: zero is never coarsened — "nobody" identifies no one,
        // so a game with no activity yet still reads as an exact 0, not a
        // withheld/"fewer than" value.
        assert_eq!(metric["exact"], true, "{field} should be exact at zero");
    }
}

/// One identity binds to the game (`POST /games/{slug}/connect`, a real
/// `game.binding_established` event through the real indexer path) and two
/// attestations are seeded directly into `indexer_attestations` (one
/// later revoked) for a second identity — every resulting cohort here
/// (1 player, 2 issued, 1 revoked, 1 unique holder) sits below the
/// server's default minimum-cohort floor (issue #96,
/// `avalon_indexer::registry::DEFAULT_MIN_COHORT` = 5 unless
/// `AVALON_REGISTRY_MIN_COHORT` overrides it), so every metric here comes
/// back coarsened to the floor itself with `exact: false` — never the real
/// sub-floor count.
#[tokio::test]
#[ignore]
async fn a_game_with_activity_below_the_floor_reports_coarsened_not_exact_counts() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (_, token) = seed_identity_session(&pool).await;
    let (slug, _) = register_unique_game(&http, &base).await;

    let connect = http
        .post(format!("{base}/games/{slug}/connect"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "capabilities": [] }))
        .send()
        .await
        .unwrap();
    assert!(connect.status().is_success(), "{:?}", connect.status());

    let issuer = format!("game:{slug}");
    let holder_valid = Uuid::new_v4();
    let holder_revoked = Uuid::new_v4();
    for holder in [holder_valid, holder_revoked] {
        sqlx::query("INSERT INTO identities (id) VALUES ($1) ON CONFLICT DO NOTHING")
            .bind(holder)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query(
        "INSERT INTO indexer_attestations (id, issuer, subject, achievement, issued_at) \
         VALUES ($1, $2, $3, $4, now())",
    )
    .bind(Uuid::new_v4())
    .bind(&issuer)
    .bind(holder_valid)
    .bind(format!("{issuer}:achievement:dragon_slayer"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO indexer_attestations (id, issuer, subject, achievement, issued_at, revoked_at) \
         VALUES ($1, $2, $3, $4, now(), now())",
    )
    .bind(Uuid::new_v4())
    .bind(&issuer)
    .bind(holder_revoked)
    .bind(format!("{issuer}:achievement:dragon_slayer"))
    .execute(&pool)
    .await
    .unwrap();

    let body: serde_json::Value = http
        .get(format!("{base}/games/{slug}/registry"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    for field in [
        "players",
        "total_players_ever",
        "achievements_issued",
        "achievements_revoked",
        "unique_achievement_holders",
    ] {
        let metric = &body[field];
        assert_eq!(
            metric["exact"], false,
            "{field} should be coarsened, not exact"
        );
        assert!(
            metric["value"].as_i64().unwrap() > 0,
            "{field}'s coarsened value should still be a positive floor, not 0"
        );
    }
}

/// Five distinct identities bind to the game — a cohort exactly at the
/// server's default minimum-cohort floor (issue #96,
/// `avalon_indexer::registry::DEFAULT_MIN_COHORT` = 5) — so `players`/
/// `total_players_ever` come back as the real, exact count rather than
/// coarsened. Proves the floor is a lower bound on what's ever withheld,
/// not a blanket rounding applied to every metric regardless of size.
#[tokio::test]
#[ignore]
async fn a_game_with_activity_at_the_floor_reports_exact_counts() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (slug, _) = register_unique_game(&http, &base).await;

    for _ in 0..5 {
        let (_, token) = seed_identity_session(&pool).await;
        let connect = http
            .post(format!("{base}/games/{slug}/connect"))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "capabilities": [] }))
            .send()
            .await
            .unwrap();
        assert!(connect.status().is_success(), "{:?}", connect.status());
    }

    let body: serde_json::Value = http
        .get(format!("{base}/games/{slug}/registry"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["players"]["value"], 5);
    assert_eq!(body["players"]["exact"], true);
    assert_eq!(body["total_players_ever"]["value"], 5);
    assert_eq!(body["total_players_ever"]["exact"], true);
}
