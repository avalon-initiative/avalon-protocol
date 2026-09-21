//! Exercises `GET /ledger/entries?subject=` (issue #364, implementing one
//! of #306's decided pieces) against a real, running `avalon-server` and
//! Postgres. Gated `--ignored` since it needs live infra — see `make
//! test-live` / `make start`.
//!
//! `subject` matches the exact compound value `ledger_entries.subject`
//! stores and `LedgerEntryResponse::subject` echoes back — e.g.
//! `identity:<uuid>:self:achievement_issued` — not just the bare owner id;
//! every event-producing module in this codebase bakes its own verb into
//! `subject` (see `crate::integrators::issuer_ref` and its per-module
//! `identity_ref`/`guild_ref` siblings), so a caller filters on the exact
//! string it already knows it wrote, same as this test does.
//!
//! A ledger entry only gets a `seq`/becomes queryable once the outbox
//! worker (`crates/server/src/outbox.rs`, 3s poll) actually drains it into
//! a committed batch, so this polls briefly after issuing before querying.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
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
        .bind(format!("ledger-subject-test-{identity_id}"))
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

/// #697/#698: `POST /integrations/{slug}/connect` is signature-required
/// -- seeds a real signing key for `identity_id` so a connect call can
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
    let message = format!(
        "avalon:integration.connect:v1:{slug}:{}",
        capabilities.join(",")
    );
    BASE64.encode(signing_key.sign(message.as_bytes()).to_bytes())
}

struct RegisteredIntegrator {
    signing_key: SigningKey,
    slug: String,
    key_id: Uuid,
}

async fn register_integrator(http: &reqwest::Client, base: &str) -> RegisteredIntegrator {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let slug = format!("test-ledger-subj-{}", &suffix[..8]);
    let body = serde_json::json!({
        "slug": slug,
        "name": format!("Ledger Subject Filter Test {}", &suffix[..8]),
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
    let key_id: Uuid = registered["credential"]["key_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    RegisteredIntegrator {
        signing_key,
        slug,
        key_id,
    }
}

async fn integrator_challenge(
    http: &reqwest::Client,
    base: &str,
    integrator: &RegisteredIntegrator,
) -> (String, Vec<u8>) {
    let challenge: serde_json::Value = http
        .post(format!("{base}/integrations/{}/challenge", integrator.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap().to_string();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    (challenge_id, nonce)
}

async fn define_achievement(
    http: &reqwest::Client,
    base: &str,
    integrator: &RegisteredIntegrator,
    key: &str,
) {
    let (challenge_id, nonce) = integrator_challenge(http, base, integrator).await;
    let signature = integrator.signing_key.sign(&nonce);

    let response = http
        .post(format!(
            "{base}/integrations/{}/achievements",
            integrator.slug
        ))
        .header("x-avalon-integrator-key-id", integrator.key_id.to_string())
        .header("x-avalon-integrator-challenge-id", challenge_id)
        .header(
            "x-avalon-integrator-signature",
            BASE64.encode(signature.to_bytes()),
        )
        .json(&serde_json::json!({
            "key": key,
            "name": key,
            "description": key,
        }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
}

async fn connect(
    http: &reqwest::Client,
    base: &str,
    pool: &PgPool,
    integrator: &RegisteredIntegrator,
    identity_id: Uuid,
    identity_token: &str,
) {
    let (signing_key_id, signing_key) = seed_signing_key(pool, identity_id).await;
    let capabilities = ["achievements.issue"];
    let response = http
        .post(format!("{base}/integrations/{}/connect", integrator.slug))
        .bearer_auth(identity_token)
        .json(&serde_json::json!({
            "capabilities": capabilities,
            "signing_key_id": signing_key_id,
            "signature": sign_connect(&signing_key, &integrator.slug, &capabilities),
        }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
}

/// Issues `key` to `subject_id`, returning the exact `subject` string this
/// write's ledger entry carries — `identity:<subject_id>:self:achievement_issued`.
async fn issue_and_get_ledger_subject(
    http: &reqwest::Client,
    base: &str,
    integrator: &RegisteredIntegrator,
    subject_id: Uuid,
    key: &str,
) -> String {
    let issuer_ref = format!("game:{}", integrator.slug);
    let signing_bytes = format!(
        "avalon:achievement.issued:v1:{issuer_ref}:{subject_id}:game:{}:achievement:{key}",
        integrator.slug
    )
    .into_bytes();
    let signature = integrator.signing_key.sign(&signing_bytes);

    let (challenge_id, nonce) = integrator_challenge(http, base, integrator).await;
    let challenge_signature = integrator.signing_key.sign(&nonce);

    let response = http
        .post(format!(
            "{base}/integrations/{}/achievements/{key}/issue",
            integrator.slug
        ))
        .header("x-avalon-integrator-key-id", integrator.key_id.to_string())
        .header("x-avalon-integrator-challenge-id", challenge_id)
        .header(
            "x-avalon-integrator-signature",
            BASE64.encode(challenge_signature.to_bytes()),
        )
        .header("x-avalon-identity-id", subject_id.to_string())
        .json(&serde_json::json!({
            "key_id": integrator.key_id,
            "signature": BASE64.encode(signature.to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());

    format!("identity:{subject_id}:self:achievement_issued")
}

/// Polls `ledger_entries` for a row with this exact `subject`, up to ~30s —
/// the outbox worker's own drain cadence.
async fn wait_for_committed(pool: &PgPool, subject: &str) {
    for _ in 0..15 {
        if sqlx::query("SELECT 1 FROM ledger_entries WHERE subject = $1")
            .bind(subject)
            .fetch_optional(pool)
            .await
            .expect("query failed")
            .is_some()
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    panic!("entry for subject `{subject}` never appeared in ledger_entries — is the outbox worker running?");
}

#[tokio::test]
#[ignore]
async fn subject_filter_returns_only_that_subjects_entries_in_seq_order() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;

    let integrator = register_integrator(&http, &base).await;
    define_achievement(&http, &base, &integrator, "dragon_slayer").await;
    define_achievement(&http, &base, &integrator, "lost_city").await;

    let (subject_a, token_a) = seed_identity_session(&pool).await;
    connect(&http, &base, &pool, &integrator, subject_a, &token_a).await;
    let (subject_b, token_b) = seed_identity_session(&pool).await;
    connect(&http, &base, &pool, &integrator, subject_b, &token_b).await;

    // Interleave writes across both subjects so a naive "just take the
    // last N" implementation couldn't accidentally pass this.
    let subject_a_str =
        issue_and_get_ledger_subject(&http, &base, &integrator, subject_a, "dragon_slayer").await;
    let subject_b_str =
        issue_and_get_ledger_subject(&http, &base, &integrator, subject_b, "dragon_slayer").await;
    issue_and_get_ledger_subject(&http, &base, &integrator, subject_a, "lost_city").await;

    wait_for_committed(&pool, &subject_a_str).await;
    wait_for_committed(&pool, &subject_b_str).await;

    let filtered: Vec<serde_json::Value> = http
        .get(format!("{base}/ledger/entries"))
        .query(&[("since_seq", "0"), ("subject", &subject_a_str)])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(
        filtered.len(),
        2,
        "expected exactly subject_a's two achievement issuances, got {filtered:?}"
    );
    assert!(filtered
        .iter()
        .all(|entry| entry["subject"] == subject_a_str));
    // Same seq order the unfiltered endpoint uses.
    assert!(filtered[0]["seq"].as_i64().unwrap() < filtered[1]["seq"].as_i64().unwrap());
}

#[tokio::test]
#[ignore]
async fn subject_filter_for_a_subject_with_no_entries_returns_an_empty_list() {
    let http = reqwest::Client::new();
    let base = server_url();

    let nonexistent_subject = format!("identity:{}:self:achievement_issued", Uuid::new_v4());
    let filtered: Vec<serde_json::Value> = http
        .get(format!("{base}/ledger/entries"))
        .query(&[("since_seq", "0"), ("subject", &nonexistent_subject)])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(
        filtered.is_empty(),
        "a subject with no entries must return an empty list, not an error: {filtered:?}"
    );
}
