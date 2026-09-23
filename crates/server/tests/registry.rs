//! Exercises `GET /integrations/{slug}/registry` against a real, running `avalon-server` and Postgres.
//! Gated `--ignored` since it needs live infra — see `make test-live` /
//! `make start`. Skipped in this sandbox (no
//! reachable Postgres here); written but not run against a live database.
//!
//! Mirrors `crates/server/tests/integrations.rs`'s own pattern for the
//! challenge-response integrator-auth flow and `crates/server/tests/connections.rs`'s
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

/// #697/#698: `POST /integrations/{slug}/connect` is signature-required —
/// seeds a real signing key for `identity_id` so a connect call can
/// produce a genuine fresh signature over HTTP.
async fn seed_signing_key(pool: &PgPool, identity_id: Uuid) -> (Uuid, SigningKey) {
    let signing_key = SigningKey::generate(&mut rand::rng());
    let public_key = signing_key.verifying_key().to_bytes();
    let row = sqlx::query(
        "INSERT INTO identity_signing_keys (identity_id, public_key) VALUES ($1, $2) RETURNING id",
    )
    .bind(identity_id)
    .bind(public_key.as_slice())
    .fetch_one(pool)
    .await
    .expect("failed to seed signing key");
    (sqlx::Row::try_get(&row, "id").unwrap(), signing_key)
}

/// Mirrors `crate::signature_gate::canonical_message` byte-for-byte.
fn sign_connect(signing_key: &SigningKey, slug: &str, capabilities: &[&str]) -> String {
    use ed25519_dalek::Signer;
    let message = format!(
        "avalon:integration.connect:v1:{slug}:{}",
        capabilities.join(",")
    );
    BASE64.encode(signing_key.sign(message.as_bytes()).to_bytes())
}

async fn register_unique_integrator(http: &reqwest::Client, base: &str) -> (String, SigningKey) {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let slug = format!("test-registry-{}", &suffix[..12]);
    let body = serde_json::json!({
        "slug": slug,
        "name": format!("Registry Test Integrator {}", &suffix[..8]),
        "owner_name": "Test Studio",
        "requested_capabilities": [],
        "initial_key": {
            "algorithm": "ed25519",
            "public_key": BASE64.encode(signing_key.verifying_key().as_bytes()),
        },
    });

    let response = http
        .post(format!("{base}/integrations"))
        .json(&body)
        .send()
        .await
        .expect("register integrator failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());
    (slug, signing_key)
}

/// Every metric is present with its `durable-derived` class label and a
/// non-empty definition — the ticket's hard contract — and every count is
/// zero, not an error or a missing field, for an integrator with no binding or
/// achievement activity at all.
#[tokio::test]
#[ignore]
async fn a_integrator_with_no_activity_returns_zeros_for_every_labeled_metric() {
    let http = reqwest::Client::new();
    let base = server_url();
    let (slug, _) = register_unique_integrator(&http, &base).await;

    let response = http
        .get(format!("{base}/integrations/{slug}/registry"))
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
        // so an integrator with no activity yet still reads as an exact 0, not a
        // withheld/"fewer than" value.
        assert_eq!(metric["exact"], true, "{field} should be exact at zero");
    }
}

/// One identity binds to the integrator (`POST /integrations/{slug}/connect`, a real
/// `game.binding_established` event through the real indexer path) and two
/// attestations are seeded directly into `indexer_attestations` (one
/// later revoked) for a second identity — every resulting cohort here
/// (1 player, 2 issued, 1 revoked, 1 unique holder) sits below the
/// server's default minimum-cohort floor,
/// `avalon_indexer::registry::DEFAULT_MIN_COHORT` = 5 unless
/// `AVALON_REGISTRY_MIN_COHORT` overrides it), so every metric here comes
/// back coarsened to the floor itself with `exact: false` — never the real
/// sub-floor count.
#[tokio::test]
#[ignore]
async fn a_integrator_with_activity_below_the_floor_reports_coarsened_not_exact_counts() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (identity_id, token) = seed_identity_session(&pool).await;
    let (signing_key_id, signing_key) = seed_signing_key(&pool, identity_id).await;
    let (slug, _) = register_unique_integrator(&http, &base).await;

    let connect_capabilities: [&str; 0] = [];
    let connect = http
        .post(format!("{base}/integrations/{slug}/connect"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "capabilities": connect_capabilities,
            "signing_key_id": signing_key_id,
            "signature": sign_connect(&signing_key, &slug, &connect_capabilities),
        }))
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
        .get(format!("{base}/integrations/{slug}/registry"))
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

/// Five distinct identities bind to the integrator — a cohort exactly at the
/// server's default minimum-cohort floor,
/// `avalon_indexer::registry::DEFAULT_MIN_COHORT` = 5) — so `players`/
/// `total_players_ever` come back as the real, exact count rather than
/// coarsened. Proves the floor is a lower bound on what's ever withheld,
/// not a blanket rounding applied to every metric regardless of size.
#[tokio::test]
#[ignore]
async fn a_integrator_with_activity_at_the_floor_reports_exact_counts() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (slug, _) = register_unique_integrator(&http, &base).await;

    let connect_capabilities: [&str; 0] = [];
    for _ in 0..5 {
        let (identity_id, token) = seed_identity_session(&pool).await;
        let (signing_key_id, signing_key) = seed_signing_key(&pool, identity_id).await;
        let connect = http
            .post(format!("{base}/integrations/{slug}/connect"))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "capabilities": connect_capabilities,
                "signing_key_id": signing_key_id,
                "signature": sign_connect(&signing_key, &slug, &connect_capabilities),
            }))
            .send()
            .await
            .unwrap();
        assert!(connect.status().is_success(), "{:?}", connect.status());
    }

    let body: serde_json::Value = http
        .get(format!("{base}/integrations/{slug}/registry"))
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

/// Issue #95: the dedicated `GET /registry/{slug}` external read surface —
/// same handler, same data, as `GET /integrations/{slug}/registry`, but
/// under its own top-level namespace so it reads as a standalone contract
/// rather than something buried inside integrator-management routes.
#[tokio::test]
#[ignore]
async fn external_registry_route_matches_the_integrator_scoped_one_field_for_field() {
    let http = reqwest::Client::new();
    let base = server_url();
    let (slug, _) = register_unique_integrator(&http, &base).await;

    let via_integrations: serde_json::Value = http
        .get(format!("{base}/integrations/{slug}/registry"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let via_registry: serde_json::Value = http
        .get(format!("{base}/registry/{slug}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(
        via_integrations, via_registry,
        "the external route must return byte-for-byte the same contract as the internal one"
    );
}

/// Issue #95's contract test: every field in the response carries both a
/// `definition` and a `class` label — the requirement this whole namespacing
/// scheme (a separate `{ value, definition, class, exact }` shape rather
/// than flattening metrics onto `IntegratorPublicResponse`) exists to make
/// structurally impossible to skip. Fails loudly (naming the field) if any
/// future metric is ever added without both.
#[tokio::test]
#[ignore]
async fn every_metric_field_carries_a_definition_and_a_class_label() {
    let http = reqwest::Client::new();
    let base = server_url();
    let (slug, _) = register_unique_integrator(&http, &base).await;

    let body: serde_json::Value = http
        .get(format!("{base}/registry/{slug}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let fields = body.as_object().expect("response must be a JSON object");
    assert!(
        !fields.is_empty(),
        "the registry response must not be empty"
    );
    for (name, metric) in fields {
        assert!(
            metric["definition"].as_str().is_some_and(|s| !s.is_empty()),
            "{name} is missing a non-empty definition"
        );
        assert!(
            matches!(
                metric["class"].as_str(),
                Some("durable-derived") | Some("realtime") | Some("self-reported")
            ),
            "{name}'s class must be one of the three documented labels, got {:?}",
            metric["class"]
        );
        assert!(
            metric["value"].is_i64() || metric["value"].is_u64(),
            "{name} must carry a numeric value"
        );
        assert!(
            metric["exact"].is_boolean(),
            "{name} must carry an exact flag"
        );
    }
}

/// Issue #95's other hard invariant: never per-identity data, regardless of
/// who's asking. Every field in the response is a `{ value, definition,
/// class, exact }` metric object — this asserts none of the top-level keys
/// are anything else (an identity id, a raw list of subjects, etc.), the
/// structural guarantee behind "never returns per-identity data, ever".
#[tokio::test]
#[ignore]
async fn no_field_in_the_response_carries_per_identity_data() {
    let http = reqwest::Client::new();
    let base = server_url();
    let (slug, _) = register_unique_integrator(&http, &base).await;

    let body: serde_json::Value = http
        .get(format!("{base}/registry/{slug}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    for (name, metric) in body.as_object().unwrap() {
        let keys: Vec<&String> = metric.as_object().unwrap().keys().collect();
        assert_eq!(
            keys.len(),
            4,
            "{name} must be exactly {{ value, definition, class, exact }}, got keys {keys:?}"
        );
        for expected in ["value", "definition", "class", "exact"] {
            assert!(
                metric.get(expected).is_some(),
                "{name} is missing the {expected} field"
            );
        }
    }
}

/// A slug that was never registered 404s on the external route too, same
/// as `GET /integrations/{slug}/registry` — this is not a route where
/// "unknown slug" silently reads as zero activity.
#[tokio::test]
#[ignore]
async fn external_registry_route_404s_for_an_unregistered_slug() {
    let http = reqwest::Client::new();
    let base = server_url();

    let response = http
        .get(format!("{base}/registry/does-not-exist-{}", Uuid::new_v4()))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
}
