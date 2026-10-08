//! Session lifecycle against a real, running `avalon-server` and Postgres: tokens are stored
//! hashed, and end on logout, revocation of the credential that produced them, recovery, and
//! expiry. Gated `--ignored` since it needs live infra.

mod chain_sign;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::{CredentialCreationOptions, CredentialRequestOptions};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

type Identity = avalon_protocol::ids::IdentityId;

fn network_id() -> String {
    std::env::var("AVALON_NETWORK_ID").unwrap_or_else(|_| "avalon-dev-local".to_string())
}

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn rp_origin() -> url::Url {
    let origin = std::env::var("AVALON_WEBAUTHN_ORIGIN")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    url::Url::parse(&origin).expect("AVALON_WEBAUTHN_ORIGIN must be a valid URL")
}

async fn test_pool() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres")
}

type VirtualClient = Client<
    MemoryStore,
    MockUserValidationMethod,
    passkey_crypto::AvailableBackend,
    public_suffix::PublicSuffixList,
    (),
>;

fn new_virtual_client(ceremonies: usize) -> VirtualClient {
    let authenticator = Authenticator::new(
        Aaguid::new_empty(),
        MemoryStore::new(),
        MockUserValidationMethod::verified_user(ceremonies),
        passkey_crypto::AvailableBackend,
    );
    Client::new(authenticator).allows_insecure_localhost(true)
}

async fn seed_identity(pool: &PgPool) -> Identity {
    let who = avalon_protocol::identity_id::TestIdentity::new();
    sqlx::query("INSERT INTO identities (id, inception_public_key) VALUES ($1, $2)")
        .bind(who.id)
        .bind(who.public_key().to_vec())
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
        .bind(who.id)
        .bind(format!("session-revocation-{}", who.id))
        .execute(pool)
        .await
        .unwrap();
    who.id
}

struct Origin2 {
    passkey_id: Option<Uuid>,
    signing_key_id: Option<Uuid>,
    expires_in: time::Duration,
}

async fn seed_session(pool: &PgPool, identity_id: Identity, origin: Origin2) -> String {
    let token = format!("session-revocation-{}", Uuid::new_v4());
    sqlx::query(
        "INSERT INTO sessions (token_hash, identity_id, expires_at, origin_passkey_id, origin_signing_key_id) \
         VALUES (sha256(convert_to($1::text, 'UTF8')), $2, $3, $4, $5)",
    )
    .bind(&token)
    .bind(identity_id)
    .bind(OffsetDateTime::now_utc() + origin.expires_in)
    .bind(origin.passkey_id)
    .bind(origin.signing_key_id)
    .execute(pool)
    .await
    .unwrap();
    token
}

async fn plain_session(pool: &PgPool, identity_id: Identity) -> String {
    seed_session(
        pool,
        identity_id,
        Origin2 {
            passkey_id: None,
            signing_key_id: None,
            expires_in: time::Duration::hours(1),
        },
    )
    .await
}

async fn seed_signing_key(pool: &PgPool, identity_id: Identity) -> (Uuid, SigningKey) {
    let signing_key = SigningKey::generate(&mut rand::rng());
    let row = sqlx::query(
        "INSERT INTO identity_signing_keys (identity_id, public_key) VALUES ($1, $2) RETURNING id",
    )
    .bind(identity_id)
    .bind(signing_key.verifying_key().to_bytes().as_slice())
    .fetch_one(pool)
    .await
    .unwrap();
    let key_id: Uuid = row.try_get("id").unwrap();
    sqlx::query(
        "INSERT INTO indexer_identity_signing_keys (signing_key_id, identity_id, public_key, added_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(key_id)
    .bind(identity_id)
    .bind(signing_key.verifying_key().to_bytes().as_slice())
    .execute(pool)
    .await
    .unwrap();
    (key_id, signing_key)
}

fn sign_action(signing_key: &SigningKey, action_tag: &str, fields: &[&str]) -> String {
    let mut message = format!("avalon:{action_tag}:v1");
    for field in fields {
        message.push(':');
        message.push_str(field);
    }
    BASE64.encode(signing_key.sign(message.as_bytes()).to_bytes())
}

async fn me_status(http: &reqwest::Client, token: &str) -> u16 {
    http.get(format!("{}/me", server_url()))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

async fn session_count(pool: &PgPool, identity_id: Identity) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE identity_id = $1")
        .bind(identity_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn create_identity_with_one_passkey(
    pool: &PgPool,
    http: &reqwest::Client,
    base: &str,
    ceremonies: usize,
) -> (Identity, VirtualClient) {
    let signing_key = SigningKey::generate(&mut rand::rng());
    let identity_id =
        avalon_protocol::identity_id::derive_identity_id_for_key(&signing_key.verifying_key());
    let display_name = format!("session-revocation-{identity_id}");
    let start: serde_json::Value = http
        .post(format!("{base}/identities/register/start"))
        .json(&serde_json::json!({
            "identity_id": identity_id,
            "event_signing_public_key": BASE64.encode(signing_key.verifying_key().to_bytes()),
            "display_name": display_name,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ticket_id = start["ticket_id"].as_str().unwrap().to_string();
    let mut client = new_virtual_client(ceremonies);
    let creation_options: CredentialCreationOptions =
        serde_json::from_value(start["challenge"].clone()).unwrap();
    let credential = client
        .register(
            Origin::from(&rp_origin()),
            creation_options,
            DefaultClientData,
        )
        .await
        .unwrap();
    let signing_bytes = avalon_protocol::identity_id::identity_created_signing_bytes(
        start["network_id"].as_str().unwrap(),
        ticket_id.parse().unwrap(),
        &identity_id,
        &signing_key.verifying_key().to_bytes(),
        &display_name,
    );
    http.post(format!("{base}/identities/register/finish"))
        .json(&serde_json::json!({
            "ticket_id": ticket_id,
            "webauthn_credential": credential,
            "event_signature": BASE64.encode(signing_key.sign(&signing_bytes).to_bytes()),
            "device_label": null,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    chain_sign::remember_registered(pool, identity_id, &signing_key).await;
    (identity_id, client)
}

async fn login(
    http: &reqwest::Client,
    base: &str,
    identity_id: Identity,
    client: &mut VirtualClient,
) -> String {
    let start: serde_json::Value = http
        .post(format!("{base}/sessions/start"))
        .json(&serde_json::json!({ "identity_id": identity_id }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let request_options: CredentialRequestOptions =
        serde_json::from_value(start["challenge"].clone()).unwrap();
    let assertion = client
        .authenticate(
            Origin::from(&rp_origin()),
            request_options,
            DefaultClientData,
        )
        .await
        .unwrap();
    let finish: serde_json::Value = http
        .post(format!("{base}/sessions/finish"))
        .json(&serde_json::json!({
            "ticket_id": start["ticket_id"],
            "credential": assertion,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    finish["token"].as_str().unwrap().to_string()
}

#[tokio::test]
#[ignore]
async fn tokens_are_stored_hashed_and_logout_ends_only_the_presented_session() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (identity_id, mut client) = create_identity_with_one_passkey(&pool, &http, &base, 3).await;
    let first = login(&http, &base, identity_id, &mut client).await;
    let second = login(&http, &base, identity_id, &mut client).await;

    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name::text FROM information_schema.columns WHERE table_name = 'sessions'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(!columns.iter().any(|c| c == "token"));
    let stored: Vec<u8> = sqlx::query_scalar(
        "SELECT token_hash FROM sessions WHERE token_hash = sha256(convert_to($1::text, 'UTF8'))",
    )
    .bind(&first)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        stored.as_slice(),
        avalon_server::sessions::hash_token(&first)
    );
    assert_ne!(stored.as_slice(), first.as_bytes());

    let logout = http
        .post(format!("{base}/sessions/logout"))
        .bearer_auth(&first)
        .send()
        .await
        .unwrap();
    assert!(logout.status().is_success());
    assert_eq!(me_status(&http, &first).await, 401);
    assert_eq!(me_status(&http, &second).await, 200);
    assert_eq!(session_count(&pool, identity_id).await, 1);

    // A second logout with the dead token is refused, not silently accepted.
    let again = http
        .post(format!("{base}/sessions/logout"))
        .bearer_auth(&first)
        .send()
        .await
        .unwrap();
    assert_eq!(again.status().as_u16(), 401);
}

#[tokio::test]
#[ignore]
async fn sessions_can_be_listed_and_revoked_one_at_a_time_but_only_by_their_owner() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let owner = seed_identity(&pool).await;
    let stranger = seed_identity(&pool).await;
    let mine = plain_session(&pool, owner).await;
    let other = plain_session(&pool, owner).await;
    let strangers = plain_session(&pool, stranger).await;

    let listed: serde_json::Value = http
        .get(format!("{base}/me/sessions"))
        .bearer_auth(&mine)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sessions = listed["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions.iter().filter(|s| s["current"] == true).count(), 1);
    let other_id = sessions.iter().find(|s| s["current"] == false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Another identity cannot end this session, and cannot learn that it exists.
    let stranger_attempt = http
        .post(format!("{base}/me/sessions/{other_id}/revoke"))
        .bearer_auth(&strangers)
        .send()
        .await
        .unwrap();
    assert_eq!(stranger_attempt.status().as_u16(), 404);
    assert_eq!(me_status(&http, &other).await, 200);

    let revoked = http
        .post(format!("{base}/me/sessions/{other_id}/revoke"))
        .bearer_auth(&mine)
        .send()
        .await
        .unwrap();
    assert!(revoked.status().is_success());
    assert_eq!(me_status(&http, &other).await, 401);
    assert_eq!(me_status(&http, &mine).await, 200);
}

#[tokio::test]
#[ignore]
async fn revoking_a_passkey_ends_the_sessions_it_created_and_no_others() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (identity_id, mut client_a) =
        create_identity_with_one_passkey(&pool, &http, &base, 3).await;
    let token_a = login(&http, &base, identity_id, &mut client_a).await;
    let token_a_again = login(&http, &base, identity_id, &mut client_a).await;

    let start: serde_json::Value = http
        .post(format!("{base}/me/passkeys/register/start"))
        .bearer_auth(&token_a)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut client_b = new_virtual_client(2);
    let creation_options: CredentialCreationOptions =
        serde_json::from_value(start["challenge"].clone()).unwrap();
    let credential = client_b
        .register(
            Origin::from(&rp_origin()),
            creation_options,
            DefaultClientData,
        )
        .await
        .unwrap();
    http.post(format!("{base}/me/passkeys/register/finish"))
        .bearer_auth(&token_a)
        .json(&serde_json::json!({
            "ticket_id": start["ticket_id"],
            "webauthn_credential": credential,
            "label": "second",
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let token_b = login(&http, &base, identity_id, &mut client_b).await;
    let pairing_session = plain_session(&pool, identity_id).await;

    let passkeys: serde_json::Value = http
        .get(format!("{base}/me/passkeys"))
        .bearer_auth(&token_b)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows = sqlx::query("SELECT id, label FROM identity_keys WHERE identity_id = $1")
        .bind(identity_id)
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(passkeys.as_array().unwrap().len(), 2);
    let first_passkey: Uuid = rows
        .iter()
        .find(|r| r.try_get::<Option<String>, _>("label").unwrap().is_none())
        .unwrap()
        .try_get("id")
        .unwrap();
    let recorded: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sessions WHERE identity_id = $1 AND origin_passkey_id = $2",
    )
    .bind(identity_id)
    .bind(first_passkey)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(recorded, 2);

    let revoke = http
        .post(format!("{base}/me/passkeys/{first_passkey}/revoke"))
        .bearer_auth(&token_b)
        .json(&serde_json::json!({
            "chain_event": chain_sign::passkey_revoked(identity_id, first_passkey).await,
        }))
        .send()
        .await
        .unwrap();
    assert!(revoke.status().is_success());

    assert_eq!(me_status(&http, &token_a).await, 401);
    assert_eq!(me_status(&http, &token_a_again).await, 401);
    assert_eq!(me_status(&http, &token_b).await, 200);
    assert_eq!(me_status(&http, &pairing_session).await, 200);
    assert_eq!(session_count(&pool, identity_id).await, 2);
}

#[tokio::test]
#[ignore]
async fn revoking_a_signing_key_ends_the_sessions_it_approved_and_no_others() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let identity_id = seed_identity(&pool).await;
    let (keep_key, keep_signer) = seed_signing_key(&pool, identity_id).await;
    let (stolen_key, _) = seed_signing_key(&pool, identity_id).await;
    let stolen_session = seed_session(
        &pool,
        identity_id,
        Origin2 {
            passkey_id: None,
            signing_key_id: Some(stolen_key),
            expires_in: time::Duration::hours(1),
        },
    )
    .await;
    let kept_session = seed_session(
        &pool,
        identity_id,
        Origin2 {
            passkey_id: None,
            signing_key_id: Some(keep_key),
            expires_in: time::Duration::hours(1),
        },
    )
    .await;

    let bytes = avalon_protocol::identity_id::signing_key_revoked_signing_bytes(
        &network_id(),
        &identity_id,
        stolen_key,
        keep_key,
        1,
        None,
    );
    let revoke = http
        .post(format!("{base}/me/devices/{stolen_key}/revoke"))
        .bearer_auth(&kept_session)
        .json(&serde_json::json!({
            "revoked_by_signing_key_id": keep_key,
            "seq": 1,
            "prev_hash": null,
            "signature": BASE64.encode(keep_signer.sign(&bytes).to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert!(revoke.status().is_success(), "{:?}", revoke.status());

    assert_eq!(me_status(&http, &stolen_session).await, 401);
    assert_eq!(me_status(&http, &kept_session).await, 200);
    assert_eq!(session_count(&pool, identity_id).await, 1);
}

#[tokio::test]
#[ignore]
async fn a_credential_revoked_on_another_node_ends_its_sessions_here() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let identity_id = seed_identity(&pool).await;
    let revoked_key = Uuid::new_v4();
    let revoked_passkey = Uuid::new_v4();
    let by_key = seed_session(
        &pool,
        identity_id,
        Origin2 {
            passkey_id: None,
            signing_key_id: Some(revoked_key),
            expires_in: time::Duration::hours(1),
        },
    )
    .await;
    let by_passkey = seed_session(
        &pool,
        identity_id,
        Origin2 {
            passkey_id: Some(revoked_passkey),
            signing_key_id: None,
            expires_in: time::Duration::hours(1),
        },
    )
    .await;
    let unrelated = seed_session(
        &pool,
        identity_id,
        Origin2 {
            passkey_id: Some(Uuid::new_v4()),
            signing_key_id: Some(Uuid::new_v4()),
            expires_in: time::Duration::hours(1),
        },
    )
    .await;
    assert_eq!(me_status(&http, &by_key).await, 200);
    assert_eq!(me_status(&http, &by_passkey).await, 200);

    sqlx::query(
        "INSERT INTO indexer_identity_signing_key_revocations (identity_id, signing_key_id, revoked_at) VALUES ($1, $2, now())",
    )
    .bind(identity_id)
    .bind(revoked_key)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO indexer_identity_passkey_revocations (identity_id, passkey_id, revoked_at) VALUES ($1, $2, now())",
    )
    .bind(identity_id)
    .bind(revoked_passkey)
    .execute(&pool)
    .await
    .unwrap();

    assert_eq!(me_status(&http, &by_key).await, 401);
    assert_eq!(me_status(&http, &by_passkey).await, 401);
    assert_eq!(me_status(&http, &unrelated).await, 200);
}

#[tokio::test]
#[ignore]
async fn an_expired_session_is_refused_and_pruning_is_bounded() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let identity_id = seed_identity(&pool).await;
    let expired = seed_session(
        &pool,
        identity_id,
        Origin2 {
            passkey_id: None,
            signing_key_id: None,
            expires_in: time::Duration::hours(-1),
        },
    )
    .await;
    let live = plain_session(&pool, identity_id).await;
    assert_eq!(me_status(&http, &expired).await, 401);
    assert_eq!(me_status(&http, &live).await, 200);

    sqlx::query(
        "INSERT INTO sessions (token_hash, identity_id, expires_at) \
         SELECT sha256(convert_to(gen_random_uuid()::text, 'UTF8')), $1, now() - interval '1 hour' \
         FROM generate_series(1, 1100)",
    )
    .bind(identity_id)
    .execute(&pool)
    .await
    .unwrap();

    let first = avalon_server::sessions::prune_expired(&pool).await.unwrap();
    assert!(
        first <= 1000,
        "one pass must stay within its batch: {first}"
    );
    while avalon_server::sessions::prune_expired(&pool).await.unwrap() > 0 {}
    assert_eq!(session_count(&pool, identity_id).await, 1);
    assert_eq!(me_status(&http, &live).await, 200);
}

#[tokio::test]
#[ignore]
async fn pairing_stores_no_usable_secret_and_records_the_approving_key() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let identity_id = seed_identity(&pool).await;
    let approver = plain_session(&pool, identity_id).await;
    let (key_id, signer) = seed_signing_key(&pool, identity_id).await;

    let start: serde_json::Value = http
        .post(format!("{base}/auth/device/start"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let device_code = start["device_code"].as_str().unwrap().to_string();
    let user_code = start["user_code"].as_str().unwrap().to_string();
    let stored_matches_plaintext: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM device_pairings WHERE device_code_hash = $1")
            .bind(&device_code)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored_matches_plaintext, 0);

    let signature = sign_action(
        &signer,
        "device_pairing.approve",
        &[&identity_id.to_string(), &user_code],
    );
    http.post(format!("{base}/auth/device/approve"))
        .bearer_auth(&approver)
        .json(&serde_json::json!({
            "user_code": user_code,
            "signing_key_id": key_id,
            "signature": signature,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    // Approval alone mints nothing: no token exists until the waiting device collects it.
    assert_eq!(session_count(&pool, identity_id).await, 1);

    let polled: serde_json::Value = http
        .post(format!("{base}/auth/device/poll"))
        .bearer_auth(&device_code)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = polled["token"].as_str().unwrap();
    assert_eq!(me_status(&http, token).await, 200);
    let origin: Option<Uuid> = sqlx::query_scalar(
        "SELECT origin_signing_key_id FROM sessions WHERE token_hash = sha256(convert_to($1::text, 'UTF8'))",
    )
    .bind(token)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(origin, Some(key_id));
}

#[tokio::test]
#[ignore]
async fn an_approved_pairing_cannot_be_collected_after_it_expires() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let identity_id = seed_identity(&pool).await;
    let approver = plain_session(&pool, identity_id).await;
    let (key_id, signer) = seed_signing_key(&pool, identity_id).await;

    let start: serde_json::Value = http
        .post(format!("{base}/auth/device/start"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let device_code = start["device_code"].as_str().unwrap().to_string();
    let user_code = start["user_code"].as_str().unwrap().to_string();
    let signature = sign_action(
        &signer,
        "device_pairing.approve",
        &[&identity_id.to_string(), &user_code],
    );
    http.post(format!("{base}/auth/device/approve"))
        .bearer_auth(&approver)
        .json(&serde_json::json!({
            "user_code": user_code,
            "signing_key_id": key_id,
            "signature": signature,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    sqlx::query(
        "UPDATE device_pairings SET expires_at = now() - interval '1 minute' WHERE user_code = $1",
    )
    .bind(&user_code)
    .execute(&pool)
    .await
    .unwrap();

    let polled: serde_json::Value = http
        .post(format!("{base}/auth/device/poll"))
        .bearer_auth(&device_code)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(polled["status"], "expired");
    assert!(polled["token"].is_null());
    assert_eq!(session_count(&pool, identity_id).await, 1);
}

async fn start_and_approve_pairing(
    http: &reqwest::Client,
    base: &str,
    pool: &PgPool,
    identity_id: Identity,
    approver_session: &str,
    key_id: Uuid,
    signer: &SigningKey,
) -> String {
    let _ = pool;
    let start: serde_json::Value = http
        .post(format!("{base}/auth/device/start"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let user_code = start["user_code"].as_str().unwrap().to_string();
    let signature = sign_action(
        signer,
        "device_pairing.approve",
        &[&identity_id.to_string(), &user_code],
    );
    http.post(format!("{base}/auth/device/approve"))
        .bearer_auth(approver_session)
        .json(&serde_json::json!({
            "user_code": user_code,
            "signing_key_id": key_id,
            "signature": signature,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    start["device_code"].as_str().unwrap().to_string()
}

async fn poll_pairing(http: &reqwest::Client, base: &str, device_code: &str) -> serde_json::Value {
    http.post(format!("{base}/auth/device/poll"))
        .bearer_auth(device_code)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
#[ignore]
async fn a_pairing_approved_by_a_key_revoked_before_the_poll_mints_nothing() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let identity_id = seed_identity(&pool).await;
    let approver = plain_session(&pool, identity_id).await;
    let (stolen_key, stolen_signer) = seed_signing_key(&pool, identity_id).await;
    let (keep_key, keep_signer) = seed_signing_key(&pool, identity_id).await;
    let device_code = start_and_approve_pairing(
        &http,
        &base,
        &pool,
        identity_id,
        &approver,
        stolen_key,
        &stolen_signer,
    )
    .await;

    let bytes = avalon_protocol::identity_id::signing_key_revoked_signing_bytes(
        &network_id(),
        &identity_id,
        stolen_key,
        keep_key,
        1,
        None,
    );
    http.post(format!("{base}/me/devices/{stolen_key}/revoke"))
        .bearer_auth(&approver)
        .json(&serde_json::json!({
            "revoked_by_signing_key_id": keep_key,
            "seq": 1,
            "prev_hash": null,
            "signature": BASE64.encode(keep_signer.sign(&bytes).to_bytes()),
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let polled = poll_pairing(&http, &base, &device_code).await;
    assert!(polled["token"].is_null(), "{polled}");
    assert_eq!(pairing_status(&pool, &device_code).await, "expired");
    assert_eq!(session_count(&pool, identity_id).await, 1);
}

#[tokio::test]
#[ignore]
async fn a_poll_waits_for_an_in_flight_revocation_of_the_approving_key() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let identity_id = seed_identity(&pool).await;
    let approver = plain_session(&pool, identity_id).await;
    let (key, signer) = seed_signing_key(&pool, identity_id).await;
    let device_code =
        start_and_approve_pairing(&http, &base, &pool, identity_id, &approver, key, &signer).await;

    // Hold the key row the way a revocation does, start the poll, then revoke and commit.
    let mut revoking = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM identity_signing_keys WHERE id = $1 FOR UPDATE")
        .bind(key)
        .fetch_one(&mut *revoking)
        .await
        .unwrap();
    let poller = {
        let (http, base, code) = (http.clone(), base.clone(), device_code.clone());
        tokio::spawn(async move { poll_pairing(&http, &base, &code).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    sqlx::query("UPDATE identity_signing_keys SET revoked_at = now() WHERE id = $1")
        .bind(key)
        .execute(&mut *revoking)
        .await
        .unwrap();
    revoking.commit().await.unwrap();

    let polled = poller.await.unwrap();
    assert!(polled["token"].is_null(), "{polled}");
    assert_eq!(pairing_status(&pool, &device_code).await, "expired");
    assert_eq!(session_count(&pool, identity_id).await, 1);
}

#[tokio::test]
#[ignore]
async fn two_concurrent_polls_of_one_approved_pairing_yield_exactly_one_token() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let identity_id = seed_identity(&pool).await;
    let approver = plain_session(&pool, identity_id).await;
    let (key, signer) = seed_signing_key(&pool, identity_id).await;
    let device_code =
        start_and_approve_pairing(&http, &base, &pool, identity_id, &approver, key, &signer).await;

    let (a, b) = tokio::join!(
        poll_pairing(&http, &base, &device_code),
        poll_pairing(&http, &base, &device_code)
    );
    let tokens = [a, b].iter().filter(|r| r["token"].is_string()).count();
    assert_eq!(tokens, 1);
    assert_eq!(session_count(&pool, identity_id).await, 2);
}

#[tokio::test]
#[ignore]
async fn a_login_whose_passkey_is_revoked_mid_ceremony_mints_nothing() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (identity_id, mut client) = create_identity_with_one_passkey(&pool, &http, &base, 2).await;
    let passkey: Uuid = sqlx::query_scalar("SELECT id FROM identity_keys WHERE identity_id = $1")
        .bind(identity_id)
        .fetch_one(&pool)
        .await
        .unwrap();

    let start: serde_json::Value = http
        .post(format!("{base}/sessions/start"))
        .json(&serde_json::json!({ "identity_id": identity_id }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let request_options: CredentialRequestOptions =
        serde_json::from_value(start["challenge"].clone()).unwrap();
    let assertion = client
        .authenticate(
            Origin::from(&rp_origin()),
            request_options,
            DefaultClientData,
        )
        .await
        .unwrap();

    let mut revoking = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM identity_keys WHERE id = $1 FOR UPDATE")
        .bind(passkey)
        .fetch_one(&mut *revoking)
        .await
        .unwrap();
    let finisher = {
        let (http, base) = (http.clone(), server_url());
        let body = serde_json::json!({ "ticket_id": start["ticket_id"], "credential": assertion });
        tokio::spawn(async move {
            http.post(format!("{base}/sessions/finish"))
                .json(&body)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    sqlx::query("DELETE FROM identity_keys WHERE id = $1")
        .bind(passkey)
        .execute(&mut *revoking)
        .await
        .unwrap();
    revoking.commit().await.unwrap();

    assert!(finisher.await.unwrap() >= 400);
    assert_eq!(session_count(&pool, identity_id).await, 0);
}

#[tokio::test]
#[ignore]
async fn no_session_is_minted_without_an_origin_credential() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let identity_id = seed_identity(&pool).await;

    let direct = avalon_server::sessions::mint(
        &pool,
        identity_id,
        avalon_server::sessions::SessionOrigin::default(),
    )
    .await;
    assert!(direct.is_err());

    // An approved pairing that somehow lacks its approving key fails closed at the poll.
    let device_code = format!("orphan-{}", Uuid::new_v4());
    sqlx::query(
        "INSERT INTO device_pairings (device_code_hash, user_code, status, identity_id, expires_at) \
         VALUES (encode(sha256(convert_to($1::text, 'UTF8')), 'hex'), $2, 'approved', $3, now() + interval '1 hour')",
    )
    .bind(&device_code)
    .bind(format!("O{}", &Uuid::new_v4().simple().to_string()[..7]))
    .bind(identity_id)
    .execute(&pool)
    .await
    .unwrap();
    let polled = poll_pairing(&http, &base, &device_code).await;
    assert!(polled["token"].is_null(), "{polled}");
    assert_eq!(pairing_status(&pool, &device_code).await, "expired");
    assert_eq!(session_count(&pool, identity_id).await, 0);
}

async fn pairing_status(pool: &PgPool, device_code: &str) -> String {
    sqlx::query_scalar(
        "SELECT status FROM device_pairings WHERE device_code_hash = encode(sha256(convert_to($1::text, 'UTF8')), 'hex')",
    )
    .bind(device_code)
    .fetch_one(pool)
    .await
    .unwrap()
}
