//! Issue #534: a revocation's coded reason controls whether the claim
//! stays visible in `GET /me/achievements` (a current-state, projection-
//! style read) after being revoked. Exercised against a real, running
//! `avalon-server` and Postgres. Gated `--ignored` since it needs live
//! infra — see `make test-live` / `make start`.
//!
//! Same fixture shape `crates/server/tests/attestation_revocations.rs`
//! already established (seed identity/session via SQL, register an
//! integrator, define + issue one achievement, then revoke it) — this
//! file only adds the `GET /me/achievements` visibility half #534
//! introduces, not a second copy of #85's own scenario C coverage.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use reqwest::header::HeaderMap;
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
        .bind(format!("reason-code-test-{identity_id}"))
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

struct RegisteredIntegrator {
    signing_key: SigningKey,
    slug: String,
    key_id: String,
}

async fn register_integrator(http: &reqwest::Client, base: &str) -> RegisteredIntegrator {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let slug = format!("test-reason-{}", &suffix[..10]);
    let body = serde_json::json!({
        "slug": slug,
        "name": format!("Reason Code Test {}", &suffix[..8]),
        "owner_name": "Test Studio",
        "requested_capabilities": ["achievements.issue"],
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
    let registered: serde_json::Value = response.json().await.unwrap();
    RegisteredIntegrator {
        signing_key,
        slug,
        key_id: registered["credential"]["key_id"]
            .as_str()
            .unwrap()
            .to_string(),
    }
}

async fn auth_headers(
    http: &reqwest::Client,
    base: &str,
    integrator: &RegisteredIntegrator,
) -> HeaderMap {
    let challenge: serde_json::Value = http
        .post(format!("{base}/integrations/{}/challenge", integrator.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let signature = integrator.signing_key.sign(&nonce);

    let mut headers = HeaderMap::new();
    headers.insert(
        "x-avalon-integrator-key-id",
        integrator.key_id.parse().unwrap(),
    );
    headers.insert(
        "x-avalon-integrator-challenge-id",
        challenge_id.parse().unwrap(),
    );
    headers.insert(
        "x-avalon-integrator-signature",
        BASE64.encode(signature.to_bytes()).parse().unwrap(),
    );
    headers
}

fn attestation_signing_bytes(issuer_ref: &str, subject: Uuid, achievement: &str) -> Vec<u8> {
    format!("avalon:achievement.issued:v1:{issuer_ref}:{subject}:{achievement}").into_bytes()
}

fn revocation_signing_bytes(issuer_ref: &str, attestation_id: Uuid, reason_code: &str) -> Vec<u8> {
    format!("avalon:achievement.revoked:v1:{issuer_ref}:{attestation_id}:{reason_code}")
        .into_bytes()
}

/// Registers an integrator, seeds a subject identity/session, defines and
/// issues one achievement, and connects the integrator to the subject —
/// everything `revoke_with_reason` and `GET /me/achievements` need.
/// Returns the integrator, subject identity's own session token, the
/// achievement's `key`, and the issued attestation id.
async fn issue_one(
    http: &reqwest::Client,
    base: &str,
    pool: &PgPool,
) -> (RegisteredIntegrator, String, Uuid, Uuid) {
    let (identity_id, token) = seed_identity_session(pool).await;
    let integrator = register_integrator(http, base).await;

    let headers = auth_headers(http, base, &integrator).await;
    let define = http
        .post(format!(
            "{base}/integrations/{}/achievements",
            integrator.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "key": "dragon_slayer",
            "name": "Dragon Slayer",
            "description": "Slew the dragon",
        }))
        .send()
        .await
        .unwrap();
    assert!(define.status().is_success(), "{:?}", define.status());
    let achievement_id = define.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let connect = http
        .post(format!("{base}/integrations/{}/connect", integrator.slug))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "capabilities": ["achievements.issue"] }))
        .send()
        .await
        .unwrap();
    assert!(connect.status().is_success(), "{:?}", connect.status());

    let issuer_ref = format!("game:{}", integrator.slug);
    let signing_bytes = attestation_signing_bytes(&issuer_ref, identity_id, &achievement_id);
    let signature = integrator.signing_key.sign(&signing_bytes);

    let mut headers = auth_headers(http, base, &integrator).await;
    headers.insert(
        "x-avalon-identity-id",
        identity_id.to_string().parse().unwrap(),
    );
    let issue = http
        .post(format!(
            "{base}/integrations/{}/achievements/dragon_slayer/issue",
            integrator.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "key_id": integrator.key_id,
            "signature": BASE64.encode(signature.to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert!(issue.status().is_success(), "{:?}", issue.status());
    let attestation_id: Uuid = issue.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    (integrator, token, identity_id, attestation_id)
}

async fn revoke_with_reason(
    http: &reqwest::Client,
    base: &str,
    integrator: &RegisteredIntegrator,
    attestation_id: Uuid,
    reason_code: &str,
) {
    let issuer_ref = format!("game:{}", integrator.slug);
    let signing_bytes = revocation_signing_bytes(&issuer_ref, attestation_id, reason_code);
    let signature = integrator.signing_key.sign(&signing_bytes);

    let headers = auth_headers(http, base, integrator).await;
    let revoke = http
        .post(format!("{base}/attestations/{attestation_id}/revoke"))
        .headers(headers)
        .json(&serde_json::json!({
            "key_id": integrator.key_id,
            "signature": BASE64.encode(signature.to_bytes()),
            "reason_code": reason_code,
            "reason": format!("test revocation ({reason_code})"),
        }))
        .send()
        .await
        .unwrap();
    assert!(revoke.status().is_success(), "{:?}", revoke.status());
}

/// A `cheating`-coded revocation still shows up in `GET /me/achievements`
/// — regression coverage against #85's existing behavior, which #534
/// must not change for this reason code.
#[tokio::test]
#[ignore]
async fn a_cheating_coded_revocation_still_appears_in_current_state_listing() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (integrator, token, _identity_id, attestation_id) = issue_one(&http, &base, &pool).await;

    revoke_with_reason(&http, &base, &integrator, attestation_id, "cheating").await;

    let listing: serde_json::Value = http
        .get(format!("{base}/me/achievements"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = listing["achievements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&attestation_id.to_string().as_str()),
        "a cheating-coded revocation should stay visible in the current-state listing, \
         got: {ids:?}"
    );
}

/// A `mistake`-coded revocation drops out of `GET /me/achievements`
/// entirely — #534's actual new behavior. Raw history (`GET
/// /attestations/{id}`) is unaffected — still shows both entries.
#[tokio::test]
#[ignore]
async fn a_mistake_coded_revocation_disappears_from_current_state_listing_but_not_raw_history() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;
    let (integrator, token, _identity_id, attestation_id) = issue_one(&http, &base, &pool).await;

    revoke_with_reason(&http, &base, &integrator, attestation_id, "mistake").await;

    let listing: serde_json::Value = http
        .get(format!("{base}/me/achievements"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = listing["achievements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert!(
        !ids.contains(&attestation_id.to_string().as_str()),
        "a mistake-coded revocation should be hidden from the current-state listing, \
         got: {ids:?}"
    );

    // Raw history — a direct id lookup, not a "browsing" listing — still
    // shows the full truth, per revocation.md's "never erase" standard.
    let raw = http
        .get(format!("{base}/attestations/{attestation_id}"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let history = raw["history"].as_array().unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0]["event"], "issued");
    assert_eq!(history[1]["event"], "revoked");
    assert_eq!(history[1]["reason_code"], "mistake");
}
