//! Exercises `GET /attestations/{id}` (issue #33) against a real, running
//! `avalon-server` and Postgres. Gated `--ignored` since it needs live
//! infra — see `make test-live` / `make start`.
//!
//! "Scenario D" (issue #33's own design text — not one of
//! `docs/architecture/issuers.md`'s lettered scenarios, which
//! only goes up through F today): an authentic, valid claim from an
//! issuer the reader doesn't trust must still read as
//! `Authentic`/`Valid` — recognition is a separate, consumer-side
//! question this endpoint deliberately never answers (see
//! `attestations.rs`'s own module doc comment). The recognition half is
//! exercised as a plain unit call to `avalon_protocol::achievements::recognize`
//! against a policy that doesn't name this issuer, no server round trip
//! needed since recognition is never a server computation.

use avalon_protocol::achievements::{recognize, Issuer, Recognition, TrustRelationship};
use avalon_protocol::ids::IntegratorId;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use reqwest::header::HeaderMap;
use time::OffsetDateTime;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
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
    let slug = format!("test-read-{}", &suffix[..10]);
    let body = serde_json::json!({
        "slug": slug,
        "name": format!("Read Test {}", &suffix[..8]),
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

fn attestation_signing_bytes(
    claim_kind: &str,
    issuer_ref: &str,
    subject: Uuid,
    achievement: &str,
) -> Vec<u8> {
    format!("avalon:{claim_kind}.issued:v1:{issuer_ref}:{subject}:{achievement}").into_bytes()
}

async fn seed_identity_session(pool: &sqlx::PgPool) -> (Uuid, String) {
    let identity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(identity_id)
        .execute(pool)
        .await
        .expect("failed to seed identity");
    sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(format!("read-test-{identity_id}"))
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

/// #697/#698: `POST /integrations/{slug}/connect` is signature-required
/// -- seeds a real signing key for `identity_id` so a connect call can
/// produce a genuine fresh signature over HTTP.
async fn seed_signing_key(pool: &sqlx::PgPool, identity_id: Uuid) -> (Uuid, SigningKey) {
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

#[tokio::test]
#[ignore]
async fn scenario_d_an_authentic_valid_claim_from_an_untrusted_issuer_is_not_recognized() {
    let http = reqwest::Client::new();
    let base = server_url();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?");
    let (identity_id, token) = seed_identity_session(&pool).await;
    let (signing_key_id, signing_key) = seed_signing_key(&pool, identity_id).await;

    let integrator_c = register_integrator(&http, &base).await;

    let headers = auth_headers(&http, &base, &integrator_c).await;
    let define = http
        .post(format!(
            "{base}/integrations/{}/achievements",
            integrator_c.slug
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
    let achievement_id = define.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let connect_capabilities = ["achievements.issue"];
    http.post(format!("{base}/integrations/{}/connect", integrator_c.slug))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "capabilities": connect_capabilities,
            "signing_key_id": signing_key_id,
            "signature": sign_connect(&signing_key, &integrator_c.slug, &connect_capabilities),
        }))
        .send()
        .await
        .unwrap();

    let issuer_ref = format!("game:{}", integrator_c.slug);
    let signing_bytes =
        attestation_signing_bytes("achievement", &issuer_ref, identity_id, &achievement_id);
    let signature = integrator_c.signing_key.sign(&signing_bytes);

    let mut headers = auth_headers(&http, &base, &integrator_c).await;
    headers.insert(
        "x-avalon-identity-id",
        identity_id.to_string().parse().unwrap(),
    );
    let issue = http
        .post(format!(
            "{base}/integrations/{}/achievements/dragon_slayer/issue",
            integrator_c.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({
            "key_id": integrator_c.key_id,
            "signature": BASE64.encode(signature.to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert!(issue.status().is_success(), "{:?}", issue.status());
    let attestation_id = issue.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Integrator B (a reader who never issued this and has no trust relationship
    // with Integrator C) reads it back.
    let read = http
        .get(format!("{base}/attestations/{attestation_id}"))
        .send()
        .await
        .unwrap();
    assert!(read.status().is_success(), "{:?}", read.status());
    let body: serde_json::Value = read.json().await.unwrap();

    assert_eq!(body["authenticity"]["status"], "authentic");
    assert_eq!(body["validity"]["status"], "valid");
    // No recognition field at all — it's not this endpoint's question to
    // answer.
    assert!(body.get("recognition").is_none());

    // Integrator B's own policy (empty — it doesn't trust Integrator C at all)
    // evaluated purely client-side, no server round trip.
    let integrator_b_policy = TrustRelationship {
        truster: IntegratorId(Uuid::new_v4()),
        trusted_issuer: Issuer::Game(IntegratorId(Uuid::new_v4())), // some other issuer entirely
        established_at: OffsetDateTime::UNIX_EPOCH,
        scopes: vec![],
    };
    let claimed_issuer = Issuer::Game(IntegratorId(Uuid::new_v4())); // Integrator C's id, distinct from the policy's trusted_issuer
    let recognition = recognize(
        &integrator_b_policy,
        &claimed_issuer,
        "achievement",
        None,
        1,
        OffsetDateTime::now_utc(),
    );
    assert!(matches!(recognition, Recognition::NotRecognized { .. }));
}
